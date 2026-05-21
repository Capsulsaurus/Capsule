use ::entity::{
    asset::{self, Entity as Asset},
    owner_member::{self, Entity as OwnerMember},
};
use sea_orm::*;

pub struct Query;

impl Query {
    /// Returns asset by ID
    pub async fn find_asset_by_id(db: &DbConn, id: &str) -> Result<Option<asset::Model>, DbErr> {
        Asset::find_by_id(id).one(db).await
    }

    /// Returns whether asset exists
    pub async fn asset_exists(db: &DbConn, id: &str) -> Result<bool, DbErr> {
        let count = Asset::find_by_id(id).count(db).await?;
        Ok(count > 0)
    }

    /// Returns all assets belonging to owner
    pub async fn find_assets_by_owner(
        db: &DbConn,
        owner_id: &str,
    ) -> Result<Vec<asset::Model>, DbErr> {
        Asset::find()
            .filter(asset::Column::OwnerId.eq(owner_id))
            .all(db)
            .await
    }

    /// Returns list of user IDs that have access to asset
    /// Returns None if asset does not exist
    pub async fn get_owners(db: &DbConn, asset_id: &str) -> Result<Option<Vec<String>>, DbErr> {
        // Find owner ID of asset
        let owner_id = Asset::find_by_id(asset_id)
            .select_only()
            .column(asset::Column::OwnerId)
            .into_tuple::<String>()
            .one(db)
            .await?;
        if let Some(owner_id) = owner_id {
            let users = OwnerMember::find()
                .filter(owner_member::Column::OwnerId.eq(owner_id))
                .select_only()
                .column(owner_member::Column::UserId)
                .into_tuple::<String>()
                .all(db)
                .await?;

            Ok(Some(users))
        } else {
            Ok(None)
        }
    }

    /// Find an existing asset by hash for user-accessible owners
    /// Used for duplicate detection during upload
    pub async fn find_by_hash_for_user(
        db: &DbConn,
        user_id: &str,
        file_hash: &str,
    ) -> Result<Option<asset::Model>, DbErr> {
        // Get all owner IDs the user is a member of
        let owner_ids: Vec<String> = OwnerMember::find()
            .filter(owner_member::Column::UserId.eq(user_id))
            .select_only()
            .column(owner_member::Column::OwnerId)
            .into_tuple()
            .all(db)
            .await?;

        if owner_ids.is_empty() {
            return Ok(None);
        }

        // Find asset with matching hash belonging to any of these owners
        Asset::find()
            .filter(asset::Column::FileHash.eq(file_hash))
            .filter(asset::Column::OwnerId.is_in(owner_ids))
            .filter(asset::Column::Uploaded.eq(true))
            .one(db)
            .await
    }
}
