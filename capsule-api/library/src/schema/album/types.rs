use async_graphql::*;
use chrono::{DateTime, Utc};

use crate::schema::user::User;

// TODO: Implement this

#[derive(SimpleObject)]
pub struct Album {
    pub id: ID,
    pub owner: User,
    pub name: String,
}

#[derive(InputObject)]
pub struct CreateAlbumInput {
    name: String,
    email: String,
}

#[derive(InputObject)]
pub struct UpdateAlbumInput {
    name: Option<String>,
    email: Option<String>,
}

#[derive(InputObject)]
pub struct ShareAlbumInput {
    user_ids: Vec<ID>,
} // TODO: Add permission level

/// Filter for albums
#[derive(InputObject)]
pub struct AlbumFilter {
    /// User ID
    pub owner_id: Option<ID>,
    /// Minimum created at date
    pub created_at_min: Option<DateTime<Utc>>,
    /// Maximum created at date
    pub created_at_max: Option<DateTime<Utc>>,
    /// Minimum modified at date
    pub modified_at_min: Option<DateTime<Utc>>,
}

// TODO: Use this
#[derive(Enum, Clone, Copy, Eq, PartialEq)]
pub enum AlbumRole {
    Owner,
    Contributor,
    Viewer,
}
