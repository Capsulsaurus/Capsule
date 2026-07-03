//! Device enrollment — the cross-device-add server surface (contract skeleton; slice
//! `S-C7` in the repo-root `SLICES.md`; SSoT: <https://docs/design/device-enrollment/>).
//!
//! An existing signed-in device issues a single-use enrollment code (fresh local device
//! authorization required on top of the session — a stolen session token alone cannot
//! enroll a rogue device); the new device redeems it to establish the relay channel. The
//! code is single-use, expires in 10 minutes, is rate-limited at redemption, and is
//! deleted on redemption or expiry. Paths live under `devices/enroll` — plain
//! `GET /auth/devices` is the existing session listing, a different surface.

use salvo::oapi::extract::JsonBody;
use salvo::prelude::*;
use serde::{Deserialize, Serialize};

/// The issued enrollment code, displayed as a QR (full entropy) with a shorter,
/// rate-limited numeric text fallback.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct EnrollmentCodeResponse {
    /// The full-entropy code for the QR payload (base64; ≥64 bits).
    pub code: String,
    /// The shorter transcribable fallback (safe because redemption is single-use,
    /// expiring, and rate-limited; channel integrity rests on the safety code).
    pub text_fallback: String,
    /// RFC 3339 expiry (10 minutes from issue).
    pub expires_at: String,
}

/// A redemption request from the new device.
#[derive(Debug, Deserialize, ToSchema)]
#[allow(dead_code)]
pub(super) struct RedeemRequest {
    /// The scanned or typed enrollment code.
    pub code: String,
}

/// The established relay channel handle the ceremony continues over.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct ChannelResponse {
    /// Opaque relay-channel handle for the ephemeral-ECDH ceremony messages.
    pub channel_id: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

/// Responses for enrollment-code issuance.
#[allow(dead_code)]
pub(super) enum IssueResponses {
    /// Code issued.
    Ok(EnrollmentCodeResponse),
    /// The session lacks the fresh local device authorization the ceremony requires.
    LocalAuthRequired,
}

#[async_trait]
impl Writer for IssueResponses {
    async fn write(mut self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Ok(data) => {
                res.status_code(StatusCode::OK);
                Json(data).write(req, depot, res).await;
            }
            Self::LocalAuthRequired => {
                res.status_code(StatusCode::FORBIDDEN);
                res.render(Json(ErrorResponse {
                    error: "fresh local device authorization required".into(),
                }));
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
            String::from("403"),
            salvo::oapi::Response::new("Fresh local device authorization required"),
        );
    }
}

/// Responses for enrollment-code redemption.
#[allow(dead_code)]
pub(super) enum RedeemResponses {
    /// Channel established.
    Ok(ChannelResponse),
    /// The code is unknown, expired, already redeemed, or rate-limited —
    /// indistinguishable by design.
    Refused,
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

/// Issue a single-use enrollment code from an existing, locally-authorized device.
#[endpoint(operation_id = "issue_enrollment_code", tags("auth"), security(("bearer" = [])))]
pub async fn issue_enrollment_code(_req: &mut Request, _depot: &mut Depot) -> IssueResponses {
    todo!("S-C7: enrollment-code issuance — see SLICES.md")
}

/// Redeem an enrollment code from the new device (unauthenticated; the code is the
/// credential).
#[endpoint(operation_id = "redeem_enrollment_code", tags("auth"))]
pub async fn redeem_enrollment_code(
    _req: &mut Request,
    _depot: &mut Depot,
    _body: JsonBody<RedeemRequest>,
) -> RedeemResponses {
    todo!("S-C7: enrollment-code redemption — see SLICES.md")
}
