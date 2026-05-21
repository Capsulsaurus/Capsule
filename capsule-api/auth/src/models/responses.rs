use super::UserProfile;
use super::errors::*;
use crate::claims::Claims;
use crate::errors::TotpEnrollError;
use crate::errors::TotpVerificationError;
use crate::errors::{ClaimValidationError, LoginError, RegisterError};
use derive_more::From;
use model::errors::InternalServerError;
use model::passkey::Passkey;
use salvo::http::StatusCode;
use salvo::oapi::{EndpointOutRegister, ToSchema};
use salvo::prelude::*;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, ToSchema, Debug)]
pub struct Device {
    pub id: String,
    pub created_at: i64,
    pub last_active_at: i64,
    pub user_agent: Option<String>,
    pub ip_address: Option<String>,
    pub is_current: bool,
}

#[derive(Serialize, Deserialize, ToSchema, Debug)]
pub struct TokenResponse {
    #[salvo(schema(value_type = String))]
    #[serde(serialize_with = "crate::models::serialize_secret")]
    pub access_token: SecretString,
    #[salvo(schema(value_type = String))]
    #[serde(serialize_with = "crate::models::serialize_secret")]
    pub refresh_token: SecretString,
    /// E.g. "Bearer"
    pub token_type: String,
    /// Access token expiry in seconds
    pub expires_by: u64,
}

#[derive(From, Debug)]
pub enum RegisterUserResponses {
    Success(TokenResponse),
    BadRequest(BadRegisterUserRequestError),
    UserAlreadyExists,
    RateLimited(u64),
    InternalServerError(InternalServerError),
}

impl From<Result<TokenResponse, RegisterError>> for RegisterUserResponses {
    fn from(result: Result<TokenResponse, RegisterError>) -> Self {
        match result {
            Ok(token) => token.into(),
            Err(e) => e.into(),
        }
    }
}

impl From<RegisterError> for RegisterUserResponses {
    fn from(e: RegisterError) -> Self {
        match e {
            RegisterError::UserAlreadyExists => Self::UserAlreadyExists,
            RegisterError::BadRequest(e) => Self::BadRequest(e),
            RegisterError::Unexpected(e) => Self::InternalServerError(e),
        }
    }
}

#[async_trait]
impl Writer for RegisterUserResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success(token_response) => {
                res.status_code(StatusCode::CREATED);
                res.render(Json(token_response));
            }
            Self::BadRequest(e) => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(e));
            }
            Self::UserAlreadyExists => {
                res.status_code(StatusCode::CONFLICT);
                res.render(Json(ApiError::new("User already exists")));
            }
            Self::RateLimited(retry_after) => {
                res.status_code(StatusCode::TOO_MANY_REQUESTS);
                res.headers_mut().insert(
                    salvo::http::header::RETRY_AFTER,
                    retry_after.to_string().parse().unwrap(),
                );
                res.render(Json(ApiError::new("Too many requests")));
            }
            Self::InternalServerError(e) => e.write(req, depot, res).await,
        }
    }
}

impl EndpointOutRegister for RegisterUserResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("201"),
            salvo::oapi::Response::new("Success - user registered and tokens returned")
                .add_content(
                    "application/json",
                    salvo::oapi::Content::new(TokenResponse::to_schema(components)),
                ),
        );
        operation.responses.insert(
            String::from("400"),
            salvo::oapi::Response::new("Bad request - invalid registration data"),
        );
        operation.responses.insert(
            String::from("409"),
            salvo::oapi::Response::new("User already exists"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

#[derive(From, Debug)]
pub enum LoginResponses {
    Success(TokenResponse),
    BadRequest,
    InvalidCredentials,
    AccountLocked,
    RateLimited(u64),
    InternalServerError(InternalServerError),
}

impl From<Result<TokenResponse, LoginError>> for LoginResponses {
    fn from(result: Result<TokenResponse, LoginError>) -> Self {
        match result {
            Ok(token) => token.into(),
            Err(e) => e.into(),
        }
    }
}

impl From<LoginError> for LoginResponses {
    fn from(e: LoginError) -> Self {
        match e {
            LoginError::InvalidCredentials => Self::InvalidCredentials,
            LoginError::AccountLocked => Self::AccountLocked,
            LoginError::RateLimited(r) => Self::RateLimited(r),
            LoginError::Unexpected(e) => Self::InternalServerError(e),
        }
    }
}

#[async_trait]
impl Writer for LoginResponses {
    async fn write(self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success(token_response) => {
                res.status_code(StatusCode::OK);
                res.render(Json(token_response));
            }
            Self::BadRequest => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(ApiError::new("Invalid request")));
            }
            Self::InvalidCredentials => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ApiError::new("Invalid credentials")));
            }
            Self::AccountLocked => {
                res.status_code(StatusCode::LOCKED);
                res.render(Json(ApiError::new(
                    "Account locked due to too many failed login attempts",
                )));
            }
            Self::RateLimited(retry_after) => {
                res.status_code(StatusCode::TOO_MANY_REQUESTS);
                res.headers_mut().insert(
                    salvo::http::header::RETRY_AFTER,
                    retry_after.to_string().parse().unwrap(),
                );
                res.render(Json(ApiError::new("Too many requests")));
            }
            Self::InternalServerError(e) => {
                e.write(_req, _depot, res).await;
            }
        }
    }
}

impl EndpointOutRegister for LoginResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Success - login successful").add_content(
                "application/json",
                salvo::oapi::Content::new(TokenResponse::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("400"),
            salvo::oapi::Response::new("Bad request"),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Invalid credentials"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

#[derive(From, Debug)]
pub enum RefreshTokenResponses {
    Success(TokenResponse),
    InvalidRefreshToken(String),
    InternalServerError(InternalServerError),
}

impl From<ClaimValidationError> for RefreshTokenResponses {
    fn from(error: ClaimValidationError) -> Self {
        Self::InvalidRefreshToken(error.to_string())
    }
}

impl From<Result<TokenResponse, InternalServerError>> for RefreshTokenResponses {
    fn from(result: Result<TokenResponse, InternalServerError>) -> Self {
        match result {
            Ok(token) => token.into(),
            Err(e) => e.into(),
        }
    }
}

#[async_trait]
impl Writer for RefreshTokenResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success(token_response) => {
                res.status_code(StatusCode::OK);
                res.render(Json(token_response));
            }
            Self::InvalidRefreshToken(e) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ApiError::new(e.to_string())));
            }
            Self::InternalServerError(e) => {
                e.write(req, depot, res).await;
                return;
            }
        }
    }
}

impl EndpointOutRegister for RefreshTokenResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Success - tokens refreshed").add_content(
                "application/json",
                salvo::oapi::Content::new(TokenResponse::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Invalid or expired refresh token"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

#[derive(From, Debug)]
pub enum ValidateTokenResponses {
    Valid(String),
    Invalid(ClaimValidationError),
}

impl From<Result<Claims, ClaimValidationError>> for ValidateTokenResponses {
    fn from(result: Result<Claims, ClaimValidationError>) -> Self {
        match result {
            Ok(claims) => Self::Valid(claims.sub),
            Err(e) => e.into(),
        }
    }
}

#[async_trait]
impl Writer for ValidateTokenResponses {
    async fn write(self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Valid(user_id) => {
                res.status_code(StatusCode::OK);
                res.render(Json(user_id));
            }
            Self::Invalid(e) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ApiError::new(e.to_string())));
            }
        }
    }
}

impl EndpointOutRegister for ValidateTokenResponses {
    fn register(_components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Token is valid - returns user ID"),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Invalid or expired token"),
        );
    }
}

#[derive(From, Debug)]
pub enum ResetPasswordRequestResponses {
    Success,
    BadRequest,
    RateLimited(u64),
    InternalServerError(InternalServerError),
}

#[async_trait]
impl Writer for ResetPasswordRequestResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success => {
                res.status_code(StatusCode::OK);
                res.render(Json(ApiError::new("Password reset request sent")));
            }
            Self::BadRequest => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(ApiError::new("Invalid request")));
            }
            Self::RateLimited(retry_after) => {
                res.status_code(StatusCode::TOO_MANY_REQUESTS);
                res.headers_mut().insert(
                    salvo::http::header::RETRY_AFTER,
                    retry_after.to_string().parse().unwrap(),
                );
                res.render(Json(ApiError::new("Too many requests")));
            }
            Self::InternalServerError(e) => e.write(req, depot, res).await,
        }
    }
}

impl EndpointOutRegister for ResetPasswordRequestResponses {
    fn register(_components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Password reset email sent (if user exists)"),
        );
        operation.responses.insert(
            String::from("400"),
            salvo::oapi::Response::new("Bad request"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

#[derive(From, Debug)]
pub enum PasswordResetResponses {
    Success,
    InvalidToken,
    InvalidNewPassword,
    InternalServerError(InternalServerError),
}

#[async_trait]
impl Writer for PasswordResetResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success => {
                res.status_code(StatusCode::OK);
                res.render(Json(ApiError::new("Password reset successful")));
            }
            Self::InvalidToken => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(ApiError::new("Invalid or expired token")));
            }
            Self::InvalidNewPassword => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(ApiError::new("Invalid new password")));
            }
            Self::InternalServerError(e) => e.write(req, depot, res).await,
        }
    }
}

impl EndpointOutRegister for PasswordResetResponses {
    fn register(_components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Password reset successful"),
        );
        operation.responses.insert(
            String::from("400"),
            salvo::oapi::Response::new("Invalid or expired token, or invalid new password"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

#[derive(From, Debug)]
pub enum UserProfileResponses {
    Success(UserProfile),
    Unauthorized(ClaimValidationError),
    UserNotFound,
    InternalServerError(InternalServerError),
}

#[async_trait]
impl Writer for UserProfileResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success(user_profile) => {
                res.status_code(StatusCode::OK);
                res.render(Json(user_profile));
            }
            Self::Unauthorized(e) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ApiError::new(e.to_string())));
            }
            Self::UserNotFound => {
                res.status_code(StatusCode::NOT_FOUND);
                res.render(Json(ApiError::new("User not found")));
            }
            Self::InternalServerError(e) => e.write(req, depot, res).await,
        }
    }
}

impl EndpointOutRegister for UserProfileResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Success - returns user profile").add_content(
                "application/json",
                salvo::oapi::Content::new(UserProfile::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Unauthorized - invalid or missing token"),
        );
        operation.responses.insert(
            String::from("404"),
            salvo::oapi::Response::new("User not found"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

pub enum UpdateUserProfileResponses {
    Success(UserProfile),
    BadRequest,
    Unauthorized(ClaimValidationError),
    InvalidPassword,
    UserNotFound,
    InternalServerError(InternalServerError),
}

#[async_trait]
impl Writer for UpdateUserProfileResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success(profile) => {
                res.status_code(StatusCode::OK);
                res.render(Json(profile));
            }
            Self::BadRequest => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(ApiError::new("Invalid request")));
            }
            Self::Unauthorized(e) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ApiError::new(e.to_string())));
            }
            Self::InvalidPassword => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(ApiError::new("Invalid password")));
            }
            Self::UserNotFound => {
                res.status_code(StatusCode::NOT_FOUND);
                res.render(Json(ApiError::new("User not found")));
            }
            Self::InternalServerError(e) => e.write(req, depot, res).await,
        }
    }
}

impl EndpointOutRegister for UpdateUserProfileResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Success - returns updated user profile").add_content(
                "application/json",
                salvo::oapi::Content::new(UserProfile::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("400"),
            salvo::oapi::Response::new("Invalid request or password"),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Unauthorized - invalid or missing token"),
        );
        operation.responses.insert(
            String::from("404"),
            salvo::oapi::Response::new("User not found"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

#[derive(From)]
pub enum LogoutResponses {
    Success,
    Unauthorized(ClaimValidationError),
    InternalServerError(InternalServerError),
}

#[async_trait]
impl Writer for LogoutResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success => {
                res.status_code(StatusCode::OK);
                res.render(Json(ApiError::new("Logout successful")));
            }
            Self::Unauthorized(e) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ApiError::new(e.to_string())));
            }
            Self::InternalServerError(e) => e.write(req, depot, res).await,
        }
    }
}

impl EndpointOutRegister for LogoutResponses {
    fn register(_components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Logout successful"),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Unauthorized - invalid or missing token"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

// TOTP Response types

#[derive(Serialize, Deserialize, ToSchema, Debug)]
pub struct TotpEnrollmentResponse {
    pub provisioning_uri: String,
}

#[derive(Serialize, Deserialize, ToSchema, Debug)]
pub struct MfaRequiredResponse {
    #[salvo(schema(value_type = String))]
    #[serde(serialize_with = "crate::models::serialize_secret")]
    pub mfa_token: SecretString,
    pub message: String,
}

#[derive(From, Debug)]
pub enum TotpEnrollResponses {
    Success(TotpEnrollmentResponse),
    AlreadyEnabled,
    Unauthorized(ClaimValidationError),
    InternalServerError(InternalServerError),
}

impl From<Result<TotpEnrollmentResponse, TotpEnrollError>> for TotpEnrollResponses {
    fn from(result: Result<TotpEnrollmentResponse, TotpEnrollError>) -> Self {
        match result {
            Ok(response) => response.into(),
            Err(e) => e.into(),
        }
    }
}

impl From<TotpEnrollError> for TotpEnrollResponses {
    fn from(e: TotpEnrollError) -> Self {
        match e {
            TotpEnrollError::AlreadyEnabled => Self::AlreadyEnabled,
            TotpEnrollError::UserNotFound => {
                Self::InternalServerError(eyre::eyre!("User not found").into())
            }
            TotpEnrollError::Db(e) => Self::InternalServerError(e.into()),
            TotpEnrollError::Unexpected(e) => Self::InternalServerError(e.into()),
        }
    }
}

#[async_trait]
impl Writer for TotpEnrollResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success(enrollment) => {
                res.status_code(StatusCode::OK);
                res.render(Json(enrollment));
            }
            Self::AlreadyEnabled => {
                res.status_code(StatusCode::CONFLICT);
                res.render(Json(ApiError::new("TOTP is already enabled")));
            }
            Self::Unauthorized(e) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ApiError::new(e.to_string())));
            }
            Self::InternalServerError(e) => e.write(req, depot, res).await,
        }
    }
}

impl EndpointOutRegister for TotpEnrollResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Success - TOTP enrollment initiated").add_content(
                "application/json",
                salvo::oapi::Content::new(TotpEnrollmentResponse::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Unauthorized"),
        );
        operation.responses.insert(
            String::from("409"),
            salvo::oapi::Response::new("TOTP already enabled"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

#[derive(From, Debug)]
pub enum TotpVerifyEnrollmentResponses {
    Success,
    InvalidCode,
    NotEnrolled,
    Unauthorized(ClaimValidationError),
    InternalServerError(InternalServerError),
}

impl From<Result<(), TotpVerificationError>> for TotpVerifyEnrollmentResponses {
    fn from(result: Result<(), TotpVerificationError>) -> Self {
        match result {
            Ok(()) => Self::Success,
            Err(e) => e.into(),
        }
    }
}

impl From<TotpVerificationError> for TotpVerifyEnrollmentResponses {
    fn from(e: TotpVerificationError) -> Self {
        match e {
            TotpVerificationError::UserNotFound => {
                Self::InternalServerError(eyre::eyre!("User not found").into())
            }
            TotpVerificationError::InvalidCode => Self::InvalidCode,
            TotpVerificationError::NotEnabled => Self::NotEnrolled,
            TotpVerificationError::Unexpected(e) => Self::InternalServerError(e.into()),
        }
    }
}

#[async_trait]
impl Writer for TotpVerifyEnrollmentResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success => {
                res.status_code(StatusCode::OK);
                res.render(Json(ApiError::new("TOTP enabled successfully")));
            }
            Self::InvalidCode => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(ApiError::new("Invalid TOTP code")));
            }
            Self::NotEnrolled => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(ApiError::new("TOTP enrollment not initiated")));
            }
            Self::Unauthorized(e) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ApiError::new(e.to_string())));
            }
            Self::InternalServerError(e) => e.write(req, depot, res).await,
        }
    }
}

impl EndpointOutRegister for TotpVerifyEnrollmentResponses {
    fn register(_components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("TOTP enabled successfully"),
        );
        operation.responses.insert(
            String::from("400"),
            salvo::oapi::Response::new("Invalid TOTP code or enrollment not initiated"),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Unauthorized"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

#[derive(From, Debug)]
pub enum TotpVerifyLoginResponses {
    Success(TokenResponse),
    InvalidMfaToken,
    InvalidCode,
    NotEnrolled,
    MaxAttemptsExceeded,
    InternalServerError(InternalServerError),
}

impl From<Result<TokenResponse, TotpVerificationError>> for TotpVerifyLoginResponses {
    fn from(result: Result<TokenResponse, TotpVerificationError>) -> Self {
        match result {
            Ok(tokens) => tokens.into(),
            Err(e) => e.into(),
        }
    }
}

impl From<TotpVerificationError> for TotpVerifyLoginResponses {
    fn from(e: TotpVerificationError) -> Self {
        match e {
            TotpVerificationError::UserNotFound => {
                Self::InternalServerError(eyre::eyre!("User not found").into())
            }
            TotpVerificationError::NotEnabled => Self::NotEnrolled,
            TotpVerificationError::InvalidCode => Self::InvalidCode,
            TotpVerificationError::Unexpected(e) => Self::InternalServerError(e.into()),
        }
    }
}

#[async_trait]
impl Writer for TotpVerifyLoginResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success(tokens) => {
                res.status_code(StatusCode::OK);
                res.render(Json(tokens));
            }
            Self::InvalidMfaToken => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ApiError::new("Invalid MFA token")));
            }
            Self::InvalidCode => {
                res.status_code(StatusCode::FORBIDDEN);
                res.render(Json(ApiError::new("Invalid TOTP code")));
            }
            Self::NotEnrolled => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(ApiError::new("TOTP not enrolled")));
            }
            Self::MaxAttemptsExceeded => {
                res.status_code(StatusCode::TOO_MANY_REQUESTS);
                res.render(Json(ApiError::new(
                    "Maximum verification attempts exceeded",
                )));
            }
            Self::InternalServerError(e) => e.write(req, depot, res).await,
        }
    }
}

impl EndpointOutRegister for TotpVerifyLoginResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Success - login completed").add_content(
                "application/json",
                salvo::oapi::Content::new(TokenResponse::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Invalid or expired MFA token"),
        );
        operation.responses.insert(
            String::from("403"),
            salvo::oapi::Response::new("Invalid TOTP code"),
        );
        operation.responses.insert(
            String::from("400"),
            salvo::oapi::Response::new("TOTP not enrolled"),
        );
        operation.responses.insert(
            String::from("429"),
            salvo::oapi::Response::new("Maximum verification attempts exceeded"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

#[derive(From, Debug)]
pub enum TotpDisableResponses {
    Success,
    NotEnrolled,
    Unauthorized(ClaimValidationError),
    InvalidCode,
    InternalServerError(InternalServerError),
}

impl From<Result<(), TotpVerificationError>> for TotpDisableResponses {
    fn from(result: Result<(), TotpVerificationError>) -> Self {
        match result {
            Ok(()) => Self::Success,
            Err(e) => e.into(),
        }
    }
}

impl From<TotpVerificationError> for TotpDisableResponses {
    fn from(e: TotpVerificationError) -> Self {
        match e {
            TotpVerificationError::UserNotFound => {
                Self::InternalServerError(eyre::eyre!("User not found").into())
            }
            TotpVerificationError::NotEnabled => Self::NotEnrolled,
            TotpVerificationError::InvalidCode => Self::InvalidCode,
            TotpVerificationError::Unexpected(e) => Self::InternalServerError(e.into()),
        }
    }
}

#[async_trait]
impl Writer for TotpDisableResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success => {
                res.status_code(StatusCode::OK);
                res.render(Json(ApiError::new("TOTP enabled successfully")));
            }
            Self::NotEnrolled => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(ApiError::new("TOTP enrollment not initiated")));
            }
            Self::Unauthorized(e) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ApiError::new(e.to_string())));
            }
            Self::InvalidCode => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ApiError::new("Invalid TOTP code")));
            }
            Self::InternalServerError(e) => e.write(req, depot, res).await,
        }
    }
}

impl EndpointOutRegister for TotpDisableResponses {
    fn register(_components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("TOTP enabled successfully"),
        );
        operation.responses.insert(
            String::from("400"),
            salvo::oapi::Response::new("Invalid TOTP code or enrollment not initiated"),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Invalid or expired MFA token"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

#[derive(From, Debug)]
pub enum GetDevicesResponses {
    Success(Vec<Device>),
    Unauthorized(ClaimValidationError),
    InternalServerError(InternalServerError),
}

#[async_trait]
impl Writer for GetDevicesResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success(devices) => {
                res.status_code(StatusCode::OK);
                res.render(Json(devices));
            }
            Self::Unauthorized(e) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ApiError::new(e.to_string())));
            }
            Self::InternalServerError(e) => e.write(req, depot, res).await,
        }
    }
}

impl EndpointOutRegister for GetDevicesResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Success - returns list of active sessions").add_content(
                "application/json",
                salvo::oapi::Content::new(Vec::<Device>::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Unauthorized"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

// ========================================
// Passkey
// ========================================

use crate::errors::{PasskeyAuthenticationError, PasskeyManagementError, PasskeyRegistrationError};
// use webauthn_rs::prelude::{CreationChallengeActions, RequestChallengeActions};

#[derive(Serialize, Deserialize, ToSchema, Debug)]
pub struct PasskeyModel {
    /// Passkey ID
    pub id: String,
    /// Passkey name
    pub name: String,
    /// Creation timestamp (RFC 3339)
    pub created_at: String,
    /// Last used timestamp (RFC 3339)
    pub last_used_at: Option<String>,
}

impl From<Passkey> for PasskeyModel {
    fn from(passkey: Passkey) -> Self {
        Self {
            id: passkey.id,
            name: passkey.name,
            created_at: passkey.created_at.to_rfc3339(),
            last_used_at: passkey.last_used_at.map(|t| t.to_rfc3339()),
        }
    }
}

#[derive(Debug)]
pub enum PasskeyRegistrationStartResponses {
    Success(serde_json::Value),
    UserNotFound,
    AlreadyExists,
    RegistrationFailed(String),
    Unauthorized(String),
    InternalServerError(InternalServerError),
}

impl From<Result<serde_json::Value, PasskeyRegistrationError>>
    for PasskeyRegistrationStartResponses
{
    fn from(result: Result<serde_json::Value, PasskeyRegistrationError>) -> Self {
        match result {
            Ok(ccr) => Self::Success(ccr),
            Err(e) => e.into(),
        }
    }
}

impl From<PasskeyRegistrationError> for PasskeyRegistrationStartResponses {
    fn from(e: PasskeyRegistrationError) -> Self {
        match e {
            PasskeyRegistrationError::UserNotFound => Self::UserNotFound,
            PasskeyRegistrationError::AlreadyExists => Self::AlreadyExists,
            PasskeyRegistrationError::RegistrationFailed(msg) => Self::RegistrationFailed(msg),
            PasskeyRegistrationError::LimitReached(msg) => Self::RegistrationFailed(msg),
            PasskeyRegistrationError::InvalidChallenge => {
                Self::InternalServerError(eyre::eyre!("Invalid challenge state").into())
            }
            PasskeyRegistrationError::Db(e) => Self::InternalServerError(e.into()),
            PasskeyRegistrationError::Unexpected(e) => Self::InternalServerError(e.into()),
        }
    }
}

impl From<InternalServerError> for PasskeyRegistrationStartResponses {
    fn from(e: InternalServerError) -> Self {
        Self::InternalServerError(e)
    }
}

#[async_trait]
impl Writer for PasskeyRegistrationStartResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success(ccr) => {
                res.status_code(StatusCode::OK);
                res.render(Json(ccr));
            }
            Self::UserNotFound => {
                res.status_code(StatusCode::NOT_FOUND);
                res.render(Json(ApiError::new("User not found")));
            }
            Self::AlreadyExists => {
                res.status_code(StatusCode::CONFLICT);
                res.render(Json(ApiError::new("Passkey already exists")));
            }
            Self::RegistrationFailed(msg) => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(ApiError::new(msg)));
            }
            Self::Unauthorized(msg) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ApiError::new(msg)));
            }
            Self::InternalServerError(e) => e.write(req, depot, res).await,
        }
    }
}

impl EndpointOutRegister for PasskeyRegistrationStartResponses {
    fn register(_components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Registration started").add_content(
                "application/json",
                // salvo::oapi::Content::new(CreationChallengeActions::to_schema(components)), // Webauthn types might not impl ToSchema?
                salvo::oapi::Content::new(salvo::oapi::Object::new()),
            ),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Unauthorized"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

#[derive(Debug)]
pub enum PasskeyRegistrationFinishResponses {
    Success,
    RegistrationFailed(String),
    Unauthorized(String),
    InternalServerError(InternalServerError),
}

impl From<Result<(), PasskeyRegistrationError>> for PasskeyRegistrationFinishResponses {
    fn from(result: Result<(), PasskeyRegistrationError>) -> Self {
        match result {
            Ok(()) => Self::Success,
            Err(e) => e.into(),
        }
    }
}

impl From<PasskeyRegistrationError> for PasskeyRegistrationFinishResponses {
    fn from(e: PasskeyRegistrationError) -> Self {
        match e {
            PasskeyRegistrationError::UserNotFound => {
                Self::RegistrationFailed("User not found".into())
            }
            PasskeyRegistrationError::AlreadyExists => {
                Self::RegistrationFailed("Passkey already exists".into())
            }
            PasskeyRegistrationError::RegistrationFailed(msg) => Self::RegistrationFailed(msg),
            PasskeyRegistrationError::LimitReached(msg) => Self::RegistrationFailed(msg),
            PasskeyRegistrationError::InvalidChallenge => {
                Self::RegistrationFailed("Invalid challenge".into())
            }
            PasskeyRegistrationError::Db(e) => Self::InternalServerError(e.into()),
            PasskeyRegistrationError::Unexpected(e) => Self::InternalServerError(e.into()),
        }
    }
}

impl From<InternalServerError> for PasskeyRegistrationFinishResponses {
    fn from(e: InternalServerError) -> Self {
        Self::InternalServerError(e)
    }
}

#[async_trait]
impl Writer for PasskeyRegistrationFinishResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success => {
                res.status_code(StatusCode::OK);
                res.render(Json(ApiError::new("Passkey registered successfully")));
            }
            Self::RegistrationFailed(msg) => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(ApiError::new(msg)));
            }
            Self::Unauthorized(msg) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ApiError::new(msg)));
            }
            Self::InternalServerError(e) => e.write(req, depot, res).await,
        }
    }
}

impl EndpointOutRegister for PasskeyRegistrationFinishResponses {
    fn register(_components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Passkey registered"),
        );
        operation.responses.insert(
            String::from("400"),
            salvo::oapi::Response::new("Registration failed"),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Unauthorized"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

// Authentication
#[derive(From, Debug)]
pub enum PasskeyAuthStartResponses {
    Success(serde_json::Value),
    UserNotFound,
    InternalServerError(InternalServerError),
}

impl From<Result<(serde_json::Value, Option<String>), PasskeyAuthenticationError>>
    for PasskeyAuthStartResponses
{
    fn from(
        result: Result<(serde_json::Value, Option<String>), PasskeyAuthenticationError>,
    ) -> Self {
        match result {
            Ok((rcr, _)) => Self::Success(rcr),
            Err(e) => e.into(),
        }
    }
}

impl From<PasskeyAuthenticationError> for PasskeyAuthStartResponses {
    fn from(e: PasskeyAuthenticationError) -> Self {
        match e {
            PasskeyAuthenticationError::UserNotFound => Self::UserNotFound,
            PasskeyAuthenticationError::ConstraintViolation(msg) => {
                Self::InternalServerError(eyre::eyre!(msg).into())
            }
            PasskeyAuthenticationError::InvalidCredential => {
                Self::InternalServerError(eyre::eyre!("Invalid credential").into())
            }
            PasskeyAuthenticationError::Db(e) => Self::InternalServerError(e.into()),
            PasskeyAuthenticationError::Unexpected(e) => Self::InternalServerError(e.into()),
        }
    }
}

#[async_trait]
impl Writer for PasskeyAuthStartResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success(rcr) => {
                res.status_code(StatusCode::OK);
                res.render(Json(rcr));
            }
            Self::UserNotFound => {
                res.status_code(StatusCode::NOT_FOUND);
                res.render(Json(ApiError::new("User not found")));
            }
            Self::InternalServerError(e) => e.write(req, depot, res).await,
        }
    }
}

impl EndpointOutRegister for PasskeyAuthStartResponses {
    fn register(_components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Authentication started"),
        );
        operation.responses.insert(
            String::from("404"),
            salvo::oapi::Response::new("User not found"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

#[derive(From, Debug)]
pub enum PasskeyAuthFinishResponses {
    Success(TokenResponse),
    InvalidCredential,
    InternalServerError(InternalServerError),
}

impl From<PasskeyAuthenticationError> for PasskeyAuthFinishResponses {
    fn from(e: PasskeyAuthenticationError) -> Self {
        match e {
            PasskeyAuthenticationError::UserNotFound => Self::InvalidCredential,
            PasskeyAuthenticationError::ConstraintViolation(_) => Self::InvalidCredential,
            PasskeyAuthenticationError::InvalidCredential => Self::InvalidCredential,
            PasskeyAuthenticationError::Db(e) => Self::InternalServerError(e.into()),
            PasskeyAuthenticationError::Unexpected(e) => Self::InternalServerError(e.into()),
        }
    }
}

#[async_trait]
impl Writer for PasskeyAuthFinishResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success(tokens) => {
                res.status_code(StatusCode::OK);
                res.render(Json(tokens));
            }
            Self::InvalidCredential => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ApiError::new("Invalid credential")));
            }
            Self::InternalServerError(e) => e.write(req, depot, res).await,
        }
    }
}

impl EndpointOutRegister for PasskeyAuthFinishResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Authentication successful").add_content(
                "application/json",
                salvo::oapi::Content::new(TokenResponse::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Invalid credential"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

#[derive(From, Debug)]
pub enum PasskeyListResponses {
    Success(Vec<PasskeyModel>),
    NotFound,
    Unauthorized(String),
    InternalServerError(InternalServerError),
}

impl From<PasskeyManagementError> for PasskeyListResponses {
    fn from(e: PasskeyManagementError) -> Self {
        match e {
            PasskeyManagementError::UserNotFound => Self::NotFound,
            PasskeyManagementError::NotFound => Self::Success(vec![]),
            PasskeyManagementError::Db(e) => Self::InternalServerError(e.into()),
            PasskeyManagementError::Unexpected(e) => Self::InternalServerError(e.into()),
        }
    }
}

#[async_trait]
impl Writer for PasskeyListResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success(models) => {
                res.status_code(StatusCode::OK);
                res.render(Json(models));
            }
            Self::NotFound => {
                res.status_code(StatusCode::NOT_FOUND);
                res.render(Json(ApiError::new("User not found")));
            }
            Self::Unauthorized(msg) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ApiError::new(msg)));
            }
            Self::InternalServerError(e) => e.write(req, depot, res).await,
        }
    }
}

impl EndpointOutRegister for PasskeyListResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("List passkeys").add_content(
                "application/json",
                salvo::oapi::Content::new(Vec::<PasskeyModel>::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Unauthorized"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

#[derive(From, Debug)]
pub enum PasskeyManageResponses {
    Success,
    NotFound,
    Unauthorized(String),
    InternalServerError(InternalServerError),
}

impl From<Result<(), PasskeyManagementError>> for PasskeyManageResponses {
    fn from(result: Result<(), PasskeyManagementError>) -> Self {
        match result {
            Ok(()) => Self::Success,
            Err(e) => e.into(),
        }
    }
}

impl From<PasskeyManagementError> for PasskeyManageResponses {
    fn from(e: PasskeyManagementError) -> Self {
        match e {
            PasskeyManagementError::UserNotFound => Self::NotFound,
            PasskeyManagementError::NotFound => Self::NotFound,
            PasskeyManagementError::Db(e) => Self::InternalServerError(e.into()),
            PasskeyManagementError::Unexpected(e) => Self::InternalServerError(e.into()),
        }
    }
}

#[async_trait]
impl Writer for PasskeyManageResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success => {
                res.status_code(StatusCode::OK);
                res.render(Json(ApiError::new("Success")));
            }
            Self::NotFound => {
                res.status_code(StatusCode::NOT_FOUND);
                res.render(Json(ApiError::new("Passkey or User not found")));
            }
            Self::Unauthorized(msg) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ApiError::new(msg)));
            }
            Self::InternalServerError(e) => e.write(req, depot, res).await,
        }
    }
}

impl EndpointOutRegister for PasskeyManageResponses {
    fn register(_components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Operation successful"),
        );
        operation.responses.insert(
            String::from("404"),
            salvo::oapi::Response::new("Resource not found"),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Unauthorized"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}
