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
use entity::{sync_cursor, synced_asset};
use jiff::Timestamp;
use sea_orm::ActiveValue::Set;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder, TransactionSession,
    TransactionTrait,
};
use thiserror::Error;
use tracing::instrument;
use uuid::Uuid;

use crate::peering_state::{feed_asset_uuid, record_purged};

/// The singleton row id for the one opaque feed cursor.
const CURSOR_ROW_ID: i32 = 0;

/// Everything the sync store can fail with.
#[derive(Debug, Error)]
pub enum SyncStoreError {
    /// A database operation failed.
    #[error("sync store database error: {0}")]
    Db(#[from] DbErr),
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
pub async fn load_sync_state<C: ConnectionTrait>(
    db: &C,
    max_known_protocol: &str,
) -> Result<SyncState, SyncStoreError> {
    let cursor = sync_cursor::Entity::find_by_id(CURSOR_ROW_ID)
        .one(db)
        .await?
        .map_or_else(SyncCursor::start, |row| SyncCursor::from_bytes(row.cursor));

    // The per-album high-water mark is MAX(sync_seq) — every applied entry was
    // recorded, so this reconstructs the anti-rewind floor exactly.
    let mut high_water: HashMap<Vec<u8>, u64> = HashMap::new();
    for row in synced_asset::Entity::find().all(db).await? {
        let album = decode_id(&row.album_id)?;
        let seq = row.sync_seq as u64;
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
pub async fn reset_for_resync<C: ConnectionTrait + TransactionTrait>(
    db: &C,
) -> Result<(), SyncStoreError> {
    let txn = db.begin().await?;
    sync_cursor::Entity::delete_by_id(CURSOR_ROW_ID)
        .exec(&txn)
        .await?;
    synced_asset::Entity::update_many()
        .col_expr(
            synced_asset::Column::SyncSeq,
            sea_orm::sea_query::Expr::value(0_i64),
        )
        .exec(&txn)
        .await?;
    txn.commit().await?;
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
/// is a reading of the server's clock, the one that refuses a late `trash-restore`.
#[must_use]
pub fn latest_changed_at(page: &SyncPage) -> Option<Timestamp> {
    page.entries
        .iter()
        .filter_map(|entry| entry.changed_at.parse::<Timestamp>().ok())
        .max()
}

/// The time retention deadlines are judged at: the device's clock `now`, floored by `server`,
/// the latest server clock reading the drain saw ([`latest_changed_at`]). A deadline the server's
/// clock has passed has passed on the server too, however far behind this device's clock is.
#[must_use]
pub fn judged_at(now: Timestamp, server: Option<Timestamp>) -> Timestamp {
    server.map_or(now, |server| server.max(now))
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
pub async fn persist_page<C: ConnectionTrait + TransactionTrait>(
    db: &C,
    page: &SyncPage,
) -> Result<usize, SyncStoreError> {
    let txn = db.begin().await?;

    for entry in &page.entries {
        let retention_until = match retention_of(entry) {
            Retention::Open => None,
            Retention::Until(until) => Some(until),
            Retention::Purged => Some(PURGED_DEADLINE),
        };
        let model = synced_asset::ActiveModel {
            asset_id: Set(encode_id(&entry.asset_id)),
            album_id: Set(encode_id(&entry.album_id)),
            sync_seq: Set(entry.sync_seq as i64),
            kind: Set(kind_label(entry.kind).to_string()),
            original_held: Set(entry.original_held),
            tombstoned: Set(entry.kind == ChangeKind::Deleted),
            retention_until: Set(retention_until),
        };
        synced_asset::Entity::insert(model)
            .on_conflict(
                OnConflict::column(synced_asset::Column::AssetId)
                    .update_columns([
                        synced_asset::Column::AlbumId,
                        synced_asset::Column::SyncSeq,
                        synced_asset::Column::Kind,
                        synced_asset::Column::OriginalHeld,
                        synced_asset::Column::Tombstoned,
                        synced_asset::Column::RetentionUntil,
                    ])
                    .to_owned(),
            )
            .exec(&txn)
            .await?;
    }

    sync_cursor::Entity::insert(sync_cursor::ActiveModel {
        id: Set(CURSOR_ROW_ID),
        cursor: Set(page.next_cursor.as_bytes().to_vec()),
    })
    .on_conflict(
        OnConflict::column(sync_cursor::Column::Id)
            .update_column(sync_cursor::Column::Cursor)
            .to_owned(),
    )
    .exec(&txn)
    .await?;

    txn.commit().await?;
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
/// deadline only when that page lands. It runs on a run that landed no page too, since a window
/// closes on the clock and not on a feed entry.
///
/// # Errors
///
/// [`SyncStoreError::Db`] when the store cannot be read or written,
/// [`SyncStoreError::CorruptId`] when a stored id does not decode.
#[instrument(level = "debug", skip(db), err)]
pub async fn record_elapsed_purges<C: ConnectionTrait>(
    db: &C,
    at: Timestamp,
) -> Result<usize, SyncStoreError> {
    let rows = synced_asset::Entity::find()
        .filter(synced_asset::Column::Tombstoned.eq(true))
        .filter(synced_asset::Column::RetentionUntil.lte(at.as_second()))
        .all(db)
        .await?;
    let mut elapsed: Vec<Uuid> = Vec::with_capacity(rows.len());
    for row in &rows {
        if let Some(asset) = feed_asset_uuid(&decode_id(&row.asset_id)?) {
            elapsed.push(asset);
        } else {
            tracing::debug!(
                asset_id = %row.asset_id,
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
        synced_asset::Entity::update_many()
            .col_expr(
                synced_asset::Column::RetentionUntil,
                sea_orm::sea_query::Expr::value(Option::<i64>::None),
            )
            .filter(synced_asset::Column::Tombstoned.eq(true))
            .filter(synced_asset::Column::RetentionUntil.lte(at.as_second()))
            .exec(db)
            .await?;
    }
    tracing::debug!(
        %at,
        elapsed = elapsed.len(),
        "sync: elapsed retention windows are recorded as purges"
    );
    Ok(elapsed.len())
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
pub async fn list_assets<C: ConnectionTrait>(
    db: &C,
    include_tombstoned: bool,
) -> Result<Vec<SyncedAssetView>, SyncStoreError> {
    let mut query = synced_asset::Entity::find();
    if !include_tombstoned {
        query = query.filter(synced_asset::Column::Tombstoned.eq(false));
    }
    let rows = query
        .order_by_asc(synced_asset::Column::AlbumId)
        .order_by_asc(synced_asset::Column::SyncSeq)
        .all(db)
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
