use chrono::Utc;
use entity::passkey;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, Set,
};

pub struct Mutation;

#[derive(Debug, Clone)]
pub struct CreatePasskeyArgs {
    pub user_id: String,
    pub cred_id: Vec<u8>,
    pub public_key: Vec<u8>,
    pub name: String,
    pub counter: i64,
    pub aaguid: Option<uuid::Uuid>,
    pub backup_eligible: bool,
    pub backup_state: bool,
}

impl Mutation {
    pub async fn create_passkey(
        conn: &DatabaseConnection,
        args: CreatePasskeyArgs,
    ) -> Result<passkey::Model, DbErr> {
        let model = passkey::ActiveModel {
            id: Set(nanoid::nanoid!()),
            user_id: Set(args.user_id),
            cred_id: Set(args.cred_id),
            public_key: Set(args.public_key),
            counter: Set(args.counter),
            name: Set(args.name),
            created_at: Set(Utc::now()),
            last_used_at: Set(Some(Utc::now())),
            aaguid: Set(args.aaguid),
            backup_eligible: Set(args.backup_eligible),
            backup_state: Set(args.backup_state),
        };

        model.insert(conn).await
    }

    pub async fn update_counter(
        conn: &DatabaseConnection,
        passkey_id: &str,
        new_counter: i64,
    ) -> Result<passkey::Model, DbErr> {
        let passkey = passkey::Entity::find_by_id(passkey_id)
            .one(conn)
            .await?
            .ok_or(DbErr::RecordNotFound(format!(
                "Passkey not found: {}",
                passkey_id
            )))?;

        let mut active: passkey::ActiveModel = passkey.into();
        active.counter = Set(new_counter);
        active.last_used_at = Set(Some(Utc::now()));

        active.update(conn).await
    }

    pub async fn delete_passkey(
        conn: &DatabaseConnection,
        passkey_id: &str,
        user_id: &str,
    ) -> Result<(), DbErr> {
        let res = passkey::Entity::delete_by_id(passkey_id)
            .filter(passkey::Column::UserId.eq(user_id))
            .exec(conn)
            .await?;

        if res.rows_affected == 0 {
            return Err(DbErr::RecordNotFound(
                "Passkey not found or not owned by user".to_string(),
            ));
        }

        Ok(())
    }
}
