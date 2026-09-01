use entity::device_directory;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait};

pub struct Query;

impl Query {
    /// Fetch the exact signed `DeviceDirectory` bytes last published for `user_id`, or
    /// `None` if the user has never published one. Returned verbatim — the server never
    /// re-models the signed document, so a client verifies the signature it pinned.
    #[tracing::instrument(skip(db), fields(user_id = %user_id))]
    pub async fn fetch<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
    ) -> Result<Option<Vec<u8>>, DbErr> {
        Ok(device_directory::Entity::find_by_id(user_id)
            .one(db)
            .await?
            .map(|row| row.document))
    }

    /// The `directory_version` currently stored for `user_id`, or `None` if unset. Used for
    /// best-effort diagnostics on a rejected publish; the monotonicity guard itself lives in
    /// the guarded upsert, not here.
    pub async fn stored_version<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
    ) -> Result<Option<i64>, DbErr> {
        Ok(device_directory::Entity::find_by_id(user_id)
            .one(db)
            .await?
            .map(|row| row.directory_version))
    }
}
