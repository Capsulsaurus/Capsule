use salvo::oapi::ToSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A generic API error response.
///
/// `error` is an English detail message. `code`, when present, is a stable
/// machine-readable identifier from the i18n `error.*` catalog (see the
/// `capsule-i18n` crate and `locales/`); clients map it to a localized
/// high-level message while the detail stays English. The field is optional for
/// backward compatibility — older responses simply omit it.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct ApiError {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl ApiError {
    pub fn new(error: impl Into<String>) -> Self {
        ApiError {
            error: error.into(),
            code: None,
        }
    }

    /// Build an error response carrying a stable catalog code for client localization.
    ///
    /// Pass a `capsule_i18n::error_codes` constant so a typo is a compile error and the
    /// code stays in sync with the canonical catalog.
    pub fn with_code(error: impl Into<String>, code: impl Into<String>) -> Self {
        ApiError {
            error: error.into(),
            code: Some(code.into()),
        }
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
