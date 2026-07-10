use ::entity::{album, album_share, owner_member, sync_entry};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
};

use super::FeedBlobManifest;

pub struct Query;

impl Query {
    /// The set of committed blob references for one asset, keyed by content address → role.
    ///
    /// This is the `indexed` source of truth for storage verification (slice `S-C3`): every
    /// finalized blob of the asset appears here because the feed row is minted **inside** the
    /// upload finalization transaction, atomically with the asset's `uploaded` flip. A blob
    /// hash absent from this map has no committed, `uploaded = true` row referencing it, so it
    /// is reported `indexed = false`. Later feed rows (metadata updates) override an earlier
    /// role for the same hash — the newest committed reference wins.
    pub async fn asset_blob_index<C: ConnectionTrait>(
        db: &C,
        asset_id: &str,
    ) -> Result<std::collections::HashMap<String, String>, DbErr> {
        let rows = sync_entry::Entity::find()
            .filter(sync_entry::Column::AssetId.eq(asset_id))
            .order_by_asc(sync_entry::Column::FeedSeq)
            .all(db)
            .await?;

        let mut index = std::collections::HashMap::new();
        for row in rows {
            let manifest: FeedBlobManifest = serde_json::from_value(row.blobs)
                .map_err(|e| DbErr::Custom(format!("decode feed blobs: {e}")))?;
            if let Some(original) = manifest.original {
                index.insert(original.ciphertext_hash, original.role);
            }
            for derivative in manifest.derivatives {
                index.insert(derivative.ciphertext_hash, derivative.role);
            }
        }
        Ok(index)
    }
    /// One forward-only page of feed entries for `album_ids`, strictly after `after_feed_seq`,
    /// ordered by the global append key. Re-issuing the same `after_feed_seq` returns the same
    /// page (the cursor's idempotency guarantee); the page never regresses.
    pub async fn feed_page<C: ConnectionTrait>(
        db: &C,
        album_ids: &[String],
        after_feed_seq: i64,
        limit: u64,
    ) -> Result<Vec<sync_entry::Model>, DbErr> {
        if album_ids.is_empty() {
            return Ok(Vec::new());
        }
        sync_entry::Entity::find()
            .filter(sync_entry::Column::AlbumId.is_in(album_ids.iter().cloned()))
            .filter(sync_entry::Column::FeedSeq.gt(after_feed_seq))
            .order_by_asc(sync_entry::Column::FeedSeq)
            .limit(limit)
            .all(db)
            .await
    }

    /// The album ids the user may read the feed for: albums owned by any owner-group the user
    /// belongs to, plus albums explicitly shared with the user. Deduplicated and sorted.
    pub async fn accessible_album_ids<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
    ) -> Result<Vec<String>, DbErr> {
        let owner_ids: Vec<String> = owner_member::Entity::find()
            .filter(owner_member::Column::UserId.eq(user_id))
            .select_only()
            .column(owner_member::Column::OwnerId)
            .into_tuple()
            .all(db)
            .await?;

        let mut ids: Vec<String> = Vec::new();
        if !owner_ids.is_empty() {
            let owned: Vec<String> = album::Entity::find()
                .filter(album::Column::OwnerId.is_in(owner_ids))
                .filter(album::Column::DeletedAt.is_null())
                .select_only()
                .column(album::Column::Id)
                .into_tuple()
                .all(db)
                .await?;
            ids.extend(owned);
        }

        let shared: Vec<String> = album_share::Entity::find()
            .filter(album_share::Column::UserId.eq(user_id))
            .select_only()
            .column(album_share::Column::AlbumId)
            .into_tuple()
            .all(db)
            .await?;
        ids.extend(shared);

        ids.sort();
        ids.dedup();
        Ok(ids)
    }
}
