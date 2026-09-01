use derive_more::From;
use model::errors::InternalServerError;
use model::passkey::Passkey;
use salvo::oapi::ToSchema;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use super::UserProfile;
use super::errors::*;
use crate::claims::Claims;
use crate::errors::{
    ClaimValidationError, LoginError, RegisterError, TotpEnrollError, TotpVerificationError,
};

/// One active session in the session listing (slice `S-C13`), carrying **both** identifiers
/// the support bundle needs (slice `S-N3`).
///
/// [`Device::id`] is the *session* id and [`Device::device_id`] is the *device* id — two
/// distinct identifier spaces that must both be present, because a support report pairs them
/// (`{cohort_hash, [(device_id, session_id, first_seen, last_seen)]}`) and one physical
/// device accumulates several session ids over its life.
#[derive(Serialize, Deserialize, ToSchema, Debug)]
pub struct Device {
    /// The **session** id. Historically the only identifier on this surface; it keeps its
    /// name so existing clients keep deserializing, and `device_id` is added beside it rather
    /// than renaming this field into something it is not.
    pub id: String,
    /// The **device** id this session was opened from (slice `S-N3`): the UUID naming an
    /// entry in the user's device directory, as asserted by the client at session creation,
    /// or `None` when it asserted none. Paired with `id` this yields the support bundle's
    /// `(device_id, session_id)` rows. Client-asserted and therefore never an authorization
    /// input.
    #[serde(default)]
    pub device_id: Option<String>,
    pub created_at: i64,
    pub last_active_at: i64,
    pub user_agent: Option<String>,
    pub ip_address: Option<String>,
    pub is_current: bool,
    /// The advisory device-cohort hash asserted when this session was created (slice
    /// `S-C13`), or `None` if the client did not assert one. A grouping aid only — clients
    /// group the ledger by this value; it carries no authority.
    #[serde(default)]
    pub cohort_hash: Option<String>,
}

/// One entry of the durable device-cohort map surfaced alongside the session listing (slice
/// `S-C13`). Lets a client label a cohort "a device you've used before (last seen …)" even
/// when its earlier sessions have expired — the map outlives sessions.
#[derive(Serialize, Deserialize, ToSchema, Debug)]
pub struct DeviceCohort {
    /// The advisory cohort hash (opaque; the client groups its sessions by this).
    pub cohort_hash: String,
    /// First time this cohort was seen for the user (Unix seconds).
    pub first_seen: i64,
    /// Most recent time this cohort was seen for the user (Unix seconds).
    pub last_seen: i64,
}

impl From<service::cohort::CohortObservation> for DeviceCohort {
    fn from(o: service::cohort::CohortObservation) -> Self {
        Self {
            cohort_hash: o.cohort_hash,
            first_seen: o.first_seen,
            last_seen: o.last_seen,
        }
    }
}

/// The session-listing surface (slice `S-C13`): the user's active sessions, each carrying its
/// advisory cohort, plus the durable cohort map. Clients group the ledger by cohort.
#[derive(Serialize, Deserialize, ToSchema, Debug)]
pub struct SessionListingResponse {
    /// Active sessions ("devices"), each annotated with its per-session cohort.
    pub devices: Vec<Device>,
    /// The durable cohort map (persists beyond session expiry).
    pub cohorts: Vec<DeviceCohort>,
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

capsule_wire::salvo_responses! {
    RegisterUserResponses {
        Success(token_response) => 201, json(token_response),
            doc("Success - user registered and tokens returned", schema = TokenResponse);
        BadRequest(e) => 400, json(e), doc("Bad request - invalid registration data");
        // Reference wiring for the i18n error-code contract: a stable catalog code
        // travels beside the English detail.
        UserAlreadyExists {} => 409, json(ApiError::with_code(
            "User already exists",
            capsule_i18n::error_codes::AUTH_USER_ALREADY_EXISTS,
        )), doc("User already exists");
        RateLimited(retry_after) => 429,
            retry_after(retry_after) json(ApiError::new("Too many requests")),
            undocumented();
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
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

capsule_wire::salvo_responses! {
    LoginResponses {
        Success(token_response) => 200, json(token_response),
            doc("Success - login successful", schema = TokenResponse);
        BadRequest {} => 400, json(ApiError::new("Invalid request")), doc("Bad request");
        InvalidCredentials {} => 401, json(ApiError::new("Invalid credentials")),
            doc("Invalid credentials");
        AccountLocked {} => 423,
            json(ApiError::new("Account locked due to too many failed login attempts")),
            undocumented();
        RateLimited(retry_after) => 429,
            retry_after(retry_after) json(ApiError::new("Too many requests")),
            undocumented();
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
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

capsule_wire::salvo_responses! {
    RefreshTokenResponses {
        Success(token_response) => 200, json(token_response),
            doc("Success - tokens refreshed", schema = TokenResponse);
        InvalidRefreshToken(e) => 401, json(ApiError::new(e)),
            doc("Invalid or expired refresh token");
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
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

capsule_wire::salvo_responses! {
    ValidateTokenResponses {
        Valid(user_id) => 200, json(user_id),
            doc("Token is valid - returns user ID");
        Invalid(e) => 401, json(ApiError::new(e.to_string())),
            doc("Invalid or expired token");
    }
}

#[derive(From, Debug)]
pub enum ResetPasswordRequestResponses {
    Success,
    BadRequest,
    RateLimited(u64),
    InternalServerError(InternalServerError),
}

capsule_wire::salvo_responses! {
    ResetPasswordRequestResponses {
        Success {} => 200, json(ApiError::new("Password reset request sent")),
            doc("Password reset email sent (if user exists)");
        BadRequest {} => 400, json(ApiError::new("Invalid request")), doc("Bad request");
        RateLimited(retry_after) => 429,
            retry_after(retry_after) json(ApiError::new("Too many requests")),
            undocumented();
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
    }
}

#[derive(From, Debug)]
pub enum PasswordResetResponses {
    Success,
    InvalidToken,
    InvalidNewPassword,
    InternalServerError(InternalServerError),
}

capsule_wire::salvo_responses! {
    PasswordResetResponses {
        Success {} => 200, json(ApiError::new("Password reset successful")),
            doc("Password reset successful");
        InvalidToken {} => 400, json(ApiError::new("Invalid or expired token")),
            doc("Invalid or expired token, or invalid new password");
        InvalidNewPassword {} => 400, json(ApiError::new("Invalid new password")),
            undocumented();
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
    }
}

#[derive(From, Debug)]
pub enum UserProfileResponses {
    Success(UserProfile),
    Unauthorized(ClaimValidationError),
    UserNotFound,
    InternalServerError(InternalServerError),
}

capsule_wire::salvo_responses! {
    UserProfileResponses {
        Success(user_profile) => 200, json(user_profile),
            doc("Success - returns user profile", schema = UserProfile);
        Unauthorized(e) => 401, json(ApiError::new(e.to_string())),
            doc("Unauthorized - invalid or missing token");
        UserNotFound {} => 404, json(ApiError::new("User not found")),
            doc("User not found");
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
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

capsule_wire::salvo_responses! {
    UpdateUserProfileResponses {
        Success(profile) => 200, json(profile),
            doc("Success - returns updated user profile", schema = UserProfile);
        BadRequest {} => 400, json(ApiError::new("Invalid request")),
            doc("Invalid request or password");
        Unauthorized(e) => 401, json(ApiError::new(e.to_string())),
            doc("Unauthorized - invalid or missing token");
        InvalidPassword {} => 400, json(ApiError::new("Invalid password")),
            undocumented();
        UserNotFound {} => 404, json(ApiError::new("User not found")),
            doc("User not found");
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
    }
}

#[derive(From)]
pub enum LogoutResponses {
    Success,
    Unauthorized(ClaimValidationError),
    InternalServerError(InternalServerError),
}

capsule_wire::salvo_responses! {
    LogoutResponses {
        Success {} => 200, json(ApiError::new("Logout successful")),
            doc("Logout successful");
        Unauthorized(e) => 401, json(ApiError::new(e.to_string())),
            doc("Unauthorized - invalid or missing token");
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
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

capsule_wire::salvo_responses! {
    TotpEnrollResponses {
        Success(enrollment) => 200, json(enrollment),
            doc("Success - TOTP enrollment initiated", schema = TotpEnrollmentResponse);
        AlreadyEnabled {} => 409, json(ApiError::new("TOTP is already enabled")),
            doc("TOTP already enabled");
        Unauthorized(e) => 401, json(ApiError::new(e.to_string())), doc("Unauthorized");
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
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

capsule_wire::salvo_responses! {
    TotpVerifyEnrollmentResponses {
        Success {} => 200, json(ApiError::new("TOTP enabled successfully")),
            doc("TOTP enabled successfully");
        InvalidCode {} => 400, json(ApiError::new("Invalid TOTP code")),
            doc("Invalid TOTP code or enrollment not initiated");
        NotEnrolled {} => 400, json(ApiError::new("TOTP enrollment not initiated")),
            undocumented();
        Unauthorized(e) => 401, json(ApiError::new(e.to_string())), doc("Unauthorized");
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
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

capsule_wire::salvo_responses! {
    TotpVerifyLoginResponses {
        Success(tokens) => 200, json(tokens),
            doc("Success - login completed", schema = TokenResponse);
        InvalidMfaToken {} => 401, json(ApiError::new("Invalid MFA token")),
            doc("Invalid or expired MFA token");
        InvalidCode {} => 403, json(ApiError::new("Invalid TOTP code")),
            doc("Invalid TOTP code");
        NotEnrolled {} => 400, json(ApiError::new("TOTP not enrolled")),
            doc("TOTP not enrolled");
        MaxAttemptsExceeded {} => 429,
            json(ApiError::new("Maximum verification attempts exceeded")),
            doc("Maximum verification attempts exceeded");
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
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

capsule_wire::salvo_responses! {
    TotpDisableResponses {
        Success {} => 200, json(ApiError::new("TOTP enabled successfully")),
            doc("TOTP enabled successfully");
        NotEnrolled {} => 400, json(ApiError::new("TOTP enrollment not initiated")),
            doc("Invalid TOTP code or enrollment not initiated");
        Unauthorized(e) => 401, json(ApiError::new(e.to_string())),
            doc("Invalid or expired MFA token");
        InvalidCode {} => 401, json(ApiError::new("Invalid TOTP code")), undocumented();
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
    }
}

#[derive(From, Debug)]
pub enum GetDevicesResponses {
    Success(SessionListingResponse),
    Unauthorized(ClaimValidationError),
    InternalServerError(InternalServerError),
}

capsule_wire::salvo_responses! {
    GetDevicesResponses {
        Success(listing) => 200, json(listing), doc(
            "Success - returns active sessions (each with its advisory cohort) plus the durable cohort map",
            schema = SessionListingResponse
        );
        Unauthorized(e) => 401, json(ApiError::new(e.to_string())), doc("Unauthorized");
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
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
            created_at: passkey.created_at.to_string(),
            last_used_at: passkey.last_used_at.map(|t| t.to_string()),
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
            PasskeyRegistrationError::RegistrationFailed(msg)
            | PasskeyRegistrationError::LimitReached(msg) => Self::RegistrationFailed(msg),
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

capsule_wire::salvo_responses! {
    PasskeyRegistrationStartResponses {
        // The webauthn challenge types do not implement ToSchema, so the payload is
        // published as an untyped object.
        Success(ccr) => 200, json(ccr), doc("Registration started", schema = object);
        UserNotFound {} => 404, json(ApiError::new("User not found")), undocumented();
        AlreadyExists {} => 409, json(ApiError::new("Passkey already exists")),
            undocumented();
        RegistrationFailed(msg) => 400, json(ApiError::new(msg)), undocumented();
        Unauthorized(msg) => 401, json(ApiError::new(msg)), doc("Unauthorized");
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
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
            PasskeyRegistrationError::RegistrationFailed(msg)
            | PasskeyRegistrationError::LimitReached(msg) => Self::RegistrationFailed(msg),
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

capsule_wire::salvo_responses! {
    PasskeyRegistrationFinishResponses {
        Success {} => 200, json(ApiError::new("Passkey registered successfully")),
            doc("Passkey registered");
        RegistrationFailed(msg) => 400, json(ApiError::new(msg)),
            doc("Registration failed");
        Unauthorized(msg) => 401, json(ApiError::new(msg)), doc("Unauthorized");
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
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

capsule_wire::salvo_responses! {
    PasskeyAuthStartResponses {
        Success(rcr) => 200, json(rcr), doc("Authentication started");
        UserNotFound {} => 404, json(ApiError::new("User not found")),
            doc("User not found");
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
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
            PasskeyAuthenticationError::UserNotFound
            | PasskeyAuthenticationError::ConstraintViolation(_)
            | PasskeyAuthenticationError::InvalidCredential => Self::InvalidCredential,
            PasskeyAuthenticationError::Db(e) => Self::InternalServerError(e.into()),
            PasskeyAuthenticationError::Unexpected(e) => Self::InternalServerError(e.into()),
        }
    }
}

capsule_wire::salvo_responses! {
    PasskeyAuthFinishResponses {
        Success(tokens) => 200, json(tokens),
            doc("Authentication successful", schema = TokenResponse);
        InvalidCredential {} => 401, json(ApiError::new("Invalid credential")),
            doc("Invalid credential");
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
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

capsule_wire::salvo_responses! {
    PasskeyListResponses {
        Success(models) => 200, json(models),
            doc("List passkeys", schema = Vec<PasskeyModel>);
        NotFound {} => 404, json(ApiError::new("User not found")), undocumented();
        Unauthorized(msg) => 401, json(ApiError::new(msg)), doc("Unauthorized");
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
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
            PasskeyManagementError::UserNotFound | PasskeyManagementError::NotFound => {
                Self::NotFound
            }
            PasskeyManagementError::Db(e) => Self::InternalServerError(e.into()),
            PasskeyManagementError::Unexpected(e) => Self::InternalServerError(e.into()),
        }
    }
}

capsule_wire::salvo_responses! {
    PasskeyManageResponses {
        Success {} => 200, json(ApiError::new("Success")),
            doc("Operation successful");
        NotFound {} => 404, json(ApiError::new("Passkey or User not found")),
            doc("Resource not found");
        Unauthorized(msg) => 401, json(ApiError::new(msg)), doc("Unauthorized");
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
    }
}

#[cfg(test)]
mod tests {
    use capsule_wire::{BodyShape, WireResponses};
    use salvo::oapi::{Components, EndpointOutRegister, Operation, RefOr};

    use super::*;

    /// The `(status, description)` rows the taxonomy declares as published.
    fn declared<T: WireResponses>() -> Vec<(u16, String)> {
        let mut rows: Vec<_> = T::documented()
            .map(|spec| {
                (
                    spec.status.expect("a documented row carries a status"),
                    spec.description
                        .expect("a documented row carries a description")
                        .to_string(),
                )
            })
            .collect();
        rows.sort();
        rows
    }

    /// The `(status, description)` rows the generated OpenAPI registration actually emits.
    fn registered<T: WireResponses + EndpointOutRegister>() -> Vec<(u16, String)> {
        let mut components = Components::new();
        let mut operation = Operation::new();
        T::register(&mut components, &mut operation);
        let mut rows: Vec<_> = operation
            .responses
            .iter()
            .map(|(status, response)| {
                let description = match response {
                    RefOr::Type(response) => response.description.clone(),
                    RefOr::Ref(_) => panic!("a registered response is never a bare $ref"),
                };
                (
                    status.parse().expect("a registered key is a status code"),
                    description,
                )
            })
            .collect();
        rows.sort();
        rows
    }

    /// The invariant this slice buys: the OpenAPI document is *derived* from the taxonomy
    /// table, so the two can no longer disagree the way two hand-written impls could.
    macro_rules! assert_document_matches_taxonomy {
        ($($ty:ty),+ $(,)?) => {
            $(
                assert_eq!(
                    registered::<$ty>(),
                    declared::<$ty>(),
                    "{} publishes rows its taxonomy does not declare",
                    ::core::stringify!($ty),
                );
            )+
        };
    }

    #[test]
    fn every_taxonomy_publishes_exactly_what_it_declares() {
        assert_document_matches_taxonomy!(
            RegisterUserResponses,
            LoginResponses,
            RefreshTokenResponses,
            ValidateTokenResponses,
            ResetPasswordRequestResponses,
            PasswordResetResponses,
            UserProfileResponses,
            UpdateUserProfileResponses,
            LogoutResponses,
            TotpEnrollResponses,
            TotpVerifyEnrollmentResponses,
            TotpVerifyLoginResponses,
            TotpDisableResponses,
            GetDevicesResponses,
            PasskeyRegistrationStartResponses,
            PasskeyRegistrationFinishResponses,
            PasskeyAuthStartResponses,
            PasskeyAuthFinishResponses,
            PasskeyListResponses,
            PasskeyManageResponses,
        );
    }

    /// A documented row's status is unique within its taxonomy: two rows sharing a status
    /// would silently overwrite each other in the document.
    #[test]
    fn documented_statuses_are_unique_per_taxonomy() {
        let rows = declared::<UpdateUserProfileResponses>();
        let mut statuses: Vec<_> = rows.iter().map(|(status, _)| *status).collect();
        statuses.dedup();
        assert_eq!(statuses.len(), rows.len());
    }

    /// The login taxonomy is the worked example from the slice: a typed success payload, four
    /// error shapes, a rate-limit row the published document deliberately omits, and a status
    /// the delegated internal-error taxonomy owns.
    #[test]
    fn the_login_taxonomy_records_its_documentation_gaps() {
        let mut gaps: Vec<_> = LoginResponses::undocumented()
            .filter_map(|spec| spec.status)
            .collect();
        gaps.sort_unstable();
        assert_eq!(gaps, vec![423, 429]);

        let success = LoginResponses::RESPONSES
            .first()
            .expect("the taxonomy is non-empty");
        assert_eq!(success.status, Some(200));
        assert_eq!(success.body, BodyShape::Json);
        assert_eq!(success.schema, Some("TokenResponse"));

        let delegated = LoginResponses::RESPONSES
            .iter()
            .find(|spec| spec.body == BodyShape::Delegated && spec.status == Some(500))
            .expect("the delegated internal-error row is declared");
        assert_eq!(delegated.description, Some("Internal server error"));
    }
}
