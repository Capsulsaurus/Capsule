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
use capsule_sdk::sync::{ChangeKind, SyncCursor, SyncPage, SyncState};
use entity::{sync_cursor, synced_asset};
use sea_orm::ActiveValue::Set;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder, TransactionSession,
    TransactionTrait,
};
use thiserror::Error;

use crate::peering_state;

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

/// Land a validated page: upsert every asset, record a peering tombstone for every delete
/// ([`crate::peering_state`]), and advance the stored cursor, all in one transaction. Returns the
/// number of entries written.
pub async fn persist_page<C: ConnectionTrait + TransactionTrait>(
    db: &C,
    page: &SyncPage,
) -> Result<usize, SyncStoreError> {
    let txn = db.begin().await?;

    let deleted: Vec<uuid::Uuid> = page
        .entries
        .iter()
        .filter(|entry| entry.kind == ChangeKind::Deleted)
        .filter_map(|entry| peering_state::feed_asset_uuid(&entry.asset_id))
        .collect();
    peering_state::record_tombstones(&txn, &deleted, jiff::Timestamp::now()).await?;

    for entry in &page.entries {
        let model = synced_asset::ActiveModel {
            asset_id: Set(encode_id(&entry.asset_id)),
            album_id: Set(encode_id(&entry.album_id)),
            sync_seq: Set(entry.sync_seq as i64),
            kind: Set(kind_label(entry.kind).to_string()),
            original_held: Set(entry.original_held),
            tombstoned: Set(entry.kind == ChangeKind::Deleted),
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
