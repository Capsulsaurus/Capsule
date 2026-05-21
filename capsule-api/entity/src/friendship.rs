use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "friendships")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i32,
    #[sea_orm(indexed)]
    pub user_id: String,
    #[sea_orm(indexed)]
    pub friend_id: String,
    #[sea_orm(indexed)]
    pub status: FriendshipStatus,
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP"
    )]
    pub created_at: DateTime<Utc>,
    #[sea_orm(column_type = "TimestampWithTimeZone", nullable)]
    pub accepted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(20))")]
pub enum FriendshipStatus {
    #[sea_orm(string_value = "pending")]
    Pending,
    #[sea_orm(string_value = "accepted")]
    Accepted,
    #[sea_orm(string_value = "rejected")]
    Rejected,
    #[sea_orm(string_value = "blocked")]
    Blocked,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id"
    )]
    User,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::FriendId",
        to = "super::user::Column::Id"
    )]
    Friend,
}

impl ActiveModelBehavior for ActiveModel {}

impl Entity {
    /// Find all friendships for a user (both as requester and as friend)
    pub fn find_for_user(user_id: &str) -> Select<Entity> {
        Self::find().filter(Column::UserId.eq(user_id).or(Column::FriendId.eq(user_id)))
    }

    /// Find accepted friendships for a user
    pub fn find_friends(user_id: &str) -> Select<Entity> {
        Self::find()
            .filter(Column::UserId.eq(user_id).or(Column::FriendId.eq(user_id)))
            .filter(Column::Status.eq(FriendshipStatus::Accepted))
    }

    /// Find pending friend requests sent to a user
    pub fn find_pending_requests(user_id: &str) -> Select<Entity> {
        Self::find()
            .filter(Column::FriendId.eq(user_id))
            .filter(Column::Status.eq(FriendshipStatus::Pending))
    }
}
