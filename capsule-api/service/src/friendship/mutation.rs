use super::error::{
    AcceptRequestResult, FriendshipError, RejectRequestResult, RemoveFriendshipResult,
    SendRequestResult,
};
use ::entity::friendship::{self, FriendshipStatus};
use chrono::Utc;
use sea_orm::*;

pub struct Mutation;

impl Mutation {
    /// Send a friend request
    ///
    /// Returns `SendRequestResult::Created` if a new request was created,
    /// or `SendRequestResult::AlreadyExists` if a friendship/request already exists.
    pub async fn send_request(
        db: &DatabaseConnection,
        user_id: &str,
        friend_id: &str,
    ) -> Result<SendRequestResult, FriendshipError> {
        // Check if request already exists (in either direction)
        let existing = friendship::Entity::find()
            .filter(
                Condition::any()
                    .add(
                        Condition::all()
                            .add(friendship::Column::UserId.eq(user_id))
                            .add(friendship::Column::FriendId.eq(friend_id)),
                    )
                    .add(
                        Condition::all()
                            .add(friendship::Column::UserId.eq(friend_id))
                            .add(friendship::Column::FriendId.eq(user_id)),
                    ),
            )
            .one(db)
            .await?;

        if let Some(existing_model) = existing {
            return Ok(SendRequestResult::AlreadyExists(existing_model));
        }

        let model = friendship::ActiveModel {
            user_id: Set(user_id.to_string()),
            friend_id: Set(friend_id.to_string()),
            status: Set(FriendshipStatus::Pending),
            created_at: Set(Utc::now()),
            ..Default::default()
        };

        let created = model.insert(db).await?;
        Ok(SendRequestResult::Created(created))
    }

    /// Accept a friend request
    ///
    /// Returns `AcceptRequestResult::Accepted` if the request was accepted,
    /// or `AcceptRequestResult::AlreadyAccepted` if it was already accepted.
    pub async fn accept_request(
        db: &DatabaseConnection,
        request_id: i32,
        accepting_user_id: &str,
    ) -> Result<AcceptRequestResult, FriendshipError> {
        let request = friendship::Entity::find_by_id(request_id)
            .one(db)
            .await?
            .ok_or(FriendshipError::NotFound)?;

        // Only the friend (recipient) can accept
        if request.friend_id != accepting_user_id {
            return Err(FriendshipError::NotAuthorized);
        }

        // Already accepted
        if request.status == FriendshipStatus::Accepted {
            return Ok(AcceptRequestResult::AlreadyAccepted(request));
        }

        // Can only accept pending requests
        if request.status != FriendshipStatus::Pending {
            return Err(FriendshipError::NotPending);
        }

        let mut model: friendship::ActiveModel = request.into();
        model.status = Set(FriendshipStatus::Accepted);
        model.accepted_at = Set(Some(Utc::now()));
        let updated = model.update(db).await?;
        Ok(AcceptRequestResult::Accepted(updated))
    }

    /// Reject a friend request
    ///
    /// Returns `RejectRequestResult::Rejected` if the request was rejected,
    /// or `RejectRequestResult::AlreadyHandled` if it was already handled.
    pub async fn reject_request(
        db: &DatabaseConnection,
        request_id: i32,
        rejecting_user_id: &str,
    ) -> Result<RejectRequestResult, FriendshipError> {
        let request = friendship::Entity::find_by_id(request_id)
            .one(db)
            .await?
            .ok_or(FriendshipError::NotFound)?;

        // Only the friend (recipient) can reject
        if request.friend_id != rejecting_user_id {
            return Err(FriendshipError::NotAuthorized);
        }

        // Already rejected or otherwise handled
        if request.status != FriendshipStatus::Pending {
            return Ok(RejectRequestResult::AlreadyHandled(request));
        }

        let mut model: friendship::ActiveModel = request.into();
        model.status = Set(FriendshipStatus::Rejected);
        let updated = model.update(db).await?;
        Ok(RejectRequestResult::Rejected(updated))
    }

    /// Remove a friendship
    ///
    /// Returns `RemoveFriendshipResult::Removed` if the friendship was removed,
    /// or `RemoveFriendshipResult::NotFound` if it didn't exist.
    pub async fn remove_friendship(
        db: &DatabaseConnection,
        friendship_id: i32,
        user_id: &str,
    ) -> Result<RemoveFriendshipResult, FriendshipError> {
        let friendship = match friendship::Entity::find_by_id(friendship_id)
            .one(db)
            .await?
        {
            Some(f) => f,
            None => return Ok(RemoveFriendshipResult::NotFound),
        };

        // Either party can remove the friendship
        if friendship.user_id != user_id && friendship.friend_id != user_id {
            return Err(FriendshipError::NotAuthorized);
        }

        friendship::Entity::delete_by_id(friendship_id)
            .exec(db)
            .await?;

        Ok(RemoveFriendshipResult::Removed)
    }
}
