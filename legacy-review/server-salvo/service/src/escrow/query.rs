use entity::backup_escrow;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait};

pub struct Query;

impl Query {
    /// Fetch the exact escrow blob last stored for `user_id`, or `None` if the user has never
    /// stored one. Returned verbatim — the server never interprets the wrap format, so the
    /// client unwraps the bytes it stored. Owner-scoped: the route only ever passes the
    /// authenticated caller's own account id.
    #[tracing::instrument(skip(db), fields(user_id = %user_id))]
    pub async fn fetch<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
    ) -> Result<Option<Vec<u8>>, DbErr> {
        Ok(backup_escrow::Entity::find_by_id(user_id)
            .one(db)
            .await?
            .map(|row| row.blob))
    }
}
