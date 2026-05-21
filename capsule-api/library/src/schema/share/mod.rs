use async_graphql::*;
use chrono::{DateTime, Utc};
use entity::share_link::ActiveModel as ShareLinkActiveModel;
use entity::share_link::Model as ShareLinkModel;
use entity::share_link::ShareLinkType as EntityShareLinkType;
use entity::{album, album_share, owner_member, share_link, user};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};

use super::album::Album;
use super::user::User;
use crate::context::AppContext;
use std::collections::HashMap;

/// Type of content being shared
#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum ShareLinkType {
    Album,
    Asset,
    Selection,
}

impl From<EntityShareLinkType> for ShareLinkType {
    fn from(t: EntityShareLinkType) -> Self {
        match t {
            EntityShareLinkType::Album => ShareLinkType::Album,
            EntityShareLinkType::Asset => ShareLinkType::Asset,
            EntityShareLinkType::Selection => ShareLinkType::Selection,
        }
    }
}

/// A public share link for sharing content externally
pub struct ShareLink {
    pub model: ShareLinkModel,
}

#[Object]
impl ShareLink {
    async fn id(&self) -> ID {
        ID::from(&self.model.id)
    }

    async fn token(&self) -> &String {
        &self.model.token
    }

    /// Full URL for sharing
    async fn url(&self) -> String {
        // TODO: Get base URL from config
        format!("https://capsule.app/s/{}", self.model.token)
    }

    async fn share_type(&self) -> ShareLinkType {
        self.model.share_type.clone().into()
    }

    async fn allow_download(&self) -> bool {
        self.model.allow_download
    }

    async fn password_protected(&self) -> bool {
        self.model.password_hash.is_some()
    }

    async fn expires_at(&self) -> Option<DateTime<Utc>> {
        self.model.expires_at
    }

    async fn view_count(&self) -> i32 {
        self.model.view_count
    }

    async fn created_at(&self) -> DateTime<Utc> {
        self.model.created_at
    }

    // TODO: Resolve album/assets via dataloader based on share_type
    // async fn album(&self) -> Option<Album>
    // async fn assets(&self) -> Vec<AssetMetadata>
}

// ===== Inputs =====

#[derive(InputObject)]
pub struct CreateShareLinkInput {
    /// Album ID (for album shares)
    pub album_id: Option<ID>,
    /// Asset IDs (for asset or selection shares)
    pub asset_ids: Option<Vec<ID>>,
    /// Whether downloads are allowed (default: true)
    pub allow_download: Option<bool>,
    /// Optional password protection
    pub password: Option<String>,
    /// Optional expiration date
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(InputObject)]
pub struct UpdateShareLinkInput {
    pub allow_download: Option<bool>,
    /// Set to null to remove password
    pub password: Option<String>,
    /// Set to null to remove expiration
    pub expires_at: Option<DateTime<Utc>>,
}

// ===== Query =====

#[derive(Default)]
pub struct ShareQuery;

#[Object]
impl ShareQuery {
    /// Get all share links created by current user
    async fn shares(&self, ctx: &Context<'_>) -> Result<Vec<ShareLink>> {
        let app_ctx = ctx.data::<AppContext>()?;
        let user_id = app_ctx.user.user_id()?;

        let links = share_link::Entity::find()
            .filter(share_link::Column::CreatorId.eq(user_id))
            .all(&app_ctx.db.conn)
            .await?;

        Ok(links.into_iter().map(|model| ShareLink { model }).collect())
    }

    /// Get share link by ID
    async fn share(&self, ctx: &Context<'_>, id: ID) -> Result<ShareLink> {
        let app_ctx = ctx.data::<AppContext>()?;

        let link = share_link::Entity::find_by_id(id.to_string())
            .one(&app_ctx.db.conn)
            .await?
            .ok_or_else(|| Error::new("Share link not found"))?;

        Ok(ShareLink { model: link })
    }

    /// Assets/albums shared with user
    async fn shared_with_me(&self, ctx: &Context<'_>) -> Result<Vec<Album>> {
        let app_ctx = ctx.data::<AppContext>()?;
        let user_id = app_ctx.user.user_id()?;

        // Find albums shared with user
        let shared_albums = album_share::Entity::find()
            .filter(album_share::Column::UserId.eq(user_id))
            .all(&app_ctx.db.conn)
            .await?;

        let album_ids: Vec<String> = shared_albums.into_iter().map(|s| s.album_id).collect();

        if album_ids.is_empty() {
            return Ok(vec![]);
        }

        // Fetch albums
        let albums = album::Entity::find()
            .filter(album::Column::Id.is_in(album_ids))
            .all(&app_ctx.db.conn)
            .await?;

        let owner_ids: Vec<String> = albums.iter().map(|a| a.owner_id.clone()).collect();

        // Fetch owner members to link owner_id -> user_id
        let members = owner_member::Entity::find()
            .filter(owner_member::Column::OwnerId.is_in(owner_ids.clone()))
            .all(&app_ctx.db.conn)
            .await?;

        let user_ids: Vec<String> = members.iter().map(|m| m.user_id.clone()).collect();

        // Fetch users
        let users = user::Entity::find()
            .filter(user::Column::Id.is_in(user_ids))
            .all(&app_ctx.db.conn)
            .await?;

        // Map user_id -> User
        let user_map: HashMap<String, user::Model> =
            users.into_iter().map(|u| (u.id.clone(), u)).collect();

        // Map owner_id -> User (via first member found)
        let mut owner_user_map: HashMap<String, user::Model> = HashMap::new();
        for member in members {
            if let Some(u) = user_map.get(&member.user_id) {
                owner_user_map.entry(member.owner_id).or_insert(u.clone());
            }
        }

        let mut result = Vec::new();
        for album_model in albums {
            if let Some(user_model) = owner_user_map.get(&album_model.owner_id) {
                result.push(Album {
                    id: ID::from(album_model.id),
                    name: album_model.name,
                    owner: User::from(user_model.clone()),
                });
            }
        }

        Ok(result)
    }
}

// ===== Mutations =====

#[derive(Default)]
pub struct ShareMutation;

#[Object]
impl ShareMutation {
    /// Create a public share link
    async fn create_share(
        &self,
        ctx: &Context<'_>,
        input: CreateShareLinkInput,
    ) -> Result<ShareLink> {
        let app_ctx = ctx.data::<AppContext>()?;
        let user_id = app_ctx.user.user_id()?;

        let (share_type, target_id) = if let Some(album_id) = input.album_id {
            (EntityShareLinkType::Album, album_id.to_string())
        } else if let Some(asset_ids) = input.asset_ids {
            if asset_ids.is_empty() {
                return Err(Error::new("Must provide at least one asset ID"));
            }
            if asset_ids.len() == 1 {
                (EntityShareLinkType::Asset, asset_ids[0].to_string())
            } else {
                let ids: Vec<String> = asset_ids.into_iter().map(|id| id.to_string()).collect();
                (EntityShareLinkType::Selection, ids.join(","))
            }
        } else {
            return Err(Error::new("Must provide album_id or asset_ids"));
        };

        let model = ShareLinkActiveModel {
            creator_id: Set(user_id.clone()),
            share_type: Set(share_type),
            target_id: Set(target_id),
            allow_download: Set(input.allow_download.unwrap_or(true)),
            password_hash: Set(input.password), // TODO: Hash password
            expires_at: Set(input.expires_at),
            ..Default::default()
        };

        let saved = model.insert(&app_ctx.db.conn).await?;
        Ok(ShareLink { model: saved })
    }

    /// Update share link settings
    async fn update_share(
        &self,
        ctx: &Context<'_>,
        id: ID,
        input: UpdateShareLinkInput,
    ) -> Result<ShareLink> {
        let app_ctx = ctx.data::<AppContext>()?;
        let user_id = app_ctx.user.user_id()?;

        let link = share_link::Entity::find_by_id(id.to_string())
            .one(&app_ctx.db.conn)
            .await?
            .ok_or_else(|| Error::new("Share link not found"))?;

        if link.creator_id != *user_id {
            return Err(Error::new("Permission denied"));
        }

        let mut active: ShareLinkActiveModel = link.into_active_model();

        if let Some(allow) = input.allow_download {
            active.allow_download = Set(allow);
        }
        if let Some(pw) = input.password {
            active.password_hash = Set(Some(pw)); // TODO: Hash
        }
        if let Some(exp) = input.expires_at {
            active.expires_at = Set(Some(exp));
        }

        let updated = active.update(&app_ctx.db.conn).await?;
        Ok(ShareLink { model: updated })
    }

    /// Revoke/delete a share link
    async fn revoke_share(&self, ctx: &Context<'_>, id: ID) -> Result<bool> {
        let app_ctx = ctx.data::<AppContext>()?;
        let user_id = app_ctx.user.user_id()?;

        let link = share_link::Entity::find_by_id(id.to_string())
            .one(&app_ctx.db.conn)
            .await?
            .ok_or_else(|| Error::new("Share link not found"))?;

        if link.creator_id != *user_id {
            return Err(Error::new("Permission denied"));
        }

        let res = share_link::Entity::delete_by_id(id.to_string())
            .exec(&app_ctx.db.conn)
            .await?;

        Ok(res.rows_affected > 0)
    }

    /// Join shared album via share link
    async fn join_share(&self, ctx: &Context<'_>, id: ID) -> Result<bool> {
        let app_ctx = ctx.data::<AppContext>()?;
        let user_id = app_ctx.user.user_id()?;

        let link = share_link::Entity::find_by_id(id.to_string())
            .one(&app_ctx.db.conn)
            .await?
            .ok_or_else(|| Error::new("Share link not found"))?;

        // Verify expiration
        if let Some(expires) = link.expires_at
            && expires < Utc::now()
        {
            return Err(Error::new("Share link expired"));
        }

        if link.share_type != EntityShareLinkType::Album {
            return Err(Error::new("Can only join shared albums"));
        }

        let album_id = link.target_id;

        // Check if already joined
        let existing = album_share::Entity::find()
            .filter(album_share::Column::AlbumId.eq(&album_id))
            .filter(album_share::Column::UserId.eq(user_id))
            .one(&app_ctx.db.conn)
            .await?;

        if existing.is_some() {
            return Ok(true);
        }

        let share = album_share::ActiveModel {
            album_id: Set(album_id),
            user_id: Set(user_id.clone()),
            permission: Set(entity::album_share::SharePermission::View),
            ..Default::default()
        };

        share.insert(&app_ctx.db.conn).await?;

        Ok(true)
    }

    /// Leave shared album (using share link ID to identify context, or we should assume it identifies method)
    /// Given the prompt, using share ID.
    async fn leave_share(&self, ctx: &Context<'_>, id: ID) -> Result<bool> {
        let app_ctx = ctx.data::<AppContext>()?;
        let user_id = app_ctx.user.user_id()?;

        // The user prompted: DELETE /v1/shares/{id}/leave
        // If {id} is the Share Link ID, we find the album and leave.

        let link = share_link::Entity::find_by_id(id.to_string())
            .one(&app_ctx.db.conn)
            .await?
            .ok_or_else(|| Error::new("Share link not found"))?;

        if link.share_type != EntityShareLinkType::Album {
            return Err(Error::new("Not an album share"));
        }

        let album_id = link.target_id;

        let res = album_share::Entity::delete_many()
            .filter(album_share::Column::AlbumId.eq(album_id))
            .filter(album_share::Column::UserId.eq(user_id))
            .exec(&app_ctx.db.conn)
            .await?;

        Ok(res.rows_affected > 0)
    }
}
