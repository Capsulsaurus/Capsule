//! The sync-fed local store: the durable half of `capsule sync`.
//!
//! `capsule-sdk`'s [`SyncState`] is the *pure* anti-rewind / forward-version state
//! machine; it is network-free and, by itself, in-memory. This
//! module is the CLI's durable backing for it: it rehydrates a `SyncState` from the
//! CLI's SQLite (the opaque cursor plus the per-album high-water marks, the latter
//! *derived* from `MAX(sync_seq)` so every applied entry re-establishes the mark),
//! and it lands each validated [`SyncPage`] into the `synced_assets` table that
//! `capsule list` queries client-side.
//!
//! Persistence is validate-then-apply, mirroring the SDK: the caller applies a page
//! to the in-memory `SyncState` first (which enforces anti-rewind), and only a page
//! that passed is handed here to persist. Each page is written in one transaction —
//! all its assets plus the advanced cursor — so a crash never leaves the cursor
//! ahead of the assets it names.

use std::collections::HashMap;

use base64::Engine as _;
use capsule_core::crypto::provenance::{Action, ProvenanceRecord};
use capsule_sdk::sync::{ChangeKind, FeedEntry, SyncCursor, SyncPage, SyncState};
use jiff::Timestamp;
use rusqlite::OptionalExtension;
use thiserror::Error;
use tracing::instrument;
use uuid::Uuid;

use crate::db::{DbError, LocalDb};
use crate::peering_state::{feed_asset_uuid, record_purged};

/// The singleton row id for the one opaque feed cursor.
const CURSOR_ROW_ID: i64 = 0;

/// Everything the sync store can fail with.
#[derive(Debug, Error)]
pub enum SyncStoreError {
    /// A database operation failed.
    #[error("sync store database error: {0}")]
    Db(#[from] DbError),
    /// A stored id was not valid base64 (store corruption).
    #[error("corrupt stored id {value:?}: {source}")]
    CorruptId {
        /// The offending stored value.
        value: String,
        /// The decode error.
        source: base64::DecodeError,
    },
}

/// Base64 the feed's opaque id bytes for lossless text storage.
fn encode_id(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Recover the feed's opaque id bytes from their stored base64 form.
pub(crate) fn decode_id(value: &str) -> Result<Vec<u8>, SyncStoreError> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|source| SyncStoreError::CorruptId {
            value: value.to_string(),
            source,
        })
}

/// The stable string form of a change kind, as stored and displayed.
fn kind_label(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Created => "created",
        ChangeKind::Updated => "metadata_updated",
        ChangeKind::Deleted => "deleted",
    }
}

/// Rehydrate a [`SyncState`] from the durable store, pinned to the running build's
/// `max_known_protocol`. A fresh store yields the first-sync sentinel.
pub async fn load_sync_state(
    db: &LocalDb,
    max_known_protocol: &str,
) -> Result<SyncState, SyncStoreError> {
    let (cursor, albums) = db
        .call(|conn| {
            let cursor: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT cursor FROM sync_cursor WHERE id = ?1",
                    [CURSOR_ROW_ID],
                    |row| row.get(0),
                )
                .optional()?;
            let mut stmt = conn.prepare("SELECT album_id, sync_seq FROM synced_assets")?;
            let albums = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok((cursor, albums))
        })
        .await?;
    let cursor = cursor.map_or_else(SyncCursor::start, SyncCursor::from_bytes);

    // The per-album high-water mark is MAX(sync_seq) — every applied entry was
    // recorded, so this reconstructs the anti-rewind floor exactly.
    let mut high_water: HashMap<Vec<u8>, u64> = HashMap::new();
    for (album_id, sync_seq) in albums {
        let album = decode_id(&album_id)?;
        let seq = sync_seq as u64;
        let slot = high_water.entry(album).or_insert(0);
        *slot = (*slot).max(seq);
    }

    Ok(SyncState::restore(max_known_protocol, cursor, high_water))
}

/// Start the durable sync state over after the server moved the library to a new sync epoch
/// (#697): the stored cursor is dropped and every synced asset's `sync_seq` is zeroed, in one
/// transaction.
///
/// The rows stay — nothing the client holds is discarded — but the anti-rewind floor
/// [`load_sync_state`] derives from them goes, for the reason [`SyncState::reset_for_resync`]
/// drops its in-memory marks: the rebuilt numbering may sit below them. The resync that follows
/// re-delivers every asset the server holds and writes its new number back.
pub async fn reset_for_resync(db: &LocalDb) -> Result<(), SyncStoreError> {
    db.call(|conn| {
        let txn = conn.transaction()?;
        txn.execute("DELETE FROM sync_cursor WHERE id = ?1", [CURSOR_ROW_ID])?;
        txn.execute("UPDATE synced_assets SET sync_seq = 0", [])?;
        txn.commit()
    })
    .await?;
    tracing::info!("the sync cursor and high-water marks were reset for a new sync epoch");
    Ok(())
}

/// What a feed entry says about the asset's retention window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Retention {
    /// The asset is live, or its delete names no deadline this device can read: nothing to
    /// purge on.
    Open,
    /// The asset is in trash until this deadline, in unix seconds rounded up.
    Until(i64),
    /// The server says it has purged the asset: its window has closed whatever any clock says.
    Purged,
}

/// The deadline stored for a [`Retention::Purged`] asset: one every judging time has passed, so
/// the sweep ([`record_elapsed_purges`]) records it on its next run, like any closed window.
const PURGED_DEADLINE: i64 = i64::MIN;

/// The retention window `entry` names. Only a `Deleted` entry has one.
///
/// A tombstone carrying the server's purge signal is [`Retention::Purged`] on that signal alone
/// (#885): the server purges only once the window has closed, so this is how a device that first
/// syncs after a purge, and so never saw the delete record, records its tombstone.
///
/// Otherwise the server serves a tombstone with its signed delete record as the manifest, whose
/// `retention_until` is the deadline after which the server refuses a `trash-restore`
/// ([Organization § Retention Window]). A delete with no manifest, or whose record does not
/// decode, is not a delete of this asset, or names no readable deadline is [`Retention::Open`]:
/// a tombstone is permanent, so a delete this device cannot read is left in trash rather than
/// recorded. That includes every tombstone from a server that predates serving the record or the
/// signal, and one whose record the server could not produce, since none is positive evidence of
/// a purge.
///
/// [Organization § Retention Window]: https://docs/design/organization/#retention-window
fn retention_of(entry: &FeedEntry) -> Retention {
    if entry.kind != ChangeKind::Deleted {
        return Retention::Open;
    }
    if entry.purged {
        tracing::debug!("sync: the server has purged a deleted asset; its window has closed");
        return Retention::Purged;
    }
    if entry.manifest_cbor.is_empty() {
        tracing::debug!("sync: a delete carries no manifest; it is held in trash with no deadline");
        return Retention::Open;
    }
    let record = match ProvenanceRecord::decode(&entry.manifest_cbor) {
        Ok(record) => record,
        Err(error) => {
            tracing::warn!(%error, "sync: a delete's manifest does not decode; it is held in trash with no deadline");
            return Retention::Open;
        }
    };
    let core = &record.manifest.core;
    if core.action != Action::Delete || feed_asset_uuid(&entry.asset_id) != Some(record.asset_id) {
        tracing::warn!(asset = %record.asset_id, action = ?core.action, "sync: a delete's manifest is not a delete of this asset; it is held in trash with no deadline");
        return Retention::Open;
    }
    let Some(until) = core.retention_until.as_deref() else {
        return Retention::Open;
    };
    match until.parse::<Timestamp>() {
        Ok(until) => Retention::Until(ceil_second(until)),
        Err(error) => {
            tracing::warn!(asset = %record.asset_id, until, %error, "sync: a delete's retention_until does not parse; it is held in trash with no deadline");
            Retention::Open
        }
    }
}

/// `at` in unix seconds, rounded up: a deadline read this way never passes early.
fn ceil_second(at: Timestamp) -> i64 {
    let second = at.as_second();
    if at.subsec_nanosecond() > 0 {
        second.saturating_add(1)
    } else {
        second
    }
}

/// The latest `changed_at` among `page`'s entries, or `None` when none parses. A `changed_at`
/// is a reading of the server's clock, the one that refuses a late `trash-restore`, and the
/// latest one a drain saw is the only instant retention deadlines are judged at
/// ([`record_elapsed_purges`]): never this device's clock, which running ahead would record a
/// purge early and then refuse a peer's copy of an asset the server restored in time.
#[must_use]
pub fn latest_changed_at(page: &SyncPage) -> Option<Timestamp> {
    page.entries
        .iter()
        .filter_map(|entry| entry.changed_at.parse::<Timestamp>().ok())
        .max()
}

/// Land a validated page: upsert every asset and advance the stored cursor, in one transaction.
/// Returns the number of entries written.
///
/// A `Deleted` entry marks the asset tombstoned and stores its retention deadline, the
/// `retention_until` of its delete record: a delete moves the asset to trash, which a
/// `trash-restore` reverses until that deadline, so the delete alone is not a peering tombstone.
/// A tombstone carrying the server's purge signal stores a deadline that has always passed.
/// Any other entry clears the deadline, which is how a restore takes the asset out of the sweep.
/// Landing a page records no purge: a restore of the same asset may sit on a later page, so the
/// sweep ([`record_elapsed_purges`]) runs only once the drain has reached the feed's head.
#[instrument(level = "debug", skip_all, fields(entries = page.entries.len()), err)]
pub async fn persist_page(db: &LocalDb, page: &SyncPage) -> Result<usize, SyncStoreError> {
    let rows: Vec<SyncedAssetRow> = page
        .entries
        .iter()
        .map(|entry| SyncedAssetRow {
            asset_id: encode_id(&entry.asset_id),
            album_id: encode_id(&entry.album_id),
            sync_seq: entry.sync_seq as i64,
            kind: kind_label(entry.kind).to_string(),
            original_held: entry.original_held,
            tombstoned: entry.kind == ChangeKind::Deleted,
            retention_until: match retention_of(entry) {
                Retention::Open => None,
                Retention::Until(until) => Some(until),
                Retention::Purged => Some(PURGED_DEADLINE),
            },
        })
        .collect();
    let cursor = page.next_cursor.as_bytes().to_vec();

    db.call(move |conn| {
        let txn = conn.transaction()?;
        {
            let mut upsert = txn.prepare(
                "INSERT INTO synced_assets \
                 (asset_id, album_id, sync_seq, kind, original_held, tombstoned, retention_until) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT (asset_id) DO UPDATE SET \
                 album_id = excluded.album_id, sync_seq = excluded.sync_seq, \
                 kind = excluded.kind, original_held = excluded.original_held, \
                 tombstoned = excluded.tombstoned, retention_until = excluded.retention_until",
            )?;
            for row in &rows {
                upsert.execute(rusqlite::params![
                    row.asset_id,
                    row.album_id,
                    row.sync_seq,
                    row.kind,
                    row.original_held,
                    row.tombstoned,
                    row.retention_until,
                ])?;
            }
        }
        txn.execute(
            "INSERT INTO sync_cursor (id, cursor) VALUES (?1, ?2) \
             ON CONFLICT (id) DO UPDATE SET cursor = excluded.cursor",
            rusqlite::params![CURSOR_ROW_ID, cursor],
        )?;
        txn.commit()
    })
    .await?;
    Ok(page.entries.len())
}

/// Record as purged every asset in trash whose retention deadline has passed at `at`, and return
/// how many there are. The server refuses a `trash-restore` at or after the deadline, by its own
/// clock, and a purge ends the asset, so once the deadline has passed nothing brings the asset
/// back and no peer may ([`record_purged`]). An asset whose id is not a UUID is skipped: no peer
/// can name it. A swept row's deadline is cleared once its tombstone is written, so each purge is
/// recorded once; recording an asset again would keep its first record anyway, so the sweep is
/// safe to repeat. A sync run runs it once the drain has reached the feed's head, and only
/// then: a delete on one page may be restored on a later one, and the restore clears the
/// deadline only when that page lands. `at` is the latest server `changed_at` the drain saw
/// ([`latest_changed_at`]), so a run that landed no entry has no reading to judge by and does
/// not sweep: a window the server's clock has closed is recorded by the next drain that lands
/// one, and a late record loses nothing.
///
/// # Errors
///
/// [`SyncStoreError::Db`] when the store cannot be read or written,
/// [`SyncStoreError::CorruptId`] when a stored id does not decode.
#[instrument(level = "debug", skip(db), err)]
pub async fn record_elapsed_purges(db: &LocalDb, at: Timestamp) -> Result<usize, SyncStoreError> {
    let judged_at = at.as_second();
    let rows: Vec<String> = db
        .call(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT asset_id FROM synced_assets \
                 WHERE tombstoned = 1 AND retention_until <= ?1",
            )?;
            stmt.query_map([judged_at], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()
        })
        .await?;
    let mut elapsed: Vec<Uuid> = Vec::with_capacity(rows.len());
    for asset_id in &rows {
        if let Some(asset) = feed_asset_uuid(&decode_id(asset_id)?) {
            elapsed.push(asset);
        } else {
            tracing::debug!(
                %asset_id,
                "sync: an elapsed asset's id is not a UUID, so no peer can offer it"
            );
        }
    }
    if !elapsed.is_empty() {
        record_purged(db, &elapsed, at).await?;
    }
    if !rows.is_empty() {
        // Recorded, so off the sweep: the next run does not record them again. Cleared after the
        // record, so a crash between the two only repeats a record that keeps its first stamp.
        db.call(move |conn| {
            conn.execute(
                "UPDATE synced_assets SET retention_until = NULL \
                 WHERE tombstoned = 1 AND retention_until <= ?1",
                [judged_at],
            )
        })
        .await?;
    }
    tracing::debug!(
        %at,
        elapsed = elapsed.len(),
        "sync: elapsed retention windows are recorded as purges"
    );
    Ok(elapsed.len())
}

/// A `synced_assets` row as stored: ids in their base64 text form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyncedAssetRow {
    pub(crate) asset_id: String,
    pub(crate) album_id: String,
    pub(crate) sync_seq: i64,
    pub(crate) kind: String,
    pub(crate) original_held: bool,
    pub(crate) tombstoned: bool,
    pub(crate) retention_until: Option<i64>,
}

/// Run `sql`, a `SELECT *` over `synced_assets`, and read each row by column name.
pub(crate) fn query_synced_assets<P: rusqlite::Params>(
    conn: &rusqlite::Connection,
    sql: &str,
    params: P,
) -> rusqlite::Result<Vec<SyncedAssetRow>> {
    let mut stmt = conn.prepare(sql)?;
    stmt.query_map(params, |row| {
        Ok(SyncedAssetRow {
            asset_id: row.get("asset_id")?,
            album_id: row.get("album_id")?,
            sync_seq: row.get("sync_seq")?,
            kind: row.get("kind")?,
            original_held: row.get("original_held")?,
            tombstoned: row.get("tombstoned")?,
            retention_until: row.get("retention_until")?,
        })
    })?
    .collect()
}

/// A row of the sync-fed local store, decoded back to feed-native shapes.
#[derive(Debug, Clone)]
pub struct SyncedAssetView {
    /// The asset id (opaque feed bytes).
    pub asset_id: Vec<u8>,
    /// The owning album id (opaque feed bytes).
    pub album_id: Vec<u8>,
    /// The per-album feed sequence.
    pub sync_seq: u64,
    /// The stored change-kind label.
    pub kind: String,
    /// Whether the original blob is finalized server-side.
    pub original_held: bool,
    /// Whether the asset is tombstoned.
    pub tombstoned: bool,
}

/// Query the sync-fed local store, ordered by album then sequence. Tombstoned
/// assets are included only when `include_tombstoned` is set.
pub async fn list_assets(
    db: &LocalDb,
    include_tombstoned: bool,
) -> Result<Vec<SyncedAssetView>, SyncStoreError> {
    let rows = db
        .call(move |conn| {
            let sql = if include_tombstoned {
                "SELECT * FROM synced_assets ORDER BY album_id, sync_seq"
            } else {
                "SELECT * FROM synced_assets WHERE tombstoned = 0 ORDER BY album_id, sync_seq"
            };
            query_synced_assets(conn, sql, [])
        })
        .await?;

    rows.into_iter()
        .map(|row| {
            Ok(SyncedAssetView {
                asset_id: decode_id(&row.asset_id)?,
                album_id: decode_id(&row.album_id)?,
                sync_seq: row.sync_seq as u64,
                kind: row.kind,
                original_held: row.original_held,
                tombstoned: row.tombstoned,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
