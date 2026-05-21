use ::entity::friendship::{self, FriendshipStatus};
use sea_orm::*;

pub struct Query;

impl Query {
    /// Get a specific friendship
    pub async fn get_friendship(
        db: &DatabaseConnection,
        id: i32,
    ) -> Result<Option<friendship::Model>, DbErr> {
        friendship::Entity::find_by_id(id).one(db).await
    }

    /// Get all accepted friends for a user
    pub async fn get_friends(
        db: &DatabaseConnection,
        user_id: &str,
    ) -> Result<Vec<friendship::Model>, DbErr> {
        friendship::Entity::find_friends(user_id).all(db).await
    }

    /// Get pending friend requests received by a user
    pub async fn get_pending_requests(
        db: &DatabaseConnection,
        user_id: &str,
    ) -> Result<Vec<friendship::Model>, DbErr> {
        friendship::Entity::find_pending_requests(user_id)
            .all(db)
            .await
    }

    /// Get all friend user IDs for a user (returns the other user's ID in each friendship)
    pub async fn get_friend_ids(
        db: &DatabaseConnection,
        user_id: &str,
    ) -> Result<Vec<String>, DbErr> {
        let friendships = Self::get_friends(db, user_id).await?;
        Ok(friendships
            .into_iter()
            .map(|f| {
                if f.user_id == user_id {
                    f.friend_id
                } else {
                    f.user_id
                }
            })
            .collect())
    }

    /// Check if two users are friends
    pub async fn are_friends(
        db: &DatabaseConnection,
        user_id: &str,
        other_user_id: &str,
    ) -> Result<bool, DbErr> {
        let friendship = friendship::Entity::find()
            .filter(
                Condition::any()
                    .add(
                        Condition::all()
                            .add(friendship::Column::UserId.eq(user_id))
                            .add(friendship::Column::FriendId.eq(other_user_id)),
                    )
                    .add(
                        Condition::all()
                            .add(friendship::Column::UserId.eq(other_user_id))
                            .add(friendship::Column::FriendId.eq(user_id)),
                    ),
            )
            .filter(friendship::Column::Status.eq(FriendshipStatus::Accepted))
            .one(db)
            .await?;

        Ok(friendship.is_some())
    }

    /// Check if user can upload with given owner_id
    /// The owner_id must have a relationship where:
    /// 1. User is a member of the owner group
    /// 2. All other members of the owner group are friends with the user
    pub async fn can_upload_with_owner(
        db: &DatabaseConnection,
        user_id: &str,
        owner_id: &str,
    ) -> Result<bool, DbErr> {
        use ::entity::owner_member;

        // Get all members of this owner group
        let members: Vec<String> = owner_member::Entity::find()
            .select_only()
            .column(owner_member::Column::UserId)
            .filter(owner_member::Column::OwnerId.eq(owner_id))
            .into_tuple()
            .all(db)
            .await?;

        // User must be a member
        if !members.contains(&user_id.to_string()) {
            return Ok(false);
        }

        // All other members must be friends with the user
        for member_id in members {
            if member_id == user_id {
                continue;
            }
            if !Self::are_friends(db, user_id, &member_id).await? {
                return Ok(false);
            }
        }

        Ok(true)
    }
}
