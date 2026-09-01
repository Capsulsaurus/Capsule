use entity::passkey;
use sea_orm::{ColumnTrait, DatabaseConnection, DbErr, EntityTrait, PaginatorTrait, QueryFilter};

pub struct Query;

impl Query {
    /// Find all passkeys for a specific user
    pub async fn find_by_user_id(
        conn: &DatabaseConnection,
        user_id: &str,
    ) -> Result<Vec<model::passkey::Passkey>, DbErr> {
        passkey::Entity::find()
            .filter(passkey::Column::UserId.eq(user_id))
            .all(conn)
            .await
            .map(|passkeys| passkeys.into_iter().map(Into::into).collect())
    }

    /// Count the number of passkeys for a specific user
    pub async fn count_by_user_id(conn: &DatabaseConnection, user_id: &str) -> Result<u64, DbErr> {
        passkey::Entity::find()
            .filter(passkey::Column::UserId.eq(user_id))
            .count(conn)
            .await
    }

    /// Find a passkey by its credential ID
    pub async fn find_by_cred_id(
        conn: &DatabaseConnection,
        cred_id: &[u8],
    ) -> Result<Option<model::passkey::Passkey>, DbErr> {
        passkey::Entity::find()
            .filter(passkey::Column::CredId.eq(cred_id))
            .one(conn)
            .await
            .map(|passkey| passkey.map(Into::into))
    }
}
