use super::{Album, AlbumFilter};
use async_graphql::*;

pub struct AlbumQuery;

#[Object]
impl AlbumQuery {
    /// Get album by ID
    async fn by_id(&self, _ctx: &Context<'_>, _id: ID) -> Result<Album> {
        todo!()
    }

    /// Get all albums for the current user
    async fn mine(&self, _ctx: &Context<'_>) -> Result<Vec<Album>> {
        todo!()
    }

    /// Search albums
    async fn search(&self, _ctx: &Context<'_>, _filter: AlbumFilter) -> Result<Vec<Album>> {
        todo!()
    }
}
