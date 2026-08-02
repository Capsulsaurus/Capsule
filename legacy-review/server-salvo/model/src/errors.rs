use salvo::http::StatusCode;
use salvo::oapi::ToSchema;
use salvo::prelude::*;
use serde::{Deserialize, Serialize};

/// Global internal server error type for fatal, unrecoverable errors.
///
/// This type is designed for the "onion architecture" error handling strategy:
/// - **Obfuscation**: In release builds, all error details are hidden from clients
/// - **Debugging**: In debug builds, full error details are exposed
/// - **Logging**: All errors are logged with full context regardless of build mode
/// - **Type Safety**: Implements `From<T>` for common error types to enable `?` operator
///
/// # Usage
///
/// Use this type for errors that represent fatal internal failures with no client-facing
/// recovery path. For client-facing errors (validation, not found, etc.), use
/// domain-specific error types instead.
///
/// ```ignore
/// // In response enums:
/// pub enum MyResponses {
///     Success(Data),
///     InternalServerError(InternalServerError),
/// }
///
/// // Converting errors:
/// let result = some_db_call().await?; // DbErr -> InternalServerError via From
/// ```
#[derive(Debug)]
pub struct InternalServerError(pub eyre::Report);

impl std::fmt::Display for InternalServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl<E> From<E> for InternalServerError
where
    E: Into<eyre::Report>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

/// Proxy struct to generate the correct API schema for InternalServerError
#[derive(ToSchema, Serialize, Deserialize)]
#[salvo(schema(description = "Internal server error"))]
pub struct InternalServerErrorSchema {
    pub error: String,
}

#[async_trait]
impl Writer for InternalServerError {
    async fn write(self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        // Always log the full error for debugging/monitoring
        tracing::error!(error = ?self.0, "Internal server error");

        // Obfuscate in release builds, show details in debug builds
        let response = if cfg!(debug_assertions) {
            format!("DEBUG: {:?}", self.0)
        } else {
            "Internal server error".to_string()
        };

        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        res.render(Text::Plain(response));
    }
}
