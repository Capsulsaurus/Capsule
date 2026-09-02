//! [`PostgresAssetIndex`] — the durable asset index (`S-C37`, #402).
//!
//! # The one thing this adapter exists to get right
//!
//! A sequence number is allocated **inside the same critical section that makes its row
//! readable**. The in-memory double gets that from holding one mutex across both writes; this
//! gets it from a row lock held to commit, which is the structure `index/mod.rs` designed the
//! port around and the one a single-process conformance suite cannot prove. Every mutating
//! operation here is one `BEGIN … COMMIT` that takes `SELECT … FOR UPDATE` on the asset row
//! first, mints from `owner_sequences` inside it, and commits both together.
//!
//! `owner_sequences` is a **counter row**, never a Postgres `SEQUENCE` or a `bigserial`.
//! `nextval` is deliberately non-transactional: it hands 5 and 6 to two concurrent
//! finalizations and does not roll back, so a reader who sees 6 commit first can page past 5
//! forever. `index/mod.rs` names that as the whole of `S-C21`, and a counter row updated by the
//! allocating transaction makes allocation order equal commit order.
//!
//! # Why the mutations are written in Rust rather than in SQL
//!
//! Each write hydrates the whole [`AssetRow`] under the lock, applies the same free functions
//! the in-memory adapter applies (`is_singular`, `set_singular`,
//! [`AssetRow::is_publishable`]), and writes the row back before committing. The alternative —
//! expressing the state machine as a chain of `UPDATE … WHERE` statements — would be a *second*
//! statement of rules the port already fixes, in a language where the conformance suite cannot
//! see it. The lock is held for the length of one small in-memory computation, and the rules
//! that decide what a row becomes stay in one place for both adapters.
//!
//! # What is a query and never a stored number
//!
//! [`AssetIndex::reference_count`] is a `COUNT(*)`. design/filesystem/server.md fixes that: a
//! blob's reference count is derived from the rows that name it, because a counter is a second
//! copy of a derivable fact and a counter that drifts low deletes a live blob. Superseded
//! manifests count (`S-C52`), or the collector reclaims the server's own rebuttal evidence.

use capsule_core::crypto::hash::Hash32;
use jiff::Timestamp;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait, Value,
};

use super::{
    AssetIndex, AssetRow, AssetState, BlobOutcome, BlobRecord, BlobRef, BlobReference, FeedEntry,
    HoldOutcome, IndexFuture, LifecycleOp, OpAction, OpOutcome, PendingAsset, Reservation,
    ServingHold, entry_for, is_singular, set_singular,
};
use crate::blob::ContentAddress;
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};
use crate::store::{AlbumId, AssetId, BlobRole, OwnerId, StoreError};

/// Which port is speaking, for every error this adapter raises.
const PORT: Port = Port {
    store: "asset-index",
    record: "AssetRow",
};

/// The durable asset index.
#[derive(Debug, Clone)]
pub struct PostgresAssetIndex {
    connection: DatabaseConnection,
}

impl PostgresAssetIndex {
    /// An index over `connection`.
    ///
    /// The schema is **not** applied here: `capsule-server` cannot link the migrator, and
    /// `postgres::assert_schema_current` is what refuses to boot against a database that has not
    /// been migrated.
    pub fn new(connection: DatabaseConnection) -> Self {
        Self { connection }
    }
}

// -------------------------------------------------------------------------------------------
// Column encodings
//
// Every enum crosses the boundary as its own stable wire token rather than as an ordinal, for
// the reason the tokens exist at all: an ordinal is a number whose meaning lives in the order of
// a Rust `enum`, and inserting a variant would silently re-label every stored row.
// -------------------------------------------------------------------------------------------

/// The token an [`AssetState`] is stored as.
fn state_token(state: AssetState) -> &'static str {
    match state {
        AssetState::Pending => "pending",
        AssetState::Visible => "visible",
        AssetState::Tombstoned => "tombstoned",
    }
}

/// Read a stored state back, or say the row is undecodable.
fn state_from(token: &str) -> Result<AssetState, StoreError> {
    match token {
        "pending" => Ok(AssetState::Pending),
        "visible" => Ok(AssetState::Visible),
        "tombstoned" => Ok(AssetState::Tombstoned),
        other => Err(PORT.undecodable(format!("`{other}` is not an asset state"))),
    }
}

/// Read a stored hold back.
fn hold_from(token: Option<String>) -> Result<Option<ServingHold>, StoreError> {
    match token.as_deref() {
        None => Ok(None),
        Some("takedown") => Ok(Some(ServingHold::Takedown)),
        Some("legal_hold") => Ok(Some(ServingHold::LegalHold)),
        Some(other) => Err(PORT.undecodable(format!("`{other}` is not a serving hold"))),
    }
}

/// Read a stored blob role back.
fn role_from(token: &str) -> Result<BlobRole, StoreError> {
    match token {
        "original" => Ok(BlobRole::Original),
        "derivative" => Ok(BlobRole::Derivative),
        "metadata" => Ok(BlobRole::Metadata),
        "provenance" => Ok(BlobRole::Provenance),
        "backup" => Ok(BlobRole::Backup),
        other => Err(PORT.undecodable(format!("`{other}` is not a blob role"))),
    }
}

/// Read a stored content address back.
fn address_from(text: &str) -> Result<ContentAddress, StoreError> {
    ContentAddress::parse(text)
        .map_err(|error| PORT.undecodable(format!("a stored address is malformed ({error})")))
}

/// Read a stored instant back.
fn instant_from(micros: i64) -> Result<Timestamp, StoreError> {
    from_micros(micros)
        .ok_or_else(|| PORT.undecodable(format!("{micros}µs is not a representable instant")))
}

/// Read a stored chain head back.
fn hash_from(bytes: Option<Vec<u8>>) -> Result<Option<Hash32>, StoreError> {
    let Some(bytes) = bytes else { return Ok(None) };
    let sized: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        PORT.undecodable(format!(
            "a stored chain head is {} bytes rather than 32",
            bytes.len()
        ))
    })?;
    Ok(Some(Hash32::from_bytes(sized)))
}

/// A sequence number as the port speaks it.
///
/// Sequence numbers are `u64` above this boundary and `BIGINT` below it. The conversion is
/// fallible in exactly one direction, and a negative one in the column is a corrupt row rather
/// than a number to clamp.
fn sequence_from(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| PORT.undecodable(format!("{value} is not a sequence number")))
}

/// A byte count as the column holds it.
fn size_to_column(size: u64) -> Result<i64, StoreError> {
    i64::try_from(size).map_err(|_| StoreError::Rejected {
        store: PORT.store,
        detail: format!("{size} bytes is past what a BIGINT column holds"),
    })
}

/// A byte count as the port speaks it.
fn size_from(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| PORT.undecodable(format!("{value} is not a byte count")))
}

// -------------------------------------------------------------------------------------------
// Reading a row back
// -------------------------------------------------------------------------------------------

/// Every column of `assets`, in the order every `SELECT` below lists them.
const ASSET_COLUMNS: &str = "asset_id, owner_id, album_id, protocol_version, crypto_suite_id, \
                             state, hold, sync_seq, first_seq, chain_head, amk_version, \
                             retention_until, created_at, updated_at";

/// Turn one `assets` row into an [`AssetRow`] with empty collections.
///
/// The blobs and the superseded chain are loaded separately, so a page of rows costs three
/// queries rather than three per row.
fn asset_without_collections(row: &sea_orm::QueryResult) -> Result<AssetRow, StoreError> {
    let column = PORT.failing("reading an asset row");
    let asset_id: String = row.try_get("", "asset_id").map_err(&column)?;
    let owner_id: String = row.try_get("", "owner_id").map_err(&column)?;
    let album_id: String = row.try_get("", "album_id").map_err(&column)?;
    let protocol_version: String = row.try_get("", "protocol_version").map_err(&column)?;
    let crypto_suite_id: i32 = row.try_get("", "crypto_suite_id").map_err(&column)?;
    let state: String = row.try_get("", "state").map_err(&column)?;
    let hold: Option<String> = row.try_get("", "hold").map_err(&column)?;
    let sync_seq: Option<i64> = row.try_get("", "sync_seq").map_err(&column)?;
    let first_seq: Option<i64> = row.try_get("", "first_seq").map_err(&column)?;
    let chain_head: Option<Vec<u8>> = row.try_get("", "chain_head").map_err(&column)?;
    let amk_version: i64 = row.try_get("", "amk_version").map_err(&column)?;
    let retention_until: Option<i64> = row.try_get("", "retention_until").map_err(&column)?;
    let created_at: i64 = row.try_get("", "created_at").map_err(&column)?;
    let updated_at: i64 = row.try_get("", "updated_at").map_err(&column)?;

    Ok(AssetRow {
        asset_id: AssetId::new(asset_id),
        owner_id: OwnerId::new(owner_id),
        album_id: AlbumId::new(album_id),
        protocol_version,
        crypto_suite_id: u16::try_from(crypto_suite_id)
            .map_err(|_| PORT.undecodable(format!("{crypto_suite_id} is not a crypto suite id")))?,
        state: state_from(&state)?,
        blobs: Vec::new(),
        first_seq: first_seq.map(sequence_from).transpose()?,
        sync_seq: sync_seq.map(sequence_from).transpose()?,
        chain_head: hash_from(chain_head)?,
        amk_version: sequence_from(amk_version)?,
        superseded: Vec::new(),
        hold: hold_from(hold)?,
        retention_until: retention_until.map(instant_from).transpose()?,
        created_at: instant_from(created_at)?,
        updated_at: instant_from(updated_at)?,
    })
}

/// Fill in `row`'s blobs and superseded chain.
async fn load_collections<C: ConnectionTrait>(
    connection: &C,
    row: &mut AssetRow,
) -> Result<(), StoreError> {
    let key = Value::from(row.asset_id.as_str().to_owned());

    let blobs = connection
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT role, address, size FROM asset_blobs WHERE asset_id = $1",
            [key.clone()],
        ))
        .await
        .map_err(PORT.failing("reading an asset's blobs"))?;
    for blob in &blobs {
        let role: String = blob
            .try_get("", "role")
            .map_err(PORT.failing("reading a blob role"))?;
        let address: String = blob
            .try_get("", "address")
            .map_err(PORT.failing("reading a blob address"))?;
        let size: i64 = blob
            .try_get("", "size")
            .map_err(PORT.failing("reading a blob size"))?;
        row.blobs.push(BlobRef {
            role: role_from(&role)?,
            address: address_from(&address)?,
            size: size_from(size)?,
        });
    }
    // The port contracts role-then-address order and a `Vec` will not sort itself, so two
    // adapters that accepted the same blobs hold the same row.
    row.blobs.sort();

    let superseded = connection
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT address FROM asset_superseded WHERE asset_id = $1 ORDER BY position",
            [key],
        ))
        .await
        .map_err(PORT.failing("reading an asset's superseded manifests"))?;
    for held in &superseded {
        let address: String = held
            .try_get("", "address")
            .map_err(PORT.failing("reading a superseded address"))?;
        row.superseded.push(address_from(&address)?);
    }

    Ok(())
}

/// The whole row for `asset`, or `None`.
async fn hydrate<C: ConnectionTrait>(
    connection: &C,
    asset: &AssetId,
) -> Result<Option<AssetRow>, StoreError> {
    let found = connection
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("SELECT {ASSET_COLUMNS} FROM assets WHERE asset_id = $1"),
            [Value::from(asset.as_str().to_owned())],
        ))
        .await
        .map_err(PORT.failing("reading an asset row"))?;
    let Some(found) = found else { return Ok(None) };
    let mut row = asset_without_collections(&found)?;
    load_collections(connection, &mut row).await?;
    Ok(Some(row))
}

/// The whole row for `asset`, with its row locked until the transaction commits.
///
/// `FOR UPDATE` is what makes the sequence mint below it commit-ordered: two finalizations for
/// one asset serialize here, and the number the second one gets is allocated after the first has
/// committed rather than beside it.
async fn hydrate_locked(
    transaction: &DatabaseTransaction,
    asset: &AssetId,
) -> Result<Option<AssetRow>, StoreError> {
    let found = transaction
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("SELECT {ASSET_COLUMNS} FROM assets WHERE asset_id = $1 FOR UPDATE"),
            [Value::from(asset.as_str().to_owned())],
        ))
        .await
        .map_err(PORT.failing("locking an asset row"))?;
    let Some(found) = found else { return Ok(None) };
    let mut row = asset_without_collections(&found)?;
    load_collections(transaction, &mut row).await?;
    Ok(Some(row))
}

/// Hydrate each of `rows`' collections, in order.
async fn load_all_collections<C: ConnectionTrait>(
    connection: &C,
    rows: &mut [AssetRow],
) -> Result<(), StoreError> {
    for row in rows {
        load_collections(connection, row).await?;
    }
    Ok(())
}

// -------------------------------------------------------------------------------------------
// Writing a row back
// -------------------------------------------------------------------------------------------

/// Allocate `owner`'s next sequence number, inside the caller's transaction.
///
/// The upsert is the allocation: the row is created at 1 on an owner's first publication and
/// incremented under the transaction's lock afterwards, so allocation order is commit order and
/// the skip window a `SEQUENCE` produces is not expressible. Numbers start at 1 so that a fresh
/// client's cursor and "I have seen nothing" are the same value.
async fn mint(transaction: &DatabaseTransaction, owner: &OwnerId) -> Result<u64, StoreError> {
    let minted = transaction
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO owner_sequences (owner_id, next_seq) VALUES ($1, 1) \
             ON CONFLICT (owner_id) DO UPDATE SET next_seq = owner_sequences.next_seq + 1 \
             RETURNING next_seq",
            [Value::from(owner.as_str().to_owned())],
        ))
        .await
        .map_err(PORT.failing("allocating a sequence number"))?
        .ok_or_else(|| StoreError::Rejected {
            store: PORT.store,
            detail: "the sequence allocation returned no row".to_owned(),
        })?;
    let next: i64 = minted
        .try_get("", "next_seq")
        .map_err(PORT.failing("reading an allocated sequence number"))?;
    sequence_from(next)
}

/// Write `row`'s mutable columns, blobs and superseded chain back.
///
/// Whole-collection replacement rather than a diff, and deliberately: the row is locked, the
/// collections are a handful of entries, and a diff would be a second description of what the
/// mutation did — one that can disagree with the row the caller is about to return.
async fn persist(transaction: &DatabaseTransaction, row: &AssetRow) -> Result<(), StoreError> {
    let asset = Value::from(row.asset_id.as_str().to_owned());
    transaction
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE assets SET state = $2, hold = $3, sync_seq = $4, first_seq = $5, \
             chain_head = $6, amk_version = $7, retention_until = $8, updated_at = $9 \
             WHERE asset_id = $1",
            [
                asset.clone(),
                Value::from(state_token(row.state).to_owned()),
                Value::from(row.hold.map(|hold| hold.as_str().to_owned())),
                Value::from(row.sync_seq.map(|seq| seq as i64)),
                Value::from(row.first_seq.map(|seq| seq as i64)),
                Value::from(row.chain_head.map(|head| head.as_bytes().to_vec())),
                Value::from(row.amk_version as i64),
                Value::from(row.retention_until.map(to_micros)),
                Value::from(to_micros(row.updated_at)),
            ],
        ))
        .await
        .map_err(PORT.failing("updating an asset row"))?;

    transaction
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM asset_blobs WHERE asset_id = $1",
            [asset.clone()],
        ))
        .await
        .map_err(PORT.failing("clearing an asset's blobs"))?;
    for blob in &row.blobs {
        transaction
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO asset_blobs (asset_id, role, address, size) VALUES ($1, $2, $3, $4)",
                [
                    asset.clone(),
                    Value::from(blob.role.as_str().to_owned()),
                    Value::from(blob.address.as_str().to_owned()),
                    Value::from(size_to_column(blob.size)?),
                ],
            ))
            .await
            .map_err(PORT.failing("recording an asset's blob"))?;
    }

    transaction
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM asset_superseded WHERE asset_id = $1",
            [asset.clone()],
        ))
        .await
        .map_err(PORT.failing("clearing an asset's superseded manifests"))?;
    for (position, address) in row.superseded.iter().enumerate() {
        transaction
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO asset_superseded (asset_id, position, address) VALUES ($1, $2, $3)",
                [
                    asset.clone(),
                    Value::from(position as i64),
                    Value::from(address.as_str().to_owned()),
                ],
            ))
            .await
            .map_err(PORT.failing("recording a superseded manifest"))?;
    }

    Ok(())
}

/// Begin a transaction, or say why not.
async fn begin(connection: &DatabaseConnection) -> Result<DatabaseTransaction, StoreError> {
    connection
        .begin()
        .await
        .map_err(PORT.failing("opening a transaction"))
}

/// Commit, or say why not.
async fn commit(transaction: DatabaseTransaction) -> Result<(), StoreError> {
    transaction
        .commit()
        .await
        .map_err(PORT.failing("committing a transaction"))
}

impl AssetIndex for PostgresAssetIndex {
    fn reserve(&self, asset: PendingAsset) -> IndexFuture<'_, Reservation> {
        Box::pin(async move {
            // `ON CONFLICT DO NOTHING` rather than a read followed by a write: two sessions of
            // one bundle reserve unconditionally at creation, so this is the *normal* path and
            // a read-then-write would let both believe they created the row.
            let inserted = self
                .connection
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO assets (asset_id, owner_id, album_id, protocol_version, \
                     crypto_suite_id, state, amk_version, created_at, updated_at) \
                     VALUES ($1, $2, $3, $4, $5, 'pending', 0, $6, $6) \
                     ON CONFLICT (asset_id) DO NOTHING",
                    [
                        Value::from(asset.asset_id.as_str().to_owned()),
                        Value::from(asset.owner_id.as_str().to_owned()),
                        Value::from(asset.album_id.as_str().to_owned()),
                        Value::from(asset.protocol_version.clone()),
                        Value::from(i32::from(asset.crypto_suite_id)),
                        Value::from(to_micros(asset.created_at)),
                    ],
                ))
                .await
                .map_err(PORT.failing("reserving an asset row"))?;

            let Some(existing) = hydrate(&self.connection, &asset.asset_id).await? else {
                // Only reachable if the row was purged between the insert and the read back,
                // which no path in this crate does; treated as a refusal rather than a panic.
                return Err(StoreError::Rejected {
                    store: PORT.store,
                    detail: "the reserved row disappeared before it could be read back".to_owned(),
                });
            };
            if inserted.rows_affected() == 1 {
                tracing::debug!(asset = %asset.asset_id, "reserved a pending asset row");
                return Ok(Reservation::Created(Box::new(existing)));
            }

            let agrees = existing.owner_id == asset.owner_id
                && existing.album_id == asset.album_id
                && existing.protocol_version == asset.protocol_version
                && existing.crypto_suite_id == asset.crypto_suite_id;
            Ok(if agrees {
                Reservation::Joined(Box::new(existing))
            } else {
                // Carries nothing: the caller is by definition not the party the row belongs
                // to, and the asset id is client-chosen.
                Reservation::Conflict
            })
        })
    }

    fn read<'a>(&'a self, asset: &'a AssetId) -> IndexFuture<'a, Option<AssetRow>> {
        Box::pin(async move { hydrate(&self.connection, asset).await })
    }

    fn record_blob<'a>(
        &'a self,
        asset: &'a AssetId,
        blob: BlobRecord,
    ) -> IndexFuture<'a, BlobOutcome> {
        Box::pin(async move {
            let transaction = begin(&self.connection).await?;
            let Some(row) = hydrate_locked(&transaction, asset).await? else {
                return Ok(BlobOutcome::NotFound);
            };

            if row
                .blobs
                .iter()
                .any(|held| held.role == blob.role && held.address == blob.address)
            {
                // A retried finalization. Idempotent by address rather than by role, so a
                // genuine retry is free.
                return Ok(BlobOutcome::AlreadyHeld(Box::new(row)));
            }
            if is_singular(blob.role) && row.blobs.iter().any(|held| held.role == blob.role) {
                // Refused rather than overwritten: an upload that re-pointed a singular role
                // would swap bytes under a signature that still verifies against the old ones.
                return Ok(BlobOutcome::Conflict);
            }

            let mut row = row;
            row.blobs.push(BlobRef {
                role: blob.role,
                address: blob.address,
                size: blob.size,
            });
            row.blobs.sort();
            row.updated_at = blob.finalized_at;

            // The create's provenance blob is the asset's first accepted manifest, so it is the
            // chain the first lifecycle op must name (invariant 17). Set from the record's own
            // `manifest_sha256` and never from the content address (`S-C31`).
            if blob.role == BlobRole::Provenance
                && row.chain_head.is_none()
                && let Some(manifest_sha256) = blob.manifest_sha256
            {
                row.chain_head = Some(manifest_sha256);
            }

            let minted = if row.state == AssetState::Tombstoned {
                // A late blob for a deleted asset is stored — the bytes exist and GC must see
                // the reference — but publishes nothing.
                None
            } else if row.is_publishable() {
                let seq = mint(&transaction, &row.owner_id).await?;
                row.state = AssetState::Visible;
                row.sync_seq = Some(seq);
                row.first_seq = Some(row.first_seq.unwrap_or(seq));
                Some(seq)
            } else {
                None
            };

            persist(&transaction, &row).await?;
            commit(transaction).await?;
            if let Some(seq) = minted {
                tracing::info!(%asset, sync_seq = seq, "an asset became visible on its owner's feed");
            }
            Ok(BlobOutcome::Recorded {
                row: Box::new(row),
                minted,
            })
        })
    }

    fn tombstone<'a>(
        &'a self,
        asset: &'a AssetId,
        at: Timestamp,
    ) -> IndexFuture<'a, Option<AssetRow>> {
        Box::pin(async move {
            let transaction = begin(&self.connection).await?;
            let Some(mut row) = hydrate_locked(&transaction, asset).await? else {
                return Ok(None);
            };
            if row.state == AssetState::Tombstoned {
                return Ok(Some(row));
            }

            // A row nobody could see needs no retraction, so it takes no sequence number. It
            // still becomes terminal, so its id cannot be reserved back into life.
            let was_published = row.sync_seq.is_some();
            row.state = AssetState::Tombstoned;
            row.updated_at = at;
            if was_published {
                row.sync_seq = Some(mint(&transaction, &row.owner_id).await?);
            }
            persist(&transaction, &row).await?;
            commit(transaction).await?;
            tracing::info!(%asset, published = was_published, "an asset was tombstoned");
            Ok(Some(row))
        })
    }

    fn find_by_address<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
        address: &'a ContentAddress,
    ) -> IndexFuture<'a, Option<AssetId>> {
        Box::pin(async move {
            // Both scopes are load-bearing and for different reasons — owner is the disclosure
            // boundary, album is the merge contract. Ordered by asset id so the answer does not
            // depend on physical row order.
            let found = self
                .connection
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT a.asset_id FROM assets a \
                     JOIN asset_blobs b ON b.asset_id = a.asset_id \
                     WHERE a.owner_id = $1 AND a.album_id = $2 AND b.address = $3 \
                       AND a.state <> 'tombstoned' \
                     ORDER BY a.asset_id COLLATE \"C\" LIMIT 1",
                    [
                        Value::from(owner.as_str().to_owned()),
                        Value::from(album.as_str().to_owned()),
                        Value::from(address.as_str().to_owned()),
                    ],
                ))
                .await
                .map_err(PORT.failing("looking an address up in an album"))?;
            let Some(found) = found else { return Ok(None) };
            let asset_id: String = found
                .try_get("", "asset_id")
                .map_err(PORT.failing("reading an asset id"))?;
            Ok(Some(AssetId::new(asset_id)))
        })
    }

    fn find_reference<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> IndexFuture<'a, Option<BlobReference>> {
        Box::pin(async move {
            // Two statements rather than one ordered query: a **visible** reference outranks a
            // tombstoned one, and a pending row is not a reference at all. Content addressing
            // means two assets share a thumbnail, so deleting one must not take the other's
            // bytes with it.
            for state in ["visible", "tombstoned"] {
                let found = self
                    .connection
                    .query_one(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        format!(
                            "SELECT {ASSET_COLUMNS} FROM assets a \
                             WHERE a.state = $2 AND EXISTS ( \
                               SELECT 1 FROM asset_blobs b \
                               WHERE b.asset_id = a.asset_id AND b.address = $1) \
                             ORDER BY a.asset_id COLLATE \"C\" LIMIT 1"
                        ),
                        [
                            Value::from(address.as_str().to_owned()),
                            Value::from(state.to_owned()),
                        ],
                    ))
                    .await
                    .map_err(PORT.failing("looking an address up for the serving path"))?;
                let Some(found) = found else { continue };
                let mut row = asset_without_collections(&found)?;
                load_collections(&self.connection, &mut row).await?;
                return Ok(Some(BlobReference {
                    asset_id: row.asset_id.clone(),
                    owner_id: row.owner_id.clone(),
                    role: row
                        .blobs
                        .iter()
                        .find(|blob| &blob.address == address)
                        .map_or(BlobRole::Original, |blob| blob.role),
                    state: row.state,
                    original_held: row.original_held(),
                    hold: row.hold,
                }));
            }
            Ok(None)
        })
    }

    fn apply_op(&self, op: LifecycleOp) -> IndexFuture<'_, OpOutcome> {
        Box::pin(async move {
            let transaction = begin(&self.connection).await?;

            // Idempotency first, before any invariant: a byte-identical resubmission of an
            // already-applied op is not a stale chain, it is the same op arriving twice.
            // Checking 17 first would answer `409` to a client whose only fault was losing an
            // acknowledgement.
            let replayed = transaction
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT sync_seq FROM applied_manifests WHERE manifest_hash = $1",
                    [Value::from(op.manifest_hash.as_bytes().to_vec())],
                ))
                .await
                .map_err(PORT.failing("checking whether a manifest was already applied"))?;
            if let Some(replayed) = replayed {
                let sync_seq: i64 = replayed
                    .try_get("", "sync_seq")
                    .map_err(PORT.failing("reading a replayed sequence number"))?;
                tracing::info!(
                    asset = %op.asset_id,
                    action = op.action.as_str(),
                    "a lifecycle write was replayed; nothing was written"
                );
                return Ok(OpOutcome::Replayed {
                    sync_seq: sequence_from(sync_seq)?,
                });
            }

            let Some(row) = hydrate_locked(&transaction, &op.asset_id).await? else {
                return Ok(OpOutcome::NotFound);
            };
            // Not this caller's asset, or not in the album the op was addressed to. Both are
            // the same answer, and neither is distinguishable from an asset that never existed.
            if row.owner_id != op.owner_id || row.album_id != op.album_id {
                tracing::info!(
                    asset = %op.asset_id,
                    "a lifecycle write was refused: the asset is not this caller's"
                );
                return Ok(OpOutcome::NotFound);
            }
            // A row nothing can see yet has no chain to extend.
            if row.state == AssetState::Pending {
                return Ok(OpOutcome::NotFound);
            }

            // Invariant 17, decided under the row lock rather than by the caller: a
            // read-compare-write above this port has a window in which two ops chain onto the
            // same head, which is the double-apply the invariant exists to catch.
            if op.prior_provenance_hash != row.chain_head {
                tracing::info!(
                    asset = %op.asset_id,
                    action = op.action.as_str(),
                    "a lifecycle write was refused: it does not chain onto the stored head"
                );
                return Ok(OpOutcome::StaleChain {
                    head: row.chain_head,
                });
            }

            // Invariant 18, over the **album's** high-water mark rather than this row's: an
            // epoch is an album-wide fact, so an op on a stale asset must not re-admit an epoch
            // the album has already moved past.
            let epoch = transaction
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT COALESCE(MAX(amk_version), 0) AS stored FROM assets WHERE album_id = $1",
                    [Value::from(op.album_id.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("reading an album's epoch"))?
                .ok_or_else(|| StoreError::Rejected {
                    store: PORT.store,
                    detail: "the album epoch query returned no row".to_owned(),
                })?;
            let stored: i64 = epoch
                .try_get("", "stored")
                .map_err(PORT.failing("reading an album's epoch"))?;
            let stored = sequence_from(stored)?;
            if op.amk_version < stored {
                tracing::info!(
                    asset = %op.asset_id,
                    stored,
                    submitted = op.amk_version,
                    "a lifecycle write was refused: the album epoch regresses"
                );
                return Ok(OpOutcome::AmkRegressed { stored });
            }

            let mut row = row;
            row.state = match op.action {
                OpAction::Delete => AssetState::Tombstoned,
                // A restore returns the asset to the live set. Every other action leaves the
                // state alone — re-uploading the bytes of something you deleted does not
                // undelete it, because undeleting is what a `trash-restore` is for.
                OpAction::TrashRestore => AssetState::Visible,
                OpAction::MetadataUpdate | OpAction::Derivative | OpAction::Replace => row.state,
            };
            // The provenance blob is re-pointed on every op: the chain *is* a succession of
            // manifests, so the newest one is what the feed must serve.
            set_singular(&mut row, BlobRole::Provenance, &op.provenance);
            if let Some(metadata) = &op.metadata {
                set_singular(&mut row, BlobRole::Metadata, metadata);
            }
            // A replace's whole point (`S-C43`): the authorized form of the change
            // `record_blob` refuses, arriving with a manifest that chains onto the one it
            // supersedes.
            if let Some(original) = &op.original {
                set_singular(&mut row, BlobRole::Original, original);
            }
            row.chain_head = Some(op.manifest_hash);
            row.amk_version = op.amk_version;
            row.retention_until = match op.action {
                OpAction::Delete => op.retention_until,
                // Back in the live set: there is no window left to run out.
                OpAction::TrashRestore => None,
                OpAction::MetadataUpdate | OpAction::Derivative | OpAction::Replace => {
                    row.retention_until
                }
            };
            row.updated_at = op.at;

            let sync_seq = mint(&transaction, &row.owner_id).await?;
            row.sync_seq = Some(sync_seq);
            transaction
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO applied_manifests (manifest_hash, sync_seq) VALUES ($1, $2)",
                    [
                        Value::from(op.manifest_hash.as_bytes().to_vec()),
                        Value::from(sync_seq as i64),
                    ],
                ))
                .await
                .map_err(PORT.failing("recording an applied manifest"))?;
            persist(&transaction, &row).await?;
            commit(transaction).await?;

            tracing::info!(
                asset = %op.asset_id,
                action = op.action.as_str(),
                sync_seq,
                "a lifecycle write was applied"
            );
            Ok(OpOutcome::Applied {
                row: Box::new(row),
                sync_seq,
            })
        })
    }

    fn set_hold<'a>(
        &'a self,
        asset: &'a AssetId,
        hold: Option<ServingHold>,
    ) -> IndexFuture<'a, HoldOutcome> {
        Box::pin(async move {
            // `IS DISTINCT FROM` rather than `<>` so a null-to-null no-op is `Unchanged` rather
            // than an update nobody can see: re-applying a takedown must not append a second
            // provenance record claiming the asset was taken down twice.
            let applied = self
                .connection
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE assets SET hold = $2 WHERE asset_id = $1 AND hold IS DISTINCT FROM $2",
                    [
                        Value::from(asset.as_str().to_owned()),
                        Value::from(hold.map(|hold| hold.as_str().to_owned())),
                    ],
                ))
                .await
                .map_err(PORT.failing("placing a serving hold"))?;
            if applied.rows_affected() == 1 {
                if let Some(hold) = hold {
                    tracing::info!(
                        %asset,
                        hold = hold.as_str(),
                        "an asset was placed under a serving hold; its bytes are untouched"
                    );
                } else {
                    tracing::info!(%asset, "an asset's serving hold was lifted");
                }
                return Ok(HoldOutcome::Applied);
            }

            let exists = self
                .connection
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT 1 AS present FROM assets WHERE asset_id = $1",
                    [Value::from(asset.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("reading an asset row"))?;
            Ok(if exists.is_some() {
                HoldOutcome::Unchanged
            } else {
                HoldOutcome::NotFound
            })
        })
    }

    fn reference_count<'a>(&'a self, address: &'a ContentAddress) -> IndexFuture<'a, u64> {
        Box::pin(async move {
            // A **query**, never a stored counter. A manifest the chain has moved past is still
            // referenced (`S-C52`); without that the collector reclaims the server's own
            // rebuttal evidence.
            let counted = self
                .connection
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT COUNT(*) AS held FROM assets a WHERE \
                       EXISTS (SELECT 1 FROM asset_blobs b \
                               WHERE b.asset_id = a.asset_id AND b.address = $1) \
                       OR EXISTS (SELECT 1 FROM asset_superseded s \
                                  WHERE s.asset_id = a.asset_id AND s.address = $1)",
                    [Value::from(address.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("counting the rows that name an address"))?
                .ok_or_else(|| StoreError::Rejected {
                    store: PORT.store,
                    detail: "the reference count returned no row".to_owned(),
                })?;
            let held: i64 = counted
                .try_get("", "held")
                .map_err(PORT.failing("reading a reference count"))?;
            sequence_from(held)
        })
    }

    fn rows<'a>(
        &'a self,
        after: Option<&'a AssetId>,
        limit: usize,
    ) -> IndexFuture<'a, Vec<AssetRow>> {
        Box::pin(async move {
            // Every row, whatever its state: a scrub that skipped pending or tombstoned rows
            // would skip exactly the rows a half-finished write leaves behind.
            //
            // `COLLATE "C"` is byte order, which is the in-memory adapter's `BTreeMap` order —
            // so "asset-id order" means one thing for both adapters rather than whatever the
            // database was initialised with. A locale collation would also be *self*-consistent
            // here (the cursor comparison and the ordering share it), so this is about adapter
            // parity rather than about a resumable walk: en_US.utf8 ignores punctuation at the
            // primary level, and asset ids are client-chosen and full of hyphens.
            let statement = match after {
                Some(after) => Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    format!(
                        "SELECT {ASSET_COLUMNS} FROM assets \
                         WHERE asset_id COLLATE \"C\" > $1 \
                         ORDER BY asset_id COLLATE \"C\" LIMIT $2"
                    ),
                    [
                        Value::from(after.as_str().to_owned()),
                        Value::from(limit as i64),
                    ],
                ),
                None => Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    format!(
                        "SELECT {ASSET_COLUMNS} FROM assets ORDER BY asset_id COLLATE \"C\" \
                         LIMIT $1"
                    ),
                    [Value::from(limit as i64)],
                ),
            };
            let found = self
                .connection
                .query_all(statement)
                .await
                .map_err(PORT.failing("walking the asset rows"))?;
            let mut rows = found
                .iter()
                .map(asset_without_collections)
                .collect::<Result<Vec<_>, _>>()?;
            load_all_collections(&self.connection, &mut rows).await?;
            Ok(rows)
        })
    }

    fn tombstoned(&self, limit: usize) -> IndexFuture<'_, Vec<AssetRow>> {
        Box::pin(async move {
            // Oldest change first, so a bounded pass makes progress on the oldest deletions
            // rather than revisiting the same page. The asset id breaks ties so the order is
            // total and a resumed pass is deterministic.
            let found = self
                .connection
                .query_all(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    format!(
                        "SELECT {ASSET_COLUMNS} FROM assets WHERE state = 'tombstoned' \
                         ORDER BY updated_at, asset_id COLLATE \"C\" LIMIT $1"
                    ),
                    [Value::from(limit as i64)],
                ))
                .await
                .map_err(PORT.failing("listing tombstoned rows"))?;
            let mut rows = found
                .iter()
                .map(asset_without_collections)
                .collect::<Result<Vec<_>, _>>()?;
            load_all_collections(&self.connection, &mut rows).await?;
            Ok(rows)
        })
    }

    fn purge<'a>(&'a self, asset: &'a AssetId) -> IndexFuture<'a, Option<AssetRow>> {
        Box::pin(async move {
            let transaction = begin(&self.connection).await?;
            let Some(mut row) = hydrate_locked(&transaction, asset).await? else {
                return Ok(None);
            };
            // The row **stays**. A client that has not synced since the delete still has to
            // learn about it, so removing the row would make the deletion invisible rather than
            // final. The chain goes with the bytes it describes: a purge is the end of the
            // retention window the user's own signed delete fixed (`S-C52`).
            row.blobs.clear();
            row.superseded.clear();
            persist(&transaction, &row).await?;
            commit(transaction).await?;
            tracing::info!(%asset, "purged a tombstoned asset's blob references");
            Ok(Some(row))
        })
    }

    fn feed_page<'a>(
        &'a self,
        owner: &'a OwnerId,
        after: u64,
        limit: usize,
    ) -> IndexFuture<'a, Vec<FeedEntry>> {
        Box::pin(async move {
            let found = self
                .connection
                .query_all(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    format!(
                        "SELECT {ASSET_COLUMNS} FROM assets \
                         WHERE owner_id = $1 AND sync_seq > $2 \
                         ORDER BY sync_seq LIMIT $3"
                    ),
                    [
                        Value::from(owner.as_str().to_owned()),
                        Value::from(after as i64),
                        Value::from(limit as i64),
                    ],
                ))
                .await
                .map_err(PORT.failing("reading a feed page"))?;
            let mut rows = found
                .iter()
                .map(asset_without_collections)
                .collect::<Result<Vec<_>, _>>()?;
            load_all_collections(&self.connection, &mut rows).await?;
            // `entry_for` is shared with the in-memory adapter so both render an entry
            // identically — the `ChangeKind` rule in particular is the kind of thing two
            // adapters drift on.
            Ok(rows
                .iter()
                .filter_map(|row| entry_for(row, after))
                .collect())
        })
    }

    fn head_seq<'a>(&'a self, owner: &'a OwnerId) -> IndexFuture<'a, u64> {
        Box::pin(async move {
            // The allocator's own row: the highest number this owner has minted, which is what
            // lets a page report whether the client is caught up without asking for another
            // page that would come back empty.
            let found = self
                .connection
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT next_seq FROM owner_sequences WHERE owner_id = $1",
                    [Value::from(owner.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("reading an owner's head sequence number"))?;
            let Some(found) = found else { return Ok(0) };
            let next: i64 = found
                .try_get("", "next_seq")
                .map_err(PORT.failing("reading an owner's head sequence number"))?;
            sequence_from(next)
        })
    }
}

#[cfg(test)]
mod tests {
    /// The suite, against a real Postgres.
    ///
    /// One case, running the whole list in one pass, because nextest runs a process per test and
    /// a container per case would be thirty containers rather than one. `index/conformance.rs`
    /// said as much before this adapter existed: *"a Postgres-backed smoke test is one `run_all`
    /// call"*.
    mod postgres_conformance {
        use super::super::PostgresAssetIndex;
        use crate::index::conformance;
        use crate::postgres::testing;

        #[tokio::test]
        async fn the_postgres_index_conforms() {
            let Some(database) = testing::start("the Postgres asset index").await else {
                return;
            };
            let index = PostgresAssetIndex::new(database.connection().clone());
            conformance::run_all(&index).await;
        }
    }
}
