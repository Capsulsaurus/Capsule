//! Cross-device add over the wire (slice `S-C7`).
//!
//! Four operations: device A issues a code, device B redeems it for a channel, either device
//! relays opaque payloads through that channel, and A closes it. [`crate::enrollment`] owns the
//! freshness gate and the reason the relay is untrusted; this module is the wire shape.
//!
//! # `S-C28` audit
//!
//! | Status | Verdict |
//! | --- | --- |
//! | issue `200` | the code, its transcribable fallback, and its expiry |
//! | issue `403 error.enrollment.local_auth_required` | the session has not proved a credential inside the freshness window |
//! | redeem `200` | a channel handle and its expiry |
//! | redeem `404 error.enrollment.code_refused` | unknown, spent, or expired — **indistinguishable**, so redemption is not an oracle |
//! | relay `204` / drain `200` | delivered, and delivered once |
//! | relay/drain/close `404 error.enrollment.channel_not_found` | the handle names no live channel |
//! | relay `400 error.enrollment.relay_malformed` | an unknown direction or an implausible payload |
//! | `401` / `403` | the framework's, on the two operations that take a session |
//! | `500 error.auth.unavailable` | a store could not answer |
//!
//! **No `429`.** The contract rate-limits redemption and the catalog carries a code for it, but
//! the per-user counter has no port (`S-C32`), so declaring the status would promise something
//! nothing can produce. Single-use, a ten-minute TTL and ≥64 bits of entropy bound the window
//! meanwhile.

use capsule_i18n::error_codes;
use kynos::prelude::*;
use kynos::response::status::NoContent;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::AccessToken;
use crate::counter::{CounterContext, CounterKey, budgets};
use crate::enrollment::{EnrollmentContext, MAX_RELAY_BYTES};
use crate::store::{
    ChannelId, Direction, DrainOutcome, EnrollmentCode, PendingEnrollment, RelayChannel,
    RelayOutcome, RelayPayload, UserId,
};

/// The enrollment surface: getting a second device into the directory.
#[derive(Tag)]
#[tag(
    name = "enrollment",
    description = "Cross-device add: the one-time code, and the relay channel it opens."
)]
pub struct EnrollmentTag;

/// A freshly issued enrollment code.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentCodeResponse {
    /// The full-entropy code the QR payload carries.
    pub code: String,
    /// The shorter transcribable numeric fallback.
    ///
    /// Deliberately weaker than the QR payload and safe because it never stands alone:
    /// redemption is single-use and expires, and channel integrity rests on the safety-code
    /// check rather than on this value.
    pub text_fallback: String,
    /// When both spellings stop being redeemable, RFC 3339.
    pub expires_at: String,
}

/// The code a device presents.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct RedeemRequest {
    /// Either spelling of the issued code.
    pub code: String,
}

/// The channel a redeemed code opens.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ChannelResponse {
    /// The handle both devices relay through. Possession of it *is* the capability.
    pub channel_id: String,
    /// When the channel closes on its own, RFC 3339.
    pub expires_at: String,
}

/// One relayed payload.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct RelayRequest {
    /// Which mailbox to append to: `to_initiator` or `to_enrollee`.
    pub direction: String,
    /// The opaque payload. The server never inspects it.
    pub payload: String,
}

/// Which mailbox to drain.
#[derive(QueryParams, Schema)]
pub struct DrainQuery {
    /// `to_initiator` or `to_enrollee`.
    pub direction: String,
}

/// The channel handle.
#[derive(PathParams, Schema)]
pub struct ChannelPath {
    /// The handle a redeemed code returned.
    pub channel_id: String,
}

/// Everything pending in one mailbox.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DrainResponse {
    /// The payloads in arrival order, removed by this call. Possibly empty.
    pub payloads: Vec<String>,
}

/// Why a code was not issued.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum IssueRejection {
    /// The session has not proved a credential recently enough.
    #[error("starting a device add needs a fresh local authorization")]
    #[problem(status = 403, title = "Local authorization required")]
    LocalAuthRequired {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A store could not answer.
    #[error("the enrollment code could not be issued")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why a code was not redeemed.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum RedeemRejection {
    /// Unknown, already redeemed, or expired.
    ///
    /// One answer for all three, deliberately: this operation takes no credential, so an answer
    /// that distinguished "expired" from "never existed" would tell an unauthenticated caller
    /// that a guessed code was once real.
    #[error("that code cannot be redeemed")]
    #[problem(status = 404, title = "Code refused")]
    Refused {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Too many redemption attempts against this code.
    ///
    /// The limiter design/device-enrollment.md names as the reason the **shorter transcribable
    /// fallback** is safe to offer: it trades entropy for transcribability, and what keeps that
    /// trade honest is that the code cannot be ground through inside its ten-minute life.
    #[error("too many attempts against this code")]
    #[problem(status = 429, title = "Too many attempts")]
    RateLimited {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A store could not answer.
    #[error("the enrollment code could not be redeemed")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why a relay operation failed.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum RelayRejection {
    /// The direction is not one of the two, or the payload is not carryable.
    #[error("{detail}")]
    #[problem(status = 400, title = "Malformed relay request")]
    Malformed {
        /// What was wrong, in English.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The handle names no live channel.
    #[error("that device-add session has ended")]
    #[problem(status = 404, title = "Channel not found")]
    NoChannel {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A store could not answer.
    #[error("the relay could not be reached")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Issue a one-time enrollment code for the caller's account.
///
/// Gated on a recent credential presentation, not merely on a valid session — a stolen token
/// must not be able to enroll a rogue device. See [`crate::enrollment`] for exactly how much
/// that gate can mean.
#[kynos::post(
    "/v1/auth/devices/enroll",
    operation_id = "issue_enrollment_code",
    tag = EnrollmentTag
)]
pub async fn issue_enrollment_code(
    Inject(enrollment): Inject<EnrollmentContext>,
    Auth(credential): Auth<AccessToken>,
) -> Result<Json<EnrollmentCodeResponse>, IssueRejection> {
    let user = UserId::new(credential.user.as_str());
    let now = enrollment.clock().now();

    // Read from the credential, not from the store. `S-C48` put the ledger on the bearer
    // scheme's path, so by the time a handler runs the session has already been confirmed live
    // and its `authenticated_at` handed over. This used to be a second `read_session` with its
    // own `500`; that answer became unreachable the moment the scheme started refusing an
    // unreadable ledger itself, and a status nothing can produce is the `S-C28` defect.
    //
    // The "session the store no longer holds" case the old comment covered is not lost — it is
    // now a `401` from the scheme, which is a better answer than the `403` this gate gave it.
    if !enrollment.is_fresh(credential.authenticated_at, now) {
        tracing::info!(%user, "an enrollment was refused: no recent local authorization");
        return Err(IssueRejection::LocalAuthRequired {
            code: error_codes::ENROLLMENT_LOCAL_AUTH_REQUIRED,
        });
    }

    // Collision-checked at generation, per the contract. A UUIDv4 collision is not a real event;
    // asking is cheap and the alternative is a silently overwritten pending enrollment.
    let (code, text_fallback) = mint(&enrollment).await?;

    enrollment
        .enrollments()
        .issue(PendingEnrollment {
            user_id: user.clone(),
            code: code.clone(),
            text_fallback: text_fallback.clone(),
            issued_at: now,
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the enrollment store could not issue");
            IssueRejection::unavailable()
        })?;

    tracing::info!(%user, "issued an enrollment code");
    Ok(Json(EnrollmentCodeResponse {
        code: code.as_str().to_owned(),
        text_fallback: text_fallback.as_str().to_owned(),
        expires_at: crate::store::deadline(now, enrollment.enrollments().ttl()).to_string(),
    }))
}

/// Redeem a code for a relay channel.
///
/// **Unauthenticated, necessarily.** Device B has no account, no session and no key material —
/// it is a phone that has just scanned a QR code. The code is the only thing it holds, so the
/// code is the credential.
#[kynos::post(
    "/v1/auth/devices/enroll/redeem",
    operation_id = "redeem_enrollment_code",
    tag = EnrollmentTag
)]
pub async fn redeem_enrollment_code(
    Inject(enrollment): Inject<EnrollmentContext>,
    Inject(counters): Inject<CounterContext>,
    Json(request): Json<RedeemRequest>,
) -> Result<Json<ChannelResponse>, RedeemRejection> {
    let presented = EnrollmentCode::new(request.code.trim());

    // Charged **before** the redemption is attempted, and charged on every attempt whatever the
    // outcome (`S-C32`). A limiter that only counted failures would let a caller who guesses
    // right on the last try escape it, and one charged after the fact would let a burst through
    // while the first attempt was still resolving.
    //
    // Keyed on the presented code rather than on a source address, because the contract's
    // budget is per *pending enrollment* — the thing being guessed — and a caller behind many
    // addresses is exactly the caller a per-address key would miss.
    let key = CounterKey::EnrollmentRedemption(request.code.trim().to_owned());
    let verdict = counters
        .hit(&key, budgets::ENROLLMENT_REDEMPTION)
        .await
        .map_err(|error| {
            // Fail closed. A limiter an attacker turns off by loading the counter store is not
            // a limiter.
            tracing::error!(%error, "the redemption counter could not be reached");
            RedeemRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?;
    if !verdict.admits() {
        return Err(RedeemRejection::RateLimited {
            code: error_codes::ENROLLMENT_RATE_LIMITED,
        });
    }

    let redeemed = enrollment
        .enrollments()
        .redeem(&presented)
        .await
        .map_err(|error| {
            tracing::error!(%error, "the enrollment store could not redeem");
            RedeemRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?;

    let Some(pending) = redeemed else {
        tracing::info!("an enrollment code was refused");
        return Err(RedeemRejection::Refused {
            code: error_codes::ENROLLMENT_CODE_REFUSED,
        });
    };

    let now = enrollment.clock().now();
    let channel_id = ChannelId::new(Uuid::new_v4().to_string());
    enrollment
        .channels()
        .open(
            &channel_id,
            RelayChannel {
                initiator_user_id: pending.user_id.clone(),
                opened_at: now,
            },
        )
        .await
        .map_err(|error| {
            tracing::error!(%error, "the channel store could not open");
            RedeemRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?;

    tracing::info!(user = %pending.user_id, "an enrollment code opened a relay channel");
    Ok(Json(ChannelResponse {
        channel_id: channel_id.as_str().to_owned(),
        expires_at: crate::store::deadline(now, enrollment.channels().ttl()).to_string(),
    }))
}

/// Append a payload to one of a channel's two mailboxes.
///
/// Unauthenticated and gated by the handle alone. The relay is a dumb pipe by design — see
/// [`crate::enrollment`] — and the safety-code check is what defends the ceremony.
#[kynos::post(
    "/v1/auth/devices/enroll/channel/{channel_id}",
    operation_id = "relay_enrollment_payload",
    tag = EnrollmentTag
)]
pub async fn relay_enrollment_payload(
    Inject(enrollment): Inject<EnrollmentContext>,
    Path(path): Path<ChannelPath>,
    Json(request): Json<RelayRequest>,
) -> Result<NoContent, RelayRejection> {
    let direction = direction(&request.direction)?;
    if request.payload.is_empty() || request.payload.len() > MAX_RELAY_BYTES {
        return Err(RelayRejection::Malformed {
            detail: format!("a relay payload must be 1..={MAX_RELAY_BYTES} bytes"),
            code: error_codes::ENROLLMENT_RELAY_MALFORMED,
        });
    }

    let channel = ChannelId::new(&path.channel_id);
    let outcome = enrollment
        .channels()
        .enqueue(&channel, direction, RelayPayload::new(request.payload))
        .await
        .map_err(|error| {
            tracing::error!(%error, "the channel store could not enqueue");
            RelayRejection::unavailable()
        })?;

    match outcome {
        RelayOutcome::Enqueued { depth } => {
            tracing::debug!(depth, "relayed an enrollment payload");
            Ok(NoContent)
        }
        RelayOutcome::NoChannel => Err(RelayRejection::no_channel()),
    }
}

/// Take everything pending in one of a channel's mailboxes.
///
/// Destructive: a relayed payload is delivered once. Draining one direction leaves the other
/// untouched, so the two devices do not consume each other's mail.
#[kynos::get(
    "/v1/auth/devices/enroll/channel/{channel_id}",
    operation_id = "drain_enrollment_channel",
    tag = EnrollmentTag
)]
pub async fn drain_enrollment_channel(
    Inject(enrollment): Inject<EnrollmentContext>,
    Path(path): Path<ChannelPath>,
    Query(query): Query<DrainQuery>,
) -> Result<Json<DrainResponse>, RelayRejection> {
    let direction = direction(&query.direction)?;
    let channel = ChannelId::new(&path.channel_id);

    let outcome = enrollment
        .channels()
        .drain(&channel, direction)
        .await
        .map_err(|error| {
            tracing::error!(%error, "the channel store could not drain");
            RelayRejection::unavailable()
        })?;

    match outcome {
        DrainOutcome::Drained(payloads) => Ok(Json(DrainResponse {
            payloads: payloads
                .into_iter()
                .map(|payload| payload.as_str().to_owned())
                .collect(),
        })),
        DrainOutcome::NoChannel => Err(RelayRejection::no_channel()),
    }
}

/// Close a channel and drop both mailboxes with it.
///
/// **The initiator's, and authenticated.** A close is the one relay operation that is not
/// idempotent from the other device's point of view — it ends the ceremony — so leaving it on
/// the handle alone would make an abandoned QR code a denial of service. The account is checked
/// against the channel's recorded initiator, and a channel belonging to another account answers
/// exactly as an unknown one does.
#[kynos::delete(
    "/v1/auth/devices/enroll/channel/{channel_id}",
    operation_id = "close_enrollment_channel",
    tag = EnrollmentTag
)]
pub async fn close_enrollment_channel(
    Inject(enrollment): Inject<EnrollmentContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<ChannelPath>,
) -> Result<NoContent, RelayRejection> {
    let user = UserId::new(credential.user.as_str());
    let channel = ChannelId::new(&path.channel_id);

    let held = enrollment
        .channels()
        .lookup(&channel)
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the channel store could not be read");
            RelayRejection::unavailable()
        })?;

    // Read before closing, so another account's channel is refused without being ended.
    let Some(_) = held.filter(|record| record.initiator_user_id == user) else {
        tracing::info!(%user, "a channel close found nothing of this account's");
        return Err(RelayRejection::no_channel());
    };

    enrollment
        .channels()
        .close(&channel)
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the channel store could not close");
            RelayRejection::unavailable()
        })?;

    tracing::info!(%user, "an enrollment channel was closed by its initiator");
    Ok(NoContent)
}

/// Mint a code pair, refusing one that is already taken.
async fn mint(
    enrollment: &EnrollmentContext,
) -> Result<(EnrollmentCode, EnrollmentCode), IssueRejection> {
    // UUIDv4 rather than v7: an enrollment code's creation time must not leak, and a v7 code
    // read off a screen would carry a timestamp. That is the Identifiers rule's exact carve-out.
    let code = EnrollmentCode::new(Uuid::new_v4().to_string());
    let text_fallback =
        EnrollmentCode::new(format!("{:08}", Uuid::new_v4().as_u128() % 100_000_000));

    for candidate in [&code, &text_fallback] {
        let taken = enrollment
            .enrollments()
            .is_taken(candidate)
            .await
            .map_err(|error| {
                tracing::error!(%error, "the enrollment store could not be checked");
                IssueRejection::unavailable()
            })?;
        if taken {
            tracing::warn!("a minted enrollment code was already taken");
            return Err(IssueRejection::unavailable());
        }
    }

    Ok((code, text_fallback))
}

/// Read a direction, refusing anything that is not one of the two.
fn direction(raw: &str) -> Result<Direction, RelayRejection> {
    match raw {
        "to_initiator" => Ok(Direction::ToInitiator),
        "to_enrollee" => Ok(Direction::ToEnrollee),
        other => {
            tracing::info!(direction = %other, "a relay named an unknown direction");
            Err(RelayRejection::Malformed {
                detail: "direction must be `to_initiator` or `to_enrollee`".to_owned(),
                code: error_codes::ENROLLMENT_RELAY_MALFORMED,
            })
        }
    }
}

impl IssueRejection {
    /// A store could not answer.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::AUTH_UNAVAILABLE,
        }
    }
}

impl RelayRejection {
    /// The handle names no live channel.
    fn no_channel() -> Self {
        Self::NoChannel {
            code: error_codes::ENROLLMENT_CHANNEL_NOT_FOUND,
        }
    }

    /// A store could not answer.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::AUTH_UNAVAILABLE,
        }
    }
}
