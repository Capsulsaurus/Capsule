use ::entity::{
    album::{self, Entity as Album},
    album_share::{self, Entity as AlbumShare, SharePermission},
    owner_member::{self, Entity as OwnerMember},
};
use capsule_core::models::album::AlbumAccess;
use sea_orm::*;

pub struct Query;

impl Query {
    /// Returns album by ID
    pub async fn find_album_by_id(db: &DbConn, id: &str) -> Result<Option<album::Model>, DbErr> {
        Album::find_by_id(id).one(db).await
    }

    /// Returns whether album exists
    pub async fn album_exists(db: &DbConn, id: &str) -> Result<bool, DbErr> {
        let count = Album::find_by_id(id).count(db).await?;
        Ok(count > 0)
    }

    /// Returns all albums belonging to owner
    pub async fn find_albums_by_owner(
        db: &DbConn,
        owner_id: &str,
    ) -> Result<Vec<album::Model>, DbErr> {
        Album::find()
            .filter(album::Column::OwnerId.eq(owner_id))
            .all(db)
            .await
    }

    /// Returns whether user has access to album
    /// Returns None if user or album does not exist or user does not have access
    pub async fn get_album_access(
        db: &DbConn,
        user_id: &str,
        album_id: &str,
    ) -> Result<Option<AlbumAccess>, DbErr> {
        // Find owner ID of album
        let owner_id = Album::find_by_id(album_id)
            .select_only()
            .column(album::Column::OwnerId)
            .into_tuple::<String>()
            .one(db)
            .await?;
        if let Some(owner_id) = owner_id {
            // Check if user is owner
            let is_owner = OwnerMember::find()
                .filter(owner_member::Column::OwnerId.eq(owner_id))
                .filter(owner_member::Column::UserId.eq(user_id))
                .one(db)
                .await?
                .is_some();
            if is_owner {
                return Ok(Some(AlbumAccess::Owner));
            }
        };

        // Check if user has share access
        let share_permission = AlbumShare::find()
            .filter(album_share::Column::AlbumId.eq(album_id))
            .filter(album_share::Column::UserId.eq(user_id))
            .one(db)
            .await?
            .map(|share| share.permission);

        match share_permission {
            Some(SharePermission::View) => Ok(Some(AlbumAccess::Read)),
            Some(SharePermission::Edit) => Ok(Some(AlbumAccess::Write)),
            None => Ok(None),
        }
    }

    // /// Returns all albums accessible by user
    // pub async fn find_all_albums(db: &DbConn, user_id: &str) -> Result<Vec<album::Model>, DbErr> {
    //     // We find all albums that has owner_id that the user is either an owner or has some share access (read/write)
    //     todo!()
    // }
}
