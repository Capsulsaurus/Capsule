//! Device enrollment — the cross-device-add server surface (slice `S-C7` in the repo-root
//! `SLICES.md`; SSoT: <https://docs/design/device-enrollment/>).
//!
//! An existing signed-in device issues a single-use enrollment code (`POST /devices/enroll`)
//! — a **fresh** access token is required on top of a valid session, the server-visible proxy
//! for the doc's fresh local device authorization, so a stolen *stale* token alone cannot
//! enroll a rogue device. The new device redeems it (`POST /devices/enroll/redeem`) to open an
//! opaque **relay channel** (`.../enroll/channel/{channel_id}`) that carries the ceremony's
//! ephemeral-ECDH / key-transfer messages between the two devices — the crypto rides the
//! payloads opaquely; the server only relays them. The code is single-use, expires in 10
//! minutes, is rate-limited, and is deleted on redemption or expiry.
//!
//! The new device's directory entry lands through the **existing S-C9** signed-directory
//! publish (`POST /devices/directory`, see [`super::directory`]): device A re-publishes its
//! directory with B's entry at an advanced `directory_version`. S-C7 reuses that monotonic
//! publish rather than forking a second directory writer.
//!
//! Paths live under `devices/enroll`; plain `GET /auth/devices` is the session listing, and
//! `devices/directory` is the S-C9 surface — three distinct surfaces.

use capsule_i18n::error_codes;
use salvo::oapi::extract::JsonBody;
use salvo::prelude::*;
use serde::{Deserialize, Serialize};

use crate::claims::Claims;
use crate::enrollment;
use crate::errors::ClaimValidationError;
use crate::models::errors::ApiError;
use crate::state::AppState;
use crate::utils::headers::get_token_from_headers;

/// The issued enrollment code, displayed as a QR (full entropy) with a shorter,
/// rate-limited numeric text fallback.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct EnrollmentCodeResponse {
    /// The full-entropy code for the QR payload (URL-safe base64; ≥64 bits).
    pub code: String,
    /// The shorter transcribable fallback (safe because redemption is single-use,
    /// expiring, and rate-limited; channel integrity rests on the safety code).
    pub text_fallback: String,
    /// RFC 3339 expiry (10 minutes from issue).
    pub expires_at: String,
}

/// A redemption request from the new device.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct RedeemRequest {
    /// The scanned or typed enrollment code (either the QR form or the numeric fallback).
    pub code: String,
}

/// The established relay channel handle the ceremony continues over.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct ChannelResponse {
    /// Opaque relay-channel handle for the ephemeral-ECDH ceremony messages.
    pub channel_id: String,
}

/// A request to relay one opaque ceremony payload into a channel mailbox.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct RelaySendRequest {
    /// Target mailbox: `"a"` toward the initiator (device A), `"b"` toward the enrollee
    /// (device B).
    pub to: String,
    /// The opaque payload, relayed verbatim — the server never decodes it.
    pub payload: String,
}

/// The drained opaque payloads pending in a channel mailbox.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct RelayMessagesResponse {
    /// Payloads in arrival order; the mailbox is drained on read.
    pub messages: Vec<String>,
}

/// Resolve the caller's validated access-token claims, or the auth error to surface.
fn authenticate(req: &Request, state: &AppState) -> Result<Claims, ClaimValidationError> {
    let token = get_token_from_headers(req.headers())?;
    use secrecy::ExposeSecret;
    let claims = state.auth_service.get_claims(token.expose_secret())?;
    claims.validate_access_token()?;
    Ok(claims)
}

/// Best-effort source-IP bucket for the redemption rate limiter (redemption is
/// unauthenticated — the code is the only credential).
fn client_ip(req: &Request) -> String {
    req.header::<String>("X-Forwarded-For")
        .and_then(|v| v.split(',').next().map(|s| s.trim().to_string()))
        .or_else(|| req.header::<String>("X-Real-IP"))
        .unwrap_or_else(|| format!("{:?}", req.remote_addr()))
}

// ── issue ────────────────────────────────────────────────────────────────────

/// Responses for enrollment-code issuance.
pub(super) enum IssueResponses {
    /// Code issued.
    Ok(EnrollmentCodeResponse),
    /// No/invalid bearer token.
    Unauthorized(ClaimValidationError),
    /// The session lacks the fresh local device authorization the ceremony requires
    /// (`403 error.enrollment.local_auth_required`).
    LocalAuthRequired,
    /// The per-user issuance budget is exhausted (`429 error.enrollment.rate_limited`).
    RateLimited { retry_after: u64 },
    /// Server fault.
    Internal,
}

#[async_trait]
impl Writer for IssueResponses {
    async fn write(mut self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Ok(data) => {
                res.status_code(StatusCode::OK);
                Json(data).write(req, depot, res).await;
            }
            Self::Unauthorized(e) => e.write(req, depot, res).await,
            Self::LocalAuthRequired => {
                res.status_code(StatusCode::FORBIDDEN);
                res.render(Json(ApiError::with_code(
                    "Fresh local device authorization required to add a device",
                    error_codes::ENROLLMENT_LOCAL_AUTH_REQUIRED,
                )));
            }
            Self::RateLimited { retry_after } => {
                res.status_code(StatusCode::TOO_MANY_REQUESTS);
                if let Ok(v) = retry_after.to_string().parse() {
                    res.headers_mut()
                        .insert(salvo::http::header::RETRY_AFTER, v);
                }
                res.render(Json(ApiError::with_code(
                    "Too many enrollment-code requests",
                    error_codes::ENROLLMENT_RATE_LIMITED,
                )));
            }
            Self::Internal => {
                res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                res.render(Json(ApiError::new("Internal server error")));
            }
        }
    }
}

impl EndpointOutRegister for IssueResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Enrollment code issued").add_content(
                "application/json",
                salvo::oapi::Content::new(EnrollmentCodeResponse::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Missing or invalid access token"),
        );
        operation.responses.insert(
            String::from("403"),
            salvo::oapi::Response::new("Fresh local device authorization required"),
        );
        operation.responses.insert(
            String::from("429"),
            salvo::oapi::Response::new("Per-user issuance budget exhausted"),
        );
    }
}

/// Issue a single-use enrollment code from an existing, locally-authorized device.
#[endpoint(operation_id = "issue_enrollment_code", tags("auth"), security(("bearer" = [])))]
pub async fn issue_enrollment_code(req: &mut Request, depot: &mut Depot) -> IssueResponses {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");

    let claims = match authenticate(req, state) {
        Ok(c) => c,
        Err(e) => return IssueResponses::Unauthorized(e),
    };

    // Local-auth gate: the access token must be fresh. A valid but stale session token is not
    // sufficient to start a cross-device add (doc step 1 / stolen-token failure mode).
    let now = jiff::Timestamp::now().as_second();
    let age = now.saturating_sub(i64::try_from(claims.iat).unwrap_or(i64::MAX));
    if age > i64::try_from(enrollment::LOCAL_AUTH_FRESHNESS.as_secs()).unwrap_or(i64::MAX) {
        tracing::info!(user_id = %claims.sub, age, "enrollment issue refused: token not fresh");
        return IssueResponses::LocalAuthRequired;
    }

    // Per-user issuance rate limit.
    match state
        .session_manager
        .check_rate_limit(
            &format!("enroll_issue:{}", claims.sub),
            enrollment::MAX_ISSUE_PER_WINDOW,
            enrollment::CODE_TTL.as_secs(),
        )
        .await
    {
        Ok(rl) if rl.count > enrollment::MAX_ISSUE_PER_WINDOW => {
            return IssueResponses::RateLimited {
                retry_after: rl.window_ttl_secs,
            };
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!("enrollment issue rate-limit error: {e}");
            return IssueResponses::Internal;
        }
    }

    match enrollment::issue(&state.session_manager, &claims.sub, enrollment::CODE_TTL).await {
        Ok(issued) => {
            let expires_at = match jiff::Timestamp::from_second(issued.expires_at) {
                Ok(ts) => ts.to_string(),
                Err(e) => {
                    tracing::error!("enrollment expiry timestamp error: {e}");
                    return IssueResponses::Internal;
                }
            };
            IssueResponses::Ok(EnrollmentCodeResponse {
                code: issued.code,
                text_fallback: issued.text_fallback,
                expires_at,
            })
        }
        Err(e) => {
            tracing::error!("enrollment issue error: {e}");
            IssueResponses::Internal
        }
    }
}

// ── redeem ───────────────────────────────────────────────────────────────────

/// Responses for enrollment-code redemption.
pub(super) enum RedeemResponses {
    /// Channel established.
    Ok(ChannelResponse),
    /// The code is unknown, expired, already redeemed, or rate-limited —
    /// indistinguishable by design (`404 error.enrollment.code_refused`).
    Refused,
    /// Server fault.
    Internal,
}

#[async_trait]
impl Writer for RedeemResponses {
    async fn write(mut self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Ok(data) => {
                res.status_code(StatusCode::OK);
                Json(data).write(req, depot, res).await;
            }
            Self::Refused => {
                res.status_code(StatusCode::NOT_FOUND);
                res.render(Json(ApiError::with_code(
                    "Enrollment code could not be redeemed",
                    error_codes::ENROLLMENT_CODE_REFUSED,
                )));
            }
            Self::Internal => {
                res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                res.render(Json(ApiError::new("Internal server error")));
            }
        }
    }
}

impl EndpointOutRegister for RedeemResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Relay channel established").add_content(
                "application/json",
                salvo::oapi::Content::new(ChannelResponse::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("404"),
            salvo::oapi::Response::new(
                "Unknown, expired, redeemed, or rate-limited code (indistinguishable)",
            ),
        );
    }
}

/// Redeem an enrollment code from the new device (unauthenticated; the code is the
/// credential).
#[endpoint(operation_id = "redeem_enrollment_code", tags("auth"))]
pub async fn redeem_enrollment_code(
    req: &mut Request,
    depot: &mut Depot,
    body: JsonBody<RedeemRequest>,
) -> RedeemResponses {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");
    let source = client_ip(req);

    match enrollment::redeem(&state.session_manager, &body.code, &source).await {
        Ok(enrollment::RedeemOutcome::Established { channel_id }) => {
            RedeemResponses::Ok(ChannelResponse { channel_id })
        }
        Ok(enrollment::RedeemOutcome::Refused(_)) => RedeemResponses::Refused,
        Err(e) => {
            tracing::error!("enrollment redeem error: {e}");
            RedeemResponses::Internal
        }
    }
}

// ── relay channel ────────────────────────────────────────────────────────────

/// Responses for relaying a payload into a channel mailbox.
pub(super) enum RelaySendResponses {
    /// Payload accepted for relay.
    NoContent,
    /// The channel is unknown or expired (`404 error.enrollment.channel_not_found`).
    NoChannel,
    /// Unknown direction or a missing/oversized payload (`400 error.enrollment.relay_malformed`).
    Malformed(String),
    /// Server fault.
    Internal,
}

#[async_trait]
impl Writer for RelaySendResponses {
    async fn write(mut self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        match self {
            Self::NoContent => {
                res.status_code(StatusCode::NO_CONTENT);
            }
            Self::NoChannel => {
                res.status_code(StatusCode::NOT_FOUND);
                res.render(Json(ApiError::with_code(
                    "Enrollment relay channel not found",
                    error_codes::ENROLLMENT_CHANNEL_NOT_FOUND,
                )));
            }
            Self::Malformed(detail) => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(ApiError::with_code(
                    format!("Malformed relay request: {detail}"),
                    error_codes::ENROLLMENT_RELAY_MALFORMED,
                )));
            }
            Self::Internal => {
                res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                res.render(Json(ApiError::new("Internal server error")));
            }
        }
    }
}

impl EndpointOutRegister for RelaySendResponses {
    fn register(_components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("204"),
            salvo::oapi::Response::new("Opaque payload accepted for relay"),
        );
        operation.responses.insert(
            String::from("400"),
            salvo::oapi::Response::new("Unknown direction or missing/oversized payload"),
        );
        operation.responses.insert(
            String::from("404"),
            salvo::oapi::Response::new("Unknown or expired relay channel"),
        );
    }
}

/// Relay one opaque ceremony payload into a channel mailbox. Authorized by possession of the
/// opaque channel handle; the server stores the payload verbatim and never inspects it.
#[endpoint(operation_id = "relay_enrollment_message", tags("auth"))]
pub async fn relay_send(
    req: &mut Request,
    depot: &mut Depot,
    body: JsonBody<RelaySendRequest>,
) -> RelaySendResponses {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");
    let channel_id = req.param::<String>("channel_id").unwrap_or_default();

    let Some(dir) = enrollment::Direction::parse(&body.to) else {
        return RelaySendResponses::Malformed(format!("unknown direction {:?}", body.to));
    };
    if body.payload.is_empty() {
        return RelaySendResponses::Malformed("empty payload".into());
    }
    if body.payload.len() > enrollment::MAX_RELAY_PAYLOAD_LEN {
        return RelaySendResponses::Malformed("payload too large".into());
    }

    match enrollment::relay_send(
        &state.session_manager,
        &channel_id,
        dir,
        body.payload.clone(),
    )
    .await
    {
        Ok(enrollment::RelaySend::Ok) => RelaySendResponses::NoContent,
        Ok(enrollment::RelaySend::NoChannel) => RelaySendResponses::NoChannel,
        Err(e) => {
            tracing::error!("enrollment relay send error: {e}");
            RelaySendResponses::Internal
        }
    }
}

/// Responses for draining a channel mailbox.
pub(super) enum RelayRecvResponses {
    /// The drained opaque payloads (possibly empty).
    Ok(RelayMessagesResponse),
    /// The channel is unknown or expired (`404 error.enrollment.channel_not_found`).
    NoChannel,
    /// Unknown direction (`400 error.enrollment.relay_malformed`).
    Malformed(String),
    /// Server fault.
    Internal,
}

#[async_trait]
impl Writer for RelayRecvResponses {
    async fn write(mut self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Ok(data) => {
                res.status_code(StatusCode::OK);
                Json(data).write(req, depot, res).await;
            }
            Self::NoChannel => {
                res.status_code(StatusCode::NOT_FOUND);
                res.render(Json(ApiError::with_code(
                    "Enrollment relay channel not found",
                    error_codes::ENROLLMENT_CHANNEL_NOT_FOUND,
                )));
            }
            Self::Malformed(detail) => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(ApiError::with_code(
                    format!("Malformed relay request: {detail}"),
                    error_codes::ENROLLMENT_RELAY_MALFORMED,
                )));
            }
            Self::Internal => {
                res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                res.render(Json(ApiError::new("Internal server error")));
            }
        }
    }
}

impl EndpointOutRegister for RelayRecvResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Drained opaque relay payloads").add_content(
                "application/json",
                salvo::oapi::Content::new(RelayMessagesResponse::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("400"),
            salvo::oapi::Response::new("Unknown direction"),
        );
        operation.responses.insert(
            String::from("404"),
            salvo::oapi::Response::new("Unknown or expired relay channel"),
        );
    }
}

/// Drain a channel mailbox for the requested direction (`?to=a|b`). Authorized by possession
/// of the opaque channel handle.
#[endpoint(operation_id = "poll_enrollment_messages", tags("auth"))]
pub async fn relay_recv(req: &mut Request, depot: &mut Depot) -> RelayRecvResponses {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");
    let channel_id = req.param::<String>("channel_id").unwrap_or_default();
    let to = req.query::<String>("to").unwrap_or_default();

    let Some(dir) = enrollment::Direction::parse(&to) else {
        return RelayRecvResponses::Malformed(format!("unknown direction {to:?}"));
    };

    match enrollment::relay_recv(&state.session_manager, &channel_id, dir).await {
        Ok(enrollment::RelayRecv::Messages(messages)) => {
            RelayRecvResponses::Ok(RelayMessagesResponse { messages })
        }
        Ok(enrollment::RelayRecv::NoChannel) => RelayRecvResponses::NoChannel,
        Err(e) => {
            tracing::error!("enrollment relay recv error: {e}");
            RelayRecvResponses::Internal
        }
    }
}
