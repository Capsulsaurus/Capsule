use ::entity::sync_entry;
use sea_orm::{ConnectionTrait, DbErr, Set, Statement};

use super::FeedEntryInput;

pub struct Mutation;

impl Mutation {
    /// Mint the next per-album `sync_seq` and append one feed entry, returning the minted
    /// `sync_seq`. MUST run inside the upload finalization transaction so the mint is atomic
    /// with the asset's `uploaded` flip and linearised per album by the counter row lock.
    pub async fn record_finalization<C: ConnectionTrait>(
        db: &C,
        input: FeedEntryInput,
    ) -> Result<i64, DbErr> {
        let sync_seq = Self::mint_next_seq(db, &input.album_id).await?;

        let blobs = serde_json::to_value(&input.blobs)
            .map_err(|e| DbErr::Custom(format!("serialize feed blobs: {e}")))?;

        let entry = sync_entry::ActiveModel {
            album_id: Set(input.album_id),
            sync_seq: Set(sync_seq),
            protocol_version: Set(input.protocol_version),
            kind: Set(input.kind.as_i16()),
            asset_id: Set(input.asset_id),
            manifest_cbor: Set(input.manifest_cbor),
            metadata_blob: Set(input.metadata_blob),
            blobs: Set(blobs),
            original_held: Set(input.original_held),
            ..Default::default()
        };
        <sync_entry::ActiveModel as sea_orm::ActiveModelTrait>::insert(entry, db).await?;

        Ok(sync_seq)
    }

    /// Atomically bump and return the album's next `sync_seq`. The `ON CONFLICT DO UPDATE`
    /// takes the counter row's lock, so concurrent minters are serialised and the sequence
    /// is strictly increasing with no gaps or duplicates.
    async fn mint_next_seq<C: ConnectionTrait>(db: &C, album_id: &str) -> Result<i64, DbErr> {
        let stmt = Statement::from_sql_and_values(
            db.get_database_backend(),
            r"INSERT INTO sync_album_seq (album_id, last_seq) VALUES ($1, 1)
              ON CONFLICT (album_id) DO UPDATE SET last_seq = sync_album_seq.last_seq + 1
              RETURNING last_seq",
            [album_id.into()],
        );
        let row = db
            .query_one(stmt)
            .await?
            .ok_or_else(|| DbErr::Custom("sync_seq mint returned no row".to_string()))?;
        row.try_get::<i64>("", "last_seq")
    }
}
