//! Pull-hint intake: the low-trust channel a home server tells this one that an album has news.
//!
//! ```text
//! POST   /v1/federation/hints                      (the hint's own signature is the credential)
//!        { home_server, recipient_server, nonce, album_id, sent_at, signature }
//! 202 { received_at }
//! 400 error.federation.hint_malformed
//! 401 error.federation.hint_unsigned
//! 403 error.federation.peer_unknown | error.moderation.server_blocked
//! 404 error.federation.not_configured
//! 429 error.federation.hint_rate_limited | error.federation.hint_at_capacity
//! 500 error.federation.unavailable
//! ```
//!
//! The contract is [`crate::federation::hints`]; what acts on a verified hint is
//! [`PullWorker::hint`](crate::federation::pull::PullWorker::hint). This route is the
//! verification [report intake](crate::routes::federation::submit_federated_report) does, check
//! for check, and then one call.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_i18n::error_codes;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::counter::{CounterContext, CounterKey, Verdict, budgets};
use crate::federation::hints::{self, HintClaim, HintError};
use crate::federation::pull::{HintOutcome, JobKey};
use crate::federation::{FederationContext, PeerId};
use crate::routes::federation::FederationTag;
use crate::store::AlbumId;

/// The most bytes any one field of a hint may carry, checked before anything is stored, logged
/// or keyed on. The same caps report intake holds the same kinds of value to.
mod bounds {
    /// A peer's origin: RFC 1035's ceiling on a domain name.
    pub(super) const ORIGIN: usize = 253;
    /// An album identifier: a UUID with room to spare.
    pub(super) const IDENTIFIER: usize = 64;
    /// An RFC 3339 instant, with room for any offset spelling.
    pub(super) const INSTANT: usize = 64;
    /// A base64 Ed25519 signature is 88 bytes; this leaves room for padding variants.
    pub(super) const SIGNATURE: usize = 128;
    /// A nonce: at least 16 bytes so it is not a counter.
    pub(super) const NONCE_MIN: usize = 16;
    /// See [`NONCE_MIN`].
    pub(super) const NONCE_MAX: usize = 64;
}

/// An advisory hint from an album's home server: "album A has news".
///
/// Every field except `signature` is covered by the signature, in canonical CBOR with a fixed
/// `context` — see [`HintClaim`].
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
pub struct PullHintRequest {
    /// The album's home server, which sends and signs the hint, as its own `server-info` names
    /// it.
    pub home_server: String,
    /// The server the hint is addressed to — this one, as its own `server-info` names it.
    pub recipient_server: String,
    /// A value the peer never signs twice: 16 to 64 bytes, a UUID being the obvious choice.
    pub nonce: String,
    /// The album that has news, as its home server names it.
    pub album_id: String,
    /// When the peer signed the hint, RFC 3339. Refused when more than five minutes from this
    /// server's clock either way.
    pub sent_at: String,
    /// The peer's Ed25519 signature over the canonical CBOR of the fields above and the
    /// `context` `capsule-federation-hint-v1`, base64.
    pub signature: String,
}

/// An accepted hint.
///
/// Says when it was received and nothing else: in particular not whether this server pulls the
/// album, which a hint must never be a way to learn.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
pub struct PullHintResponse {
    /// When this server accepted the hint, RFC 3339.
    pub received_at: String,
}

/// Why a hint was not accepted.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum HintRejection {
    /// A field of the body is not what it must be.
    #[error("the hint is malformed")]
    #[problem(status = 400, title = "Malformed hint")]
    Malformed {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The signature does not verify under the peer's pinned key — or it does, over a hint
    /// addressed to another server, signed outside the freshness window, or already received.
    #[error("the hint's signature could not be verified")]
    #[problem(status = 401, title = "Hint unsigned")]
    Unsigned {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// No operator has pinned a key for the home server.
    #[error("this server is not one we know")]
    #[problem(status = 403, title = "Peer unknown")]
    PeerUnknown {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The home server is on this server's blocklist.
    #[error("this server is blocked")]
    #[problem(status = 403, title = "Server blocked")]
    PeerBlocked {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// This peer has spent its hint budget, or a hint counter is holding as many keys as it
    /// will hold. The code says which.
    #[error("no hint can be received right now; try again later")]
    #[problem(status = 429, title = "Hint rate limited")]
    RateLimited {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer, so nothing was scheduled.
    #[error("the hint could not be received")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl HintRejection {
    fn malformed() -> Self {
        Self::Malformed {
            code: error_codes::FEDERATION_HINT_MALFORMED,
        }
    }

    fn unsigned() -> Self {
        Self::Unsigned {
            code: error_codes::FEDERATION_HINT_UNSIGNED,
        }
    }

    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::FEDERATION_UNAVAILABLE,
        }
    }

    fn peer_unknown() -> Self {
        Self::PeerUnknown {
            code: error_codes::FEDERATION_PEER_UNKNOWN,
        }
    }
}

/// Receive a signed pull hint from an album's home server.
///
/// Answers `404 error.federation.not_configured` when this deployment does not federate —
/// `FEDERATION_ENABLED` is off, which is the default — before the body is read, a budget is
/// charged or a store is touched.
///
/// # A hint is advisory, and that is all it is
///
/// A verified hint brings the job pulling `album_id` from `home_server` forward: no sooner than
/// one minute after that job's last pull, never for a peer on probation, and never past the
/// peer's back-off or open circuit, which the pull worker applies to the job exactly as it would
/// have. A hint adds no job, changes nothing about what is pulled or where from, and carries
/// nothing that reaches validation. The album is pulled on its own interval whether or not a
/// hint ever arrives, so a refused hint costs at most that interval.
///
/// # Only a pinned peer can hint, and only about its own albums
///
/// The hint carries its own Ed25519 signature, verified against the key an operator pinned for
/// `home_server` with `capsule-server peer pin`; a peer nobody pinned is
/// `403 error.federation.peer_unknown`, whatever it signed. The job a hint touches is keyed on the
/// signer, so a peer can only hint about albums this server pulls from that peer.
///
/// # The order the checks run in
///
/// The order report intake runs them in: bounds; whether it is addressed here and signed just
/// now; who is speaking; whether they are welcome; whether they really said it; whether they said
/// it before; how much they may ask for. **No budget is charged before the signature
/// verifies**, so a stranger cannot spend a real peer's hint budget by sending garbage under its
/// name. A hint addressed to another server, signed more than five minutes from this server's
/// clock, or reusing a nonce this peer already used is `401 error.federation.hint_unsigned`; a
/// peer that retries signs again with a fresh nonce.
///
/// # `202` whether or not this server pulls the album
///
/// A verified, admitted hint is answered `202` with the instant it was received, whatever became
/// of it: it moved a job, the job was already due, this server pulls no such album, or the
/// peer is on probation. Telling them apart would let a pinned peer walk album ids and read off
/// which ones this server's members subscribe to. Each of those does the same work — one peer
/// read and one schedule operation — so the answer's timing does not tell them apart either.
/// Only a store that could not answer is different: `500 error.federation.unavailable`, which
/// says nothing about any album.
#[kynos::post(
    "/v1/federation/hints",
    operation_id = "submit_pull_hint",
    tag = FederationTag
)]
pub async fn submit_pull_hint(
    Inject(federation): Inject<FederationContext>,
    Inject(counters): Inject<CounterContext>,
    Json(request): Json<PullHintRequest>,
) -> Result<HintReply, HintRejection> {
    for (name, value, cap) in [
        ("home_server", request.home_server.trim(), bounds::ORIGIN),
        (
            "recipient_server",
            request.recipient_server.trim(),
            bounds::ORIGIN,
        ),
        ("nonce", request.nonce.trim(), bounds::NONCE_MAX),
        ("album_id", request.album_id.trim(), bounds::IDENTIFIER),
        ("sent_at", request.sent_at.trim(), bounds::INSTANT),
        ("signature", request.signature.trim(), bounds::SIGNATURE),
    ] {
        if value.is_empty() || value.len() > cap {
            tracing::info!(
                field = name,
                length = value.len(),
                "a pull hint's field is out of bounds"
            );
            return Err(HintRejection::malformed());
        }
    }
    if request.nonce.trim().len() < bounds::NONCE_MIN {
        tracing::info!(
            length = request.nonce.trim().len(),
            "a pull hint's nonce is too short"
        );
        return Err(HintRejection::malformed());
    }
    let Ok(peer) = PeerId::parse(&request.home_server) else {
        tracing::info!("a pull hint named a home_server that is not a host name");
        return Err(HintRejection::malformed());
    };
    let Ok(sent_at) = request.sent_at.trim().parse::<jiff::Timestamp>() else {
        tracing::info!(%peer, "a pull hint carried an unreadable sent_at");
        return Err(HintRejection::malformed());
    };
    let Ok(signature) = BASE64.decode(request.signature.trim().as_bytes()) else {
        tracing::info!(%peer, "a pull hint's signature is not base64");
        return Err(HintRejection::malformed());
    };

    // Addressed here, and signed just now: pure checks on the body, made binding by the
    // signature below.
    if PeerId::new(request.recipient_server.trim()).as_str()
        != PeerId::new(federation.codec().server_id()).as_str()
    {
        tracing::info!(%peer, "a pull hint was addressed to another server");
        return Err(HintRejection::unsigned());
    }
    if !hints::is_fresh(sent_at, federation.clock().now()) {
        tracing::info!(%peer, %sent_at, "a pull hint is outside the freshness window");
        return Err(HintRejection::unsigned());
    }

    // Who is speaking, and whether they are welcome. A peer nobody pinned and a peer pinned
    // without a key are one answer: there is nothing to verify against.
    let record = federation.peers().read(&peer).await.map_err(|error| {
        tracing::error!(%error, %peer, "the peer store could not answer a pull hint");
        HintRejection::unavailable()
    })?;
    let Some(record) = record else {
        tracing::info!(%peer, "a pull hint was refused: the peer is unknown");
        return Err(HintRejection::peer_unknown());
    };
    if record.is_blocked() {
        tracing::info!(%peer, "a pull hint was refused: the peer is blocked");
        return Err(HintRejection::PeerBlocked {
            code: error_codes::MODERATION_SERVER_BLOCKED,
        });
    }
    let Some(key) = record.signing_key else {
        tracing::info!(%peer, "a pull hint was refused: the peer has no pinned key");
        return Err(HintRejection::peer_unknown());
    };

    // Whether they really said it: the body's fields, whitespace-trimmed and otherwise as sent,
    // under the fixed context.
    let claim = HintClaim {
        home_server: request.home_server.trim().to_owned(),
        recipient_server: request.recipient_server.trim().to_owned(),
        nonce: request.nonce.trim().to_owned(),
        album_id: request.album_id.trim().to_owned(),
        sent_at: request.sent_at.trim().to_owned(),
    };
    claim
        .verify(&signature, &key)
        .map_err(|error| match error {
            HintError::NotAuthentic => HintRejection::unsigned(),
            HintError::Unencodable => HintRejection::unavailable(),
        })?;

    // From here the peer is who it says it is. A replay is refused before the budget is charged,
    // so a captured hint cannot spend its peer's allowance; the nonce is spent only after, so a
    // pinned peer cannot grow the nonce partition past its budget. Two copies racing past the
    // peek still meet a one-hit budget at the hit.
    let nonce = CounterKey::FederatedHintNonce(format!("{peer}:{}", claim.nonce));
    if !counters
        .peek(&nonce, budgets::FEDERATED_HINT_NONCE)
        .await
        .map_err(|error| {
            tracing::error!(%error, %peer, "the hint nonce counter could not be reached");
            HintRejection::unavailable()
        })?
        .admits()
    {
        tracing::warn!(%peer, "a pull hint replayed a nonce");
        return Err(HintRejection::unsigned());
    }
    let budget = CounterKey::FederatedHints(peer.as_str().to_owned());
    match counters
        .hit(&budget, budgets::FEDERATED_HINTS)
        .await
        .map_err(|error| refused_by_counter(&counters, &error, &budget, budgets::FEDERATED_HINTS))?
    {
        Verdict::Admitted { .. } => {}
        Verdict::Limited { retry_after } => {
            tracing::info!(%peer, %retry_after, "a peer's pull-hint budget is spent");
            return Err(HintRejection::RateLimited {
                code: error_codes::FEDERATION_HINT_RATE_LIMITED,
            });
        }
    }
    match counters
        .hit(&nonce, budgets::FEDERATED_HINT_NONCE)
        .await
        .map_err(|error| {
            refused_by_counter(&counters, &error, &nonce, budgets::FEDERATED_HINT_NONCE)
        })? {
        Verdict::Admitted { .. } => {}
        Verdict::Limited { .. } => {
            tracing::warn!(%peer, "a pull hint replayed a nonce");
            return Err(HintRejection::unsigned());
        }
    }

    // Taken before the hint is acted on, so the answer's shape cannot depend on what it did.
    let received_at = crate::postgres::time::stored(federation.clock().now());
    let job = JobKey::new(peer.clone(), AlbumId::new(&claim.album_id));
    let outcome = if let Some(receiver) = federation.hints() {
        receiver.hint(&job).await
    } else {
        tracing::debug!(%peer, "no pull worker is assembled; a verified hint acts on nothing");
        HintOutcome::Unchanged
    };
    match outcome {
        HintOutcome::Moved(_) | HintOutcome::Unchanged | HintOutcome::Ignored => {
            tracing::debug!(%peer, ?outcome, "a verified pull hint was received");
            Ok(HintReply::Accepted(PullHintResponse {
                received_at: received_at.to_string(),
            }))
        }
        HintOutcome::Unavailable => Err(HintRejection::unavailable()),
    }
}

/// What a hint counter that refused to answer tells the peer.
///
/// Fail closed either way: a full partition is the limiter at capacity, a `429` under the
/// at-capacity code because no budget of the peer's was spent; anything else is a store that
/// could not answer, the `500`.
fn refused_by_counter(
    counters: &CounterContext,
    error: &crate::store::StoreError,
    key: &CounterKey,
    budget: crate::counter::Budget,
) -> HintRejection {
    if let Some(retry_after) = counters.capacity_refusal(error, budget) {
        tracing::warn!(%error, kind = key.as_str(), %retry_after, "a hint counter is at capacity");
        HintRejection::RateLimited {
            code: error_codes::FEDERATION_HINT_AT_CAPACITY,
        }
    } else {
        tracing::error!(%error, kind = key.as_str(), "a hint counter could not be reached");
        HintRejection::unavailable()
    }
}

/// The one way intake succeeds.
///
/// `202`: the hint was accepted as advice, and this server will pull on its own terms.
#[derive(Reply)]
pub enum HintReply {
    /// The hint was received.
    #[reply(status = 202, description = "The hint was received")]
    Accepted(PullHintResponse),
}
