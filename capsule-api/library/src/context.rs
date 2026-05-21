use std::collections::HashSet;

use async_graphql::{Error, ErrorExtensions, ServerError};
use auth::{claims::Scope, errors::ClaimValidationError, roles::UserRole, service::AuthService};
use salvo::http::HeaderMap;
use sea_orm::DatabaseConnection;
use secrecy::ExposeSecret;

#[derive(Debug, Clone)]
pub enum UserType {
    /// A normal user (user_id)
    User(String),
    /// An admin user (user_id)
    Admin(String),
    /// A guest user
    Guest,
}

#[derive(Debug, Clone)]
pub struct UserContext {
    user_type: UserType,
    #[allow(dead_code)]
    scopes: HashSet<Scope>,
}

impl UserContext {
    pub fn from_salvo_headers(
        headers: &HeaderMap,
        auth_service: &AuthService,
    ) -> Result<Self, UserContextError> {
        let mut scopes = None;
        let user_type: UserType = match auth::utils::headers::get_token_from_headers(headers) {
            Ok(token) => {
                let claims = auth_service.get_claims(token.expose_secret())?;
                claims.validate_access_token()?;

                scopes = Some(claims.scopes);

                match claims.role {
                    UserRole::User => UserType::User(claims.sub),
                    UserRole::Admin => UserType::Admin(claims.sub),
                }
            }
            Err(ClaimValidationError::TokenMissing) => UserType::Guest,
            Err(e) => return Err(e.into()),
        };

        Ok(Self {
            user_type,
            scopes: scopes.map_or_else(HashSet::new, |scopes| scopes.into_iter().collect()),
        })
    }
    pub fn user_id(&self) -> Result<&String, Error> {
        match &self.user_type {
            UserType::User(id) | UserType::Admin(id) => Ok(id),
            UserType::Guest => Err(Error::new("Unauthorized: Login required")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DbContext {
    pub conn: DatabaseConnection,
}

#[derive(Debug, Clone)]
pub struct AppContext {
    pub user: UserContext,
    pub db: DbContext,
}

pub struct UserContextError(ClaimValidationError);

impl From<ClaimValidationError> for UserContextError {
    fn from(err: ClaimValidationError) -> Self {
        Self(err)
    }
}

impl ErrorExtensions for UserContextError {
    // lets define our base extensions
    fn extend(&self) -> Error {
        let error = &self.0;
        Error::new(format!("{}", error)).extend_with(|_err, e| match error {
            ClaimValidationError::TokenExpired => e.set("code", "TOKEN_MISSING"),
            ClaimValidationError::TokenInvalid(msg) => {
                e.set("code", format!("TOKEN_INVALID ({})", msg))
            }
            ClaimValidationError::TokenMissing => e.set("code", "TOKEN_EXPIRED"),
            ClaimValidationError::UnexpectedHeaderFormat => {
                e.set("code", "UNEXPECTED_HEADER_FORMAT")
            }
            ClaimValidationError::InvalidScopes => e.set("code", "INVALID_SCOPES"),
        })
    }
}

impl From<UserContextError> for ServerError {
    fn from(err: UserContextError) -> Self {
        let field_error = err.extend();
        ServerError::new(field_error.message.as_str(), None)
    }
}
