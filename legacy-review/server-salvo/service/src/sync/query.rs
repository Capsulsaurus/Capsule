use ::entity::{album, album_share, owner_member, sync_entry};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    Statement,
};

use super::{BlobServeReference, FeedBlobManifest};
use crate::blob_store::is_content_hash;

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
    /// The serve-time reverse lookup for slice `S-C10`: the newest committed feed reference
    /// that names ciphertext content address `hash`, or `None` when no committed row does.
    ///
    /// This reads the same `FeedBlobManifest` SSoT `asset_blob_index` reads, but keyed by
    /// content address instead of asset id — the match is pushed into Postgres jsonb (the
    /// `original` slot by extraction, the `derivatives` array by containment) so a media
    /// serve never scans the feed. The highest `feed_seq` wins, mirroring `asset_blob_index`'s
    /// "newest committed reference" rule. `hash` is validated to be a well-formed content
    /// address first, so it is injection-safe to interpolate into the containment literal.
    pub async fn blob_serve_reference<C: ConnectionTrait>(
        db: &C,
        hash: &str,
    ) -> Result<Option<BlobServeReference>, DbErr> {
        // A malformed address can address no blob by construction — never reaches the JSON.
        if !is_content_hash(hash) {
            return Ok(None);
        }
        let containment = format!(r#"[{{"ciphertext_hash":"{hash}"}}]"#);
        let stmt = Statement::from_sql_and_values(
            db.get_database_backend(),
            r"SELECT album_id, asset_id, original_held,
                     CASE
                         WHEN blobs -> 'original' ->> 'ciphertext_hash' = $1
                             THEN blobs -> 'original' ->> 'role'
                         ELSE (
                             SELECT d ->> 'role'
                             FROM jsonb_array_elements(blobs -> 'derivatives') AS d
                             WHERE d ->> 'ciphertext_hash' = $1
                             LIMIT 1
                         )
                     END AS role
              FROM sync_entries
              WHERE blobs -> 'original' ->> 'ciphertext_hash' = $1
                 OR blobs -> 'derivatives' @> $2::jsonb
              ORDER BY feed_seq DESC
              LIMIT 1",
            [hash.into(), containment.into()],
        );
        let Some(row) = db.query_one(stmt).await? else {
            return Ok(None);
        };
        Ok(Some(BlobServeReference {
            album_id: row.try_get("", "album_id")?,
            asset_id: row.try_get("", "asset_id")?,
            role: row
                .try_get::<Option<String>>("", "role")?
                .unwrap_or_default(),
            original_held: row.try_get("", "original_held")?,
        }))
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
