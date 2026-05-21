use super::{Album, CreateAlbumInput, ShareAlbumInput, UpdateAlbumInput};
use async_graphql::*;

pub struct AlbumMutation;

#[Object]
impl AlbumMutation {
    /// Create an album
    async fn create(&self, _ctx: &Context<'_>, _input: CreateAlbumInput) -> Result<Album> {
        todo!()
    }

    /// Update an album
    async fn update(&self, _ctx: &Context<'_>, _id: ID, _input: UpdateAlbumInput) -> Result<Album> {
        todo!()
    }

    /// Delete an album
    async fn delete(&self, _ctx: &Context<'_>, _id: ID) -> Result<Album> {
        todo!()
    }

    /// Share an album
    async fn share(&self, _ctx: &Context<'_>, _id: ID, _input: ShareAlbumInput) -> Result<Album> {
        todo!()
    }
}
