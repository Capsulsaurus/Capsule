use activity::ActivityQuery;
use async_graphql::extensions::Logger;
use async_graphql::extensions::apollo_persisted_queries::{
    ApolloPersistedQueries, LruCacheStorage,
};
use async_graphql::{MergedSubscription, Object, Schema};

use crate::loaders::Loaders;
use crate::schema::album::{AlbumMutation, AlbumQuery};
use crate::schema::asset::{AssetMutation, AssetQuery, AssetSubscription};
use crate::schema::memory::{MemoryMutation, MemoryQuery};
use crate::schema::person::{PersonMutation, PersonQuery};
use crate::schema::share::{ShareMutation, ShareQuery};
use crate::schema::stack::{StackMutation, StackQuery};
use crate::schema::user::{UserMutation, UserQuery};
pub mod activity;
pub mod album;
pub mod asset;
pub mod memory;
pub mod person;
pub mod share;
pub mod smart_tag;
pub mod stack;
pub mod storage;
pub mod user;

mod types;
pub use types::*;
use user::statistics::UserStatisticsQuery;

pub struct QueryRoot {
    pub user: UserQuery,
    pub album: AlbumQuery,
    pub asset: AssetQuery,
    pub activity: ActivityQuery,
    pub person: PersonQuery,
    pub share: ShareQuery,
    pub memory: MemoryQuery,
    pub stack: StackQuery,
}

pub struct MutationRoot {
    pub user: UserMutation,
    pub album: AlbumMutation,
    pub asset: AssetMutation,
    pub person: PersonMutation,
    pub share: ShareMutation,
    pub memory: MemoryMutation,
    pub stack: StackMutation,
}

#[Object]
impl QueryRoot {
    async fn user(&self) -> &UserQuery {
        &self.user
    }

    async fn album(&self) -> &AlbumQuery {
        &self.album
    }

    async fn asset(&self) -> &AssetQuery {
        &self.asset
    }

    async fn activity(&self) -> &ActivityQuery {
        &self.activity
    }

    async fn person(&self) -> &PersonQuery {
        &self.person
    }

    async fn share(&self) -> &ShareQuery {
        &self.share
    }

    async fn memory(&self) -> &MemoryQuery {
        &self.memory
    }

    async fn stack(&self) -> &StackQuery {
        &self.stack
    }
}

#[Object]
impl MutationRoot {
    async fn album(&self) -> &AlbumMutation {
        &self.album
    }

    async fn asset(&self) -> &AssetMutation {
        &self.asset
    }

    async fn person(&self) -> &PersonMutation {
        &self.person
    }

    async fn share(&self) -> &ShareMutation {
        &self.share
    }

    async fn memory(&self) -> &MemoryMutation {
        &self.memory
    }

    async fn stack(&self) -> &StackMutation {
        &self.stack
    }
}

#[derive(MergedSubscription, Default)]
pub struct SubscriptionRoot(AssetSubscription);

pub type AppSchema = Schema<QueryRoot, MutationRoot, SubscriptionRoot>;

pub fn create_schema(loaders: Loaders) -> AppSchema {
    let schema = Schema::build(
        QueryRoot {
            user: UserQuery {
                statistics: UserStatisticsQuery,
            },
            album: AlbumQuery,
            asset: AssetQuery,
            activity: ActivityQuery,
            person: PersonQuery,
            share: ShareQuery,
            memory: MemoryQuery,
            stack: StackQuery,
        },
        MutationRoot {
            user: UserMutation,
            album: AlbumMutation,
            asset: AssetMutation,
            person: PersonMutation,
            share: ShareMutation,
            memory: MemoryMutation,
            stack: StackMutation,
        },
        SubscriptionRoot::default(),
    );

    #[cfg(debug_assertions)]
    let schema = schema.extension(Logger);

    // TODO: Setup twemproxy+memcached for distributed cache w/ r2d2-memcache
    let schema = schema.extension(ApolloPersistedQueries::new(LruCacheStorage::new(1024)));

    schema.data(loaders).finish()
}
