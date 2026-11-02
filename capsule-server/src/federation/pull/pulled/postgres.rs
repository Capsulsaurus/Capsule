//! [`PostgresPulledAlbums`]: the durable [`PulledAlbums`] over `federation_pulled_albums` and
//! its child tables, with the metadata blobs in the [`BlobStore`].
//!
//! # A commit is one transaction, decided under the job's row lock
//!
//! [`PulledAlbums::commit`] opens a transaction, makes sure the job's `federation_pulled_albums`
//! row exists, and takes it `FOR UPDATE`. The stored high-water mark is compared with `from`
//! under that lock, so two replicas whose leases overlapped cannot both write from one position:
//! the second waits for the first, then reads the mark the first moved and is refused. A refusal
//! rolls back, so not even the row it created is left behind.
//!
//! Inside the lock the commit writes, in order: the metadata blobs' bytes to the blob store, one
//! row per blob the job now holds, the charge for the blobs it holds for the first time
//! ([`PulledBlobCharge`]), the asset heads that changed, the entries with their blobs and
//! derivative manifests, and the new position. Then it commits.
//!
//! # The blobs land before the rows that name them
//!
//! The blob store is not in the transaction, so the order is what keeps the two consistent. A
//! blob is durable at its content address before any row names it, so a crash or a rollback
//! after the put leaves an unreferenced blob — the orphan the collector reclaims — and never a
//! row naming bytes that are not there. A blob already present is not written again
//! ([`Placement::AlreadyPresent`]): its address is its content. The port hands over bytes the
//! puller already checked against their address, and the blob store's contract is that its
//! caller has; neither re-hashes them.
//!
//! # Charging is the caller's policy, inside the commit
//!
//! The per-`(receiving_user, source_peer)` cap on pulled storage (design/quota.md) is decided by
//! whoever assembles the adapter. [`PulledBlobCharge::charge`] is handed the commit's own
//! transaction and only the blobs the job did not hold before, so a refusal rolls the whole page
//! back and a blob is charged to a job once. [`PeerBudgetCharge`] is the one `serve` assembles:
//! it enforces a [`PeerCachingBudget`] as the [port's docs](super) count it. [`Uncharged`], the
//! default of [`PostgresPulledAlbums::new`], charges nothing.
//!
//! # The budget is read under the job's lock
//!
//! [`PeerBudgetCharge`] reads each subscriber's usage in one statement inside the commit's
//! transaction, after the job's new blob rows are inserted, so the rows it sums already include
//! the page and are never added a second time. Two jobs on one peer sharing a subscriber are two
//! row locks, so their commits may each read the other's usage before it commits: a subscriber
//! can pass its budget by at most what its other albums on that peer commit concurrently, one
//! page each. Taking a lock per subscriber would close that and serialise every album it holds
//! on the peer; a budget sized in gigabytes does not need it.
//!
//! # The heads are written as a difference
//!
//! A page's [`PulledAlbum`] is the album's whole state, but a page moves a handful of assets.
//! The commit reads the held heads under the lock, writes the ones that differ, and deletes the
//! ones the state no longer names, so a page costs what it changed rather than the album's size.
//!
//! # Encodings
//!
//! Heads are `BYTEA` of 32 bytes; an asset's superseded heads are their concatenation, oldest
//! first. Blob addresses are lowercase hex `TEXT`, the blob store's own spelling and the asset
//! index's. Every unsigned count is a `BIGINT`, refused on the way in past `i64::MAX` rather than
//! wrapped.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use capsule_core::crypto::hash::Hash32;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, QueryResult, Statement,
    TransactionTrait, Value,
};

use super::conformance::Inspect;
use super::{
    AcceptedEntry, AssetHead, Change, Committed, OverBudget, PeerCachingBudget, PulledAlbum,
    PulledAlbums, PulledBlob, ValidatedPage,
};
use crate::blob::{BlobError, BlobStore, ContentAddress, Placement};
use crate::federation::pull::JobKey;
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};
use crate::store::{StoreError, StoreFuture};

/// Which port is speaking, for every error the adapter raises.
const PORT: Port = Port {
    store: "pulled albums",
    record: "PulledAlbum",
};

/// The most bind parameters one statement carries, well inside Postgres's 65,535.
const MAX_PARAMETERS: usize = 30_000;

/// A blob a commit holds for its job for the first time: what [`PulledBlobCharge`] charges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldBlob {
    /// Its content address.
    pub hash: Hash32,
    /// Its size in bytes.
    pub size: u64,
}

/// What a commit's newly held metadata blobs cost, decided inside the commit's transaction.
///
/// The hook for the per-`(receiving_user, source_peer)` storage cap on pulled albums. It is
/// called once per commit that holds at least one blob its job did not hold before, inside the
/// commit's transaction and after the conditional check passed. By then the blob bytes are
/// stored and the job's new `federation_pulled_metadata_blobs` rows are inserted in that same
/// transaction, so usage read back from that table already includes `held`: an implementation
/// must not count those rows again. The asset heads, entries and position are not yet written.
/// A refusal ([`OverBudget`]) rolls the whole commit back and is the commit's
/// [`Committed::OverBudget`]; an error rolls it back and is the commit's error. Anything it
/// writes through `transaction` lands or rolls back with the page.
pub trait PulledBlobCharge: fmt::Debug + Send + Sync {
    /// Charge `held`, the blobs `job` holds for the first time: `None` admits them.
    fn charge<'a>(
        &'a self,
        transaction: &'a DatabaseTransaction,
        job: &'a JobKey,
        held: &'a [HeldBlob],
    ) -> StoreFuture<'a, Option<OverBudget>>;
}

/// The [`PulledBlobCharge`] that charges nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct Uncharged;

impl PulledBlobCharge for Uncharged {
    fn charge<'a>(
        &'a self,
        _transaction: &'a DatabaseTransaction,
        job: &'a JobKey,
        held: &'a [HeldBlob],
    ) -> StoreFuture<'a, Option<OverBudget>> {
        Box::pin(async move {
            tracing::trace!(%job, blobs = held.len(), "pulled metadata blobs held uncharged");
            Ok(None)
        })
    }
}

/// Every subscriber of the job `$1`/`$2` with its usage on the peer `$1` and the part of it
/// that the addresses `$3…` add, which are the ones the job holds for the first time.
///
/// A subscriber's usage is the distinct blobs held by every job it subscribes to on the peer;
/// what the commit adds to it is the held addresses no other of those jobs holds. Both read the
/// rows this transaction inserted, so `held` is counted exactly once.
const SUBSCRIBER_USAGE: &str = "\
    SELECT s.subscriber_id, \
           COALESCE(SUM(u.size), 0)::BIGINT AS used, \
           COALESCE(SUM(u.size) FILTER (WHERE u.added), 0)::BIGINT AS added \
    FROM federation_subscriptions s \
    LEFT JOIN LATERAL ( \
        SELECT b.address, MAX(b.size) AS size, \
               bool_and(b.album_id = $2) AND b.address IN ({held}) AS added \
        FROM federation_pulled_metadata_blobs b \
        JOIN federation_subscriptions mine \
          ON mine.peer_id = b.peer_id AND mine.album_id = b.album_id \
        WHERE mine.subscriber_id = s.subscriber_id AND b.peer_id = $1 \
        GROUP BY b.address \
    ) u ON TRUE \
    WHERE s.peer_id = $1 AND s.album_id = $2 \
    GROUP BY s.subscriber_id \
    ORDER BY s.subscriber_id COLLATE \"C\"";

/// The [`PulledBlobCharge`] that enforces a [`PeerCachingBudget`] on every subscriber of the
/// job, live or lapsed.
#[derive(Debug, Clone, Copy)]
pub struct PeerBudgetCharge {
    budget: PeerCachingBudget,
}

impl PeerBudgetCharge {
    /// A charge refusing growth past `budget`.
    pub fn new(budget: PeerCachingBudget) -> Self {
        Self { budget }
    }
}

impl PulledBlobCharge for PeerBudgetCharge {
    fn charge<'a>(
        &'a self,
        transaction: &'a DatabaseTransaction,
        job: &'a JobKey,
        held: &'a [HeldBlob],
    ) -> StoreFuture<'a, Option<OverBudget>> {
        Box::pin(async move {
            let doing = "reading the subscribers' caching budgets";
            let failed = PORT.failing(doing);
            let mut refusal = None;
            let mut subscribers = 0_usize;
            // A page holds at most a few hundred new blobs; chunked anyway, so the bind bound
            // holds whatever a caller passes. A subscriber past its budget on any chunk is past
            // it, since every chunk reads the same `used`.
            for chunk in held.chunks(MAX_PARAMETERS) {
                let placeholders: Vec<String> = (0..chunk.len())
                    .map(|index| format!("${}", index + 3))
                    .collect();
                let mut values = job_values(job);
                for blob in chunk {
                    values.push(Value::from(address_of(&blob.hash)?.as_str().to_owned()));
                }
                let rows = transaction
                    .query_all_raw(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        SUBSCRIBER_USAGE.replace("{held}", &placeholders.join(", ")),
                        values,
                    ))
                    .await
                    .map_err(&failed)?;
                subscribers = rows.len();
                for row in rows {
                    let subscriber: String = row.try_get("", "subscriber_id").map_err(&failed)?;
                    let used: i64 = row.try_get("", "used").map_err(&failed)?;
                    let added: i64 = row.try_get("", "added").map_err(&failed)?;
                    let subscriber = crate::store::UserId::new(subscriber);
                    let used: u64 = count_from(used, "byte count")?;
                    let added: u64 = count_from(added, "byte count")?;
                    tracing::trace!(%job, %subscriber, used, added, budget = self.budget.limit(), "a subscriber's caching budget on the peer");
                    if refusal.is_none() {
                        refusal = self.budget.refusal(&subscriber, &job.peer, used, added);
                    }
                }
                if refusal.is_some() {
                    break;
                }
            }
            match &refusal {
                Some(over) => tracing::info!(
                    %job,
                    subscriber = %over.subscriber,
                    used = over.used,
                    budget = over.budget,
                    "a pulled page would pass a subscriber's caching budget; it is rolled back"
                ),
                None => {
                    tracing::debug!(%job, blobs = held.len(), subscribers, "pulled metadata blobs charged within every subscriber's budget");
                }
            }
            Ok(refusal)
        })
    }
}

/// The durable pulled-album store.
#[derive(Debug, Clone)]
pub struct PostgresPulledAlbums {
    connection: DatabaseConnection,
    blobs: Arc<dyn BlobStore>,
    charge: Arc<dyn PulledBlobCharge>,
}

impl PostgresPulledAlbums {
    /// A store over `connection`, keeping metadata blobs in `blobs`, charging nothing.
    pub fn new(connection: DatabaseConnection, blobs: Arc<dyn BlobStore>) -> Self {
        Self {
            connection,
            blobs,
            charge: Arc::new(Uncharged),
        }
    }

    /// This store, charging newly held metadata blobs through `charge`.
    #[must_use]
    pub fn with_charge(mut self, charge: Arc<dyn PulledBlobCharge>) -> Self {
        self.charge = charge;
        self
    }

    /// Every row `sql` selects with `values`.
    async fn select(
        &self,
        sql: &str,
        values: Vec<Value>,
        doing: &'static str,
    ) -> Result<Vec<QueryResult>, StoreError> {
        self.connection
            .query_all_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await
            .map_err(PORT.failing(doing))
    }

    /// The commit itself, once the transaction is open.
    async fn commit_in(
        &self,
        transaction: &DatabaseTransaction,
        job: &JobKey,
        from: Option<u64>,
        page: ValidatedPage,
    ) -> Result<Committed, StoreError> {
        let key = job_values(job);
        transaction
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO federation_pulled_albums (peer_id, album_id) VALUES ($1, $2) \
                 ON CONFLICT (peer_id, album_id) DO NOTHING",
                key.clone(),
            ))
            .await
            .map_err(PORT.failing("creating a pulled album"))?;
        let Some(row) = transaction
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT high_water, entries_written FROM federation_pulled_albums \
                 WHERE peer_id = $1 AND album_id = $2 FOR UPDATE",
                key.clone(),
            ))
            .await
            .map_err(PORT.failing("locking a pulled album"))?
        else {
            return Err(StoreError::Rejected {
                store: PORT.store,
                detail: "the pulled album's row was not there after it was created".to_owned(),
            });
        };
        let failed = PORT.failing("locking a pulled album");
        let held_high_water: Option<i64> = row.try_get("", "high_water").map_err(&failed)?;
        let held_high_water = held_high_water
            .map(|value| count_from::<u64>(value, "high-water mark"))
            .transpose()?;
        if held_high_water != from {
            tracing::debug!(%job, ?from, current = ?held_high_water, "a pulled page was validated against a stale position");
            return Ok(Committed::Stale);
        }
        let entries_written: i64 = row.try_get("", "entries_written").map_err(&failed)?;

        let newly_held = self
            .hold_metadata_blobs(transaction, job, &page.metadata_blobs)
            .await?;
        if !newly_held.is_empty()
            && let Some(refusal) = self.charge.charge(transaction, job, &newly_held).await?
        {
            return Ok(Committed::OverBudget(refusal));
        }
        let assets = write_heads(transaction, job, &page.state.assets).await?;
        let written = write_entries(transaction, job, entries_written, &page.entries).await?;

        let state = &page.state;
        let mut values = key;
        values.extend([
            Value::from(state.cursor.clone()),
            Value::from(state.high_water.map(to_column).transpose()?),
            Value::from(state.amk_version.map(i64::from)),
            Value::from(written),
        ]);
        transaction
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE federation_pulled_albums \
                 SET cursor = $3, high_water = $4, amk_version = $5, entries_written = $6 \
                 WHERE peer_id = $1 AND album_id = $2",
                values,
            ))
            .await
            .map_err(PORT.failing("moving a pulled album's position"))?;
        tracing::debug!(
            %job,
            ?from,
            high_water = ?state.high_water,
            entries = page.entries.len(),
            metadata_blobs = page.metadata_blobs.len(),
            newly_held = newly_held.len(),
            assets_written = assets,
            "a pulled page is ready to commit"
        );
        Ok(Committed::Written)
    }

    /// Put every blob of `blobs` in the blob store and record that `job` holds it, answering the
    /// ones it did not hold before.
    async fn hold_metadata_blobs(
        &self,
        transaction: &DatabaseTransaction,
        job: &JobKey,
        blobs: &BTreeMap<Hash32, Vec<u8>>,
    ) -> Result<Vec<HeldBlob>, StoreError> {
        let mut rows = Vec::with_capacity(blobs.len());
        for (hash, bytes) in blobs {
            let address = address_of(hash)?;
            let placement = self
                .blobs
                .put(&address, bytes)
                .await
                .map_err(blob_failure)?;
            tracing::trace!(%job, %address, stored = placement == Placement::Stored, "a pulled metadata blob is in the blob store");
            let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            let mut row = job_values(job);
            row.extend([
                Value::from(address.as_str().to_owned()),
                Value::from(to_column(size)?),
            ]);
            rows.push(row);
        }
        let inserted = insert_rows(
            transaction,
            "INSERT INTO federation_pulled_metadata_blobs (peer_id, album_id, address, size)",
            rows,
            "ON CONFLICT (peer_id, album_id, address) DO NOTHING RETURNING address, size",
            "recording a job's metadata blobs",
        )
        .await?;
        let failed = PORT.failing("recording a job's metadata blobs");
        inserted
            .iter()
            .map(|row| {
                let address: String = row.try_get("", "address").map_err(&failed)?;
                let size: i64 = row.try_get("", "size").map_err(&failed)?;
                Ok(HeldBlob {
                    hash: hash_from_hex(&address)?,
                    size: count_from(size, "blob size")?,
                })
            })
            .collect()
    }
}

/// A job's key as the first two bind parameters.
fn job_values(job: &JobKey) -> Vec<Value> {
    vec![
        Value::from(job.peer.as_str().to_owned()),
        Value::from(job.album.as_str().to_owned()),
    ]
}

/// An unsigned count as a `BIGINT`, refused past what one holds.
fn to_column(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::Rejected {
        store: PORT.store,
        detail: format!("{value} is past what a BIGINT column holds"),
    })
}

/// A `BIGINT` column as an unsigned count.
fn count_from<T: TryFrom<i64>>(value: i64, what: &str) -> Result<T, StoreError> {
    T::try_from(value).map_err(|_| PORT.undecodable(format!("{value} is not a {what}")))
}

/// The blob-store address of `hash`.
fn address_of(hash: &Hash32) -> Result<ContentAddress, StoreError> {
    ContentAddress::parse(&hash.to_hex()).map_err(|error| StoreError::Rejected {
        store: PORT.store,
        detail: format!("{hash} is not a content address: {error}"),
    })
}

/// A stored hex address as the hash it spells.
fn hash_from_hex(hex: &str) -> Result<Hash32, StoreError> {
    Hash32::from_hex(hex)
        .map_err(|error| PORT.undecodable(format!("`{hex}` is not a hash: {error}")))
}

/// A stored head as the hash it holds.
fn hash_from_bytes(bytes: &[u8]) -> Result<Hash32, StoreError> {
    <[u8; 32]>::try_from(bytes)
        .map(Hash32)
        .map_err(|_| PORT.undecodable(format!("a head of {} bytes is not a hash", bytes.len())))
}

/// Heads as one column: their concatenation, in order.
fn encode_heads(heads: &[Hash32]) -> Vec<u8> {
    heads.iter().flat_map(|head| head.0).collect()
}

/// The column [`encode_heads`] wrote, as the heads it holds.
fn decode_heads(bytes: &[u8]) -> Result<Vec<Hash32>, StoreError> {
    if !bytes.len().is_multiple_of(32) {
        return Err(PORT.undecodable(format!(
            "{} bytes of superseded heads is not a whole number of hashes",
            bytes.len()
        )));
    }
    bytes.chunks_exact(32).map(hash_from_bytes).collect()
}

/// A change as its column.
fn change_name(change: Change) -> &'static str {
    match change {
        Change::Created => "created",
        Change::Updated => "updated",
        Change::Deleted => "deleted",
    }
}

/// The column [`change_name`] wrote, as the change it names.
fn change_from(name: &str) -> Result<Change, StoreError> {
    match name {
        "created" => Ok(Change::Created),
        "updated" => Ok(Change::Updated),
        "deleted" => Ok(Change::Deleted),
        other => Err(PORT.undecodable(format!("`{other}` is not a change"))),
    }
}

/// A stored blob role as the role it names. The record holds a `&'static str`, so only the
/// roles the wire has are read back.
fn role_from(name: &str) -> Result<&'static str, StoreError> {
    ["original", "metadata", "derivative", "provenance"]
        .into_iter()
        .find(|role| *role == name)
        .ok_or_else(|| PORT.undecodable(format!("`{name}` is not a blob role")))
}

/// A blob-store failure as the store error the port answers.
///
/// `Rejected`, not `Unavailable`: a put that failed may have left its bytes at the address,
/// so whether state changed is unknown. A leftover is an unreferenced blob, which is benign.
fn blob_failure(error: BlobError) -> StoreError {
    StoreError::Rejected {
        store: PORT.store,
        detail: format!("the blob store failed a pulled metadata blob: {error}"),
    }
}

/// Run `head VALUES (…), (…) tail` over `rows`, as few statements as the parameter bound allows,
/// answering every row the statements return.
async fn insert_rows(
    transaction: &DatabaseTransaction,
    head: &str,
    rows: Vec<Vec<Value>>,
    tail: &str,
    doing: &'static str,
) -> Result<Vec<QueryResult>, StoreError> {
    let Some(width) = rows.first().map(Vec::len) else {
        return Ok(Vec::new());
    };
    let per_statement = (MAX_PARAMETERS / width.max(1)).max(1);
    let mut returned = Vec::new();
    let mut rows = rows.into_iter().peekable();
    while rows.peek().is_some() {
        let mut values = Vec::new();
        let mut tuples = Vec::new();
        for row in rows.by_ref().take(per_statement) {
            let first = values.len();
            let placeholders: Vec<String> = (1..=row.len())
                .map(|offset| format!("${}", first + offset))
                .collect();
            tuples.push(format!("({})", placeholders.join(", ")));
            values.extend(row);
        }
        let sql = format!("{head} VALUES {} {tail}", tuples.join(", "));
        returned.extend(
            transaction
                .query_all_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    sql,
                    values,
                ))
                .await
                .map_err(PORT.failing(doing))?,
        );
    }
    Ok(returned)
}

/// Decode one `federation_pulled_assets` row.
fn asset_from(row: &QueryResult) -> Result<(String, AssetHead), StoreError> {
    let failed = PORT.failing("reading a pulled asset head");
    let asset_id: String = row.try_get("", "asset_id").map_err(&failed)?;
    let head: Vec<u8> = row.try_get("", "head").map_err(&failed)?;
    let superseded: Vec<u8> = row.try_get("", "superseded").map_err(&failed)?;
    let deleted: bool = row.try_get("", "deleted").map_err(&failed)?;
    let amk_version: i64 = row.try_get("", "amk_version").map_err(&failed)?;
    let metadata_blob: Option<String> = row.try_get("", "metadata_blob").map_err(&failed)?;
    Ok((
        asset_id,
        AssetHead {
            head: hash_from_bytes(&head)?,
            superseded: decode_heads(&superseded)?,
            deleted,
            amk_version: count_from(amk_version, "AMK epoch")?,
            metadata_blob: metadata_blob.as_deref().map(hash_from_hex).transpose()?,
        },
    ))
}

/// The columns [`asset_from`] decodes.
const ASSET_COLUMNS: &str = "asset_id, head, superseded, deleted, amk_version, metadata_blob";

/// Make `job`'s stored heads `assets`, writing only what differs. Answers how many were written
/// or removed.
async fn write_heads(
    transaction: &DatabaseTransaction,
    job: &JobKey,
    assets: &BTreeMap<String, AssetHead>,
) -> Result<usize, StoreError> {
    let held: BTreeMap<String, AssetHead> = transaction
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT {ASSET_COLUMNS} FROM federation_pulled_assets \
                 WHERE peer_id = $1 AND album_id = $2"
            ),
            job_values(job),
        ))
        .await
        .map_err(PORT.failing("reading the held asset heads"))?
        .iter()
        .map(asset_from)
        .collect::<Result<_, _>>()?;

    let mut rows = Vec::new();
    for (asset_id, head) in assets {
        if held.get(asset_id) == Some(head) {
            continue;
        }
        let mut row = job_values(job);
        row.extend([
            Value::from(asset_id.clone()),
            Value::from(head.head.0.to_vec()),
            Value::from(encode_heads(&head.superseded)),
            Value::from(head.deleted),
            Value::from(i64::from(head.amk_version)),
            Value::from(head.metadata_blob.map(|hash| hash.to_hex())),
        ]);
        rows.push(row);
    }
    let changed = rows.len();
    insert_rows(
        transaction,
        "INSERT INTO federation_pulled_assets (peer_id, album_id, asset_id, head, superseded, \
         deleted, amk_version, metadata_blob)",
        rows,
        "ON CONFLICT (peer_id, album_id, asset_id) DO UPDATE SET head = EXCLUDED.head, \
         superseded = EXCLUDED.superseded, deleted = EXCLUDED.deleted, \
         amk_version = EXCLUDED.amk_version, metadata_blob = EXCLUDED.metadata_blob",
        "writing asset heads",
    )
    .await?;

    let gone: Vec<&String> = held.keys().filter(|id| !assets.contains_key(*id)).collect();
    for chunk in gone.chunks(MAX_PARAMETERS) {
        let placeholders: Vec<String> = (0..chunk.len())
            .map(|index| format!("${}", index + 3))
            .collect();
        let mut values = job_values(job);
        values.extend(chunk.iter().map(|id| Value::from((*id).clone())));
        transaction
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "DELETE FROM federation_pulled_assets \
                     WHERE peer_id = $1 AND album_id = $2 AND asset_id IN ({})",
                    placeholders.join(", ")
                ),
                values,
            ))
            .await
            .map_err(PORT.failing("removing asset heads"))?;
    }
    Ok(changed + gone.len())
}

/// Write `entries` for `job`, numbered from `first`. Answers the next number.
async fn write_entries(
    transaction: &DatabaseTransaction,
    job: &JobKey,
    first: i64,
    entries: &[AcceptedEntry],
) -> Result<i64, StoreError> {
    let mut entry_rows = Vec::with_capacity(entries.len());
    let mut blob_rows = Vec::new();
    let mut derivative_rows = Vec::new();
    let mut position = first;
    for entry in entries {
        let mut row = job_values(job);
        row.extend([
            Value::from(position),
            Value::from(entry.asset_id.clone()),
            Value::from(to_column(entry.sync_seq)?),
            Value::from(change_name(entry.change).to_owned()),
            Value::from(to_micros(entry.changed_at)),
            Value::from(entry.manifest.clone()),
            Value::from(entry.head.map(|head| head.0.to_vec())),
            Value::from(entry.metadata_blob.map(|hash| hash.to_hex())),
            Value::from(entry.original_held),
            Value::from(entry.purged),
        ]);
        entry_rows.push(row);
        for (ordinal, blob) in (0_i32..).zip(&entry.blobs) {
            let mut row = job_values(job);
            row.extend([
                Value::from(position),
                Value::from(ordinal),
                Value::from(blob.role.to_owned()),
                Value::from(blob.hash.to_hex()),
                Value::from(to_column(blob.size)?),
            ]);
            blob_rows.push(row);
        }
        for (ordinal, manifest) in (0_i32..).zip(&entry.derivative_manifests) {
            let mut row = job_values(job);
            row.extend([
                Value::from(position),
                Value::from(ordinal),
                Value::from(manifest.clone()),
            ]);
            derivative_rows.push(row);
        }
        position = position
            .checked_add(1)
            .ok_or_else(|| StoreError::Rejected {
                store: PORT.store,
                detail: "the job has written more entries than a BIGINT counts".to_owned(),
            })?;
    }
    insert_rows(
        transaction,
        "INSERT INTO federation_accepted_entries (peer_id, album_id, position, asset_id, \
         sync_seq, change, changed_at, manifest, head, metadata_blob, original_held, purged)",
        entry_rows,
        "",
        "writing accepted entries",
    )
    .await?;
    insert_rows(
        transaction,
        "INSERT INTO federation_accepted_entry_blobs (peer_id, album_id, position, ordinal, \
         role, address, size)",
        blob_rows,
        "",
        "writing accepted entries' blobs",
    )
    .await?;
    insert_rows(
        transaction,
        "INSERT INTO federation_accepted_entry_derivatives (peer_id, album_id, position, \
         ordinal, manifest)",
        derivative_rows,
        "",
        "writing accepted entries' derivative manifests",
    )
    .await?;
    Ok(position)
}

impl PulledAlbums for PostgresPulledAlbums {
    fn read<'a>(&'a self, job: &'a JobKey) -> StoreFuture<'a, PulledAlbum> {
        Box::pin(async move {
            let rows = self
                .select(
                    "SELECT cursor, high_water, amk_version FROM federation_pulled_albums \
                     WHERE peer_id = $1 AND album_id = $2",
                    job_values(job),
                    "reading a pulled album",
                )
                .await?;
            let Some(row) = rows.first() else {
                return Ok(PulledAlbum::default());
            };
            let failed = PORT.failing("reading a pulled album");
            let cursor: Option<String> = row.try_get("", "cursor").map_err(&failed)?;
            let high_water: Option<i64> = row.try_get("", "high_water").map_err(&failed)?;
            let amk_version: Option<i64> = row.try_get("", "amk_version").map_err(&failed)?;
            let assets = self
                .select(
                    &format!(
                        "SELECT {ASSET_COLUMNS} FROM federation_pulled_assets \
                         WHERE peer_id = $1 AND album_id = $2"
                    ),
                    job_values(job),
                    "reading a pulled album's heads",
                )
                .await?
                .iter()
                .map(asset_from)
                .collect::<Result<_, _>>()?;
            Ok(PulledAlbum {
                cursor,
                high_water: high_water
                    .map(|value| count_from(value, "high-water mark"))
                    .transpose()?,
                amk_version: amk_version
                    .map(|value| count_from(value, "AMK epoch"))
                    .transpose()?,
                assets,
            })
        })
    }

    fn commit<'a>(
        &'a self,
        job: &'a JobKey,
        from: Option<u64>,
        page: ValidatedPage,
    ) -> StoreFuture<'a, Committed> {
        Box::pin(async move {
            let transaction = self
                .connection
                .begin()
                .await
                .map_err(PORT.failing("opening a transaction"))?;
            // A refusal or an error returns before the commit; dropping the transaction rolls
            // it back.
            let committed = self.commit_in(&transaction, job, from, page).await?;
            if committed != Committed::Written {
                return Ok(committed);
            }
            transaction
                .commit()
                .await
                .map_err(PORT.failing("committing a pulled page"))?;
            tracing::debug!(%job, "a pulled page was committed");
            Ok(Committed::Written)
        })
    }

    fn restart<'a>(&'a self, job: &'a JobKey) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let result = self
                .connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE federation_pulled_albums SET cursor = NULL, high_water = NULL \
                     WHERE peer_id = $1 AND album_id = $2",
                    job_values(job),
                ))
                .await
                .map_err(PORT.failing("restarting a pulled album"))?;
            tracing::info!(%job, held = result.rows_affected() > 0, "a pulled album restarts from the start of its feed");
            Ok(())
        })
    }
}

impl Inspect for PostgresPulledAlbums {
    fn written_entries<'a>(&'a self, job: &'a JobKey) -> StoreFuture<'a, Vec<AcceptedEntry>> {
        Box::pin(async move {
            let doing = "listing a job's accepted entries";
            let failed = PORT.failing(doing);
            let mut blobs: BTreeMap<i64, Vec<PulledBlob>> = BTreeMap::new();
            for row in self
                .select(
                    "SELECT position, role, address, size FROM federation_accepted_entry_blobs \
                     WHERE peer_id = $1 AND album_id = $2 ORDER BY position, ordinal",
                    job_values(job),
                    doing,
                )
                .await?
            {
                let position: i64 = row.try_get("", "position").map_err(&failed)?;
                let role: String = row.try_get("", "role").map_err(&failed)?;
                let address: String = row.try_get("", "address").map_err(&failed)?;
                let size: i64 = row.try_get("", "size").map_err(&failed)?;
                blobs.entry(position).or_default().push(PulledBlob {
                    role: role_from(&role)?,
                    hash: hash_from_hex(&address)?,
                    size: count_from(size, "blob size")?,
                });
            }
            let mut derivatives: BTreeMap<i64, Vec<Vec<u8>>> = BTreeMap::new();
            for row in self
                .select(
                    "SELECT position, manifest FROM federation_accepted_entry_derivatives \
                     WHERE peer_id = $1 AND album_id = $2 ORDER BY position, ordinal",
                    job_values(job),
                    doing,
                )
                .await?
            {
                let position: i64 = row.try_get("", "position").map_err(&failed)?;
                let manifest: Vec<u8> = row.try_get("", "manifest").map_err(&failed)?;
                derivatives.entry(position).or_default().push(manifest);
            }
            self.select(
                "SELECT position, asset_id, sync_seq, change, changed_at, manifest, head, \
                 metadata_blob, original_held, purged FROM federation_accepted_entries \
                 WHERE peer_id = $1 AND album_id = $2 ORDER BY position",
                job_values(job),
                doing,
            )
            .await?
            .iter()
            .map(|row| {
                let position: i64 = row.try_get("", "position").map_err(&failed)?;
                let change: String = row.try_get("", "change").map_err(&failed)?;
                let sync_seq: i64 = row.try_get("", "sync_seq").map_err(&failed)?;
                let changed_at: i64 = row.try_get("", "changed_at").map_err(&failed)?;
                let head: Option<Vec<u8>> = row.try_get("", "head").map_err(&failed)?;
                let metadata_blob: Option<String> =
                    row.try_get("", "metadata_blob").map_err(&failed)?;
                Ok(AcceptedEntry {
                    asset_id: row.try_get("", "asset_id").map_err(&failed)?,
                    sync_seq: count_from(sync_seq, "feed position")?,
                    change: change_from(&change)?,
                    changed_at: from_micros(changed_at).ok_or_else(|| {
                        PORT.undecodable(format!("{changed_at}µs is not a representable instant"))
                    })?,
                    manifest: row.try_get("", "manifest").map_err(&failed)?,
                    head: head.as_deref().map(hash_from_bytes).transpose()?,
                    metadata_blob: metadata_blob.as_deref().map(hash_from_hex).transpose()?,
                    blobs: blobs.remove(&position).unwrap_or_default(),
                    derivative_manifests: derivatives.remove(&position).unwrap_or_default(),
                    original_held: row.try_get("", "original_held").map_err(&failed)?,
                    purged: row.try_get("", "purged").map_err(&failed)?,
                })
            })
            .collect()
        })
    }

    fn held_metadata_blob<'a>(
        &'a self,
        job: &'a JobKey,
        hash: &'a Hash32,
    ) -> StoreFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            let address = address_of(hash)?;
            let mut values = job_values(job);
            values.push(Value::from(address.as_str().to_owned()));
            let rows = self
                .select(
                    "SELECT size FROM federation_pulled_metadata_blobs \
                     WHERE peer_id = $1 AND album_id = $2 AND address = $3",
                    values,
                    "looking up a held metadata blob",
                )
                .await?;
            let Some(row) = rows.first() else {
                return Ok(None);
            };
            let size: i64 = row
                .try_get("", "size")
                .map_err(PORT.failing("looking up a held metadata blob"))?;
            let len: usize = count_from(size, "blob size")?;
            let bytes = self
                .blobs
                .read_at(&address, 0, len)
                .await
                .map_err(blob_failure)?;
            if bytes.is_none() {
                // A row naming bytes the store does not have: the dangling reference the
                // collector reports and never resolves by deleting the row.
                tracing::error!(%job, %address, "a held pulled metadata blob is missing from the blob store");
            }
            Ok(bytes)
        })
    }
}

#[cfg(test)]
mod tests;
