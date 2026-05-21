use sea_orm::DbErr;
use thiserror::Error;

/// Errors that can occur during friendship operations
#[derive(Debug, Error)]
pub enum FriendshipError {
    /// Database error
    #[error("Database error: {0}")]
    DbError(#[from] DbErr),

    /// Friendship or request not found
    #[error("Not found")]
    NotFound,

    /// User is not authorized to perform this action
    #[error("Not authorized")]
    NotAuthorized,

    /// Request is not in pending state
    #[error("Request is not pending")]
    NotPending,
}

/// Result of sending a friend request
#[derive(Debug)]
pub enum SendRequestResult {
    /// New request was created
    Created(::entity::friendship::Model),
    /// Friendship already exists (either pending, accepted, or other state)
    AlreadyExists(::entity::friendship::Model),
}

impl SendRequestResult {
    /// Get the underlying model regardless of variant
    pub fn into_model(self) -> ::entity::friendship::Model {
        match self {
            Self::Created(m) | Self::AlreadyExists(m) => m,
        }
    }

    /// Returns true if this was a new request
    pub fn is_created(&self) -> bool {
        matches!(self, Self::Created(_))
    }
}

/// Result of accepting a friend request
#[derive(Debug)]
pub enum AcceptRequestResult {
    /// Request was accepted
    Accepted(::entity::friendship::Model),
    /// Request was already accepted
    AlreadyAccepted(::entity::friendship::Model),
}

impl AcceptRequestResult {
    pub fn into_model(self) -> ::entity::friendship::Model {
        match self {
            Self::Accepted(m) | Self::AlreadyAccepted(m) => m,
        }
    }
}

/// Result of rejecting a friend request
#[derive(Debug)]
pub enum RejectRequestResult {
    /// Request was rejected
    Rejected(::entity::friendship::Model),
    /// Request was already rejected or in non-pending state
    AlreadyHandled(::entity::friendship::Model),
}

impl RejectRequestResult {
    pub fn into_model(self) -> ::entity::friendship::Model {
        match self {
            Self::Rejected(m) | Self::AlreadyHandled(m) => m,
        }
    }
}

/// Result of removing a friendship
#[derive(Debug)]
pub enum RemoveFriendshipResult {
    /// Friendship was removed
    Removed,
    /// Friendship was already removed or didn't exist
    NotFound,
}
