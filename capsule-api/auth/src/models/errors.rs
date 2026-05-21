use salvo::oapi::ToSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A generic API error response
#[derive(Serialize, Deserialize, ToSchema)]
pub struct ApiError {
    pub error: String,
}

impl ApiError {
    pub fn new(error: impl Into<String>) -> Self {
        let error = error.into();
        ApiError { error }
    }
}

#[derive(Serialize, Deserialize, Error, ToSchema, Debug)]
pub enum BadRegisterUserRequestError {
    #[error("Invalid email")]
    Email,
    #[error("Invalid username")]
    Username,
    #[error("Invalid password")]
    Password,
    #[error("Invalid request")]
    InvalidRequest,
}
