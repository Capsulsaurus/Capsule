//! Proposing an **album upgrade** — the client half of the ceremony's one server-side step
//! (over the server's album-upgrade wire).
//!
//! [Versioning — Album Upgrade Ceremony] is a client ceremony carried on MLS application
//! messages the server cannot read. Four of its steps are the server's, and the first is the
//! one this module drives: `POST /v1/albums/{album_id}/upgrade` hands the server a **signed
//! `UpgradeIntent`**, which quiesces the album (a v_old client that never saw the proposal is
//! precisely the party that will not stop writing on its own) and starts the deadline on the
//! server's own clock (so a skewed member clock can neither extend nor shorten the window).
//!
//! Two rules shape this module, and both are the directory client's rules for the same reason:
//!
//! - **The signed bytes travel verbatim.** `intent_cbor` is the canonical CBOR
//!   `capsule_core::crypto::upgrade::SignedUpgradeIntent` produced, and it is written to the
//!   body unchanged. Re-encoding it here would detach it from the signature the server checks
//!   against the proposing device's DSK in the account's published directory, and the failure
//!   would look like a forged proposal.
//! - **Nothing cryptographic happens here.** The intent is built and signed in `capsule-core`;
//!   this module is the wire and its refusals.
//!
//! # What is hand-written, and what would retire it
//!
//! The request body is `application/cbor`, and `spargen` 0.5's `classify_media` does not know
//! that media type, so `capsule-sdk/build.rs` narrows the operation out of the generated client
//! — the *surface* is narrowed, the document is never mutilated. Only the **request**
//! is hand-written: the path, the media type, and the verbatim CBOR body. Every JSON document
//! the operation answers with decodes into the generated types — the phase into
//! [`crate::rest::types::UpgradePhaseResponse`], every refusal into
//! [`crate::rest::types::CodedProblem`] — so this module carries no second parser for a shape
//! the contract already declares. ADR 0002 records the exception; teaching spargen the media
//! type (issue #440) retires it, and nothing in this repository can.
//!
//! The **other two** operations on this path — `GET` (read the phase) and `DELETE` (end the
//! ceremony) — are plain JSON and *are* generated. Call them through
//! [`crate::client::AuthenticatedClient`]; this module deliberately does not duplicate them.
//!
//! [Versioning — Album Upgrade Ceremony]: https://docs/design/versioning/#album-upgrade-ceremony

use jiff::Timestamp;
use tracing::instrument;
use uuid::Uuid;

use crate::auth::{AuthError, Session};
use crate::rest::types::{CodedProblem, UpgradePhaseResponse};

/// The media type the intent is *signed* in, and therefore the only one it may be sent as.
const CBOR: &str = "application/cbor";

/// The phase response's media type — the answer is a plain JSON document.
const JSON: &str = "application/json";

// ─── Errors ───────────────────────────────────────────────────────────────────

/// Everything a proposal can fail with. Callers switch on the typed variant, or on its stable
/// `error.*` code, and never on a bare status.
///
/// Every refusal carries the code the **server** stamped rather than one this module inferred
/// from the status, because the ceremony's refusals are the ones a user actually reads: "the
/// album is already upgrading" and "your device is not an admin" are different sentences.
#[derive(Debug, thiserror::Error)]
pub enum UpgradeError {
    /// The authenticated request itself failed (transport, session expiry, refresh).
    #[error(transparent)]
    Auth(#[from] AuthError),
    /// The server refused the body as one it cannot read as a signed intent (`400`), refused
    /// the media type (`415`), or refused its size (`413`). Retrying the same bytes changes
    /// nothing — rebuild and re-sign the intent.
    #[error("the server rejected the upgrade intent: {detail}")]
    Malformed {
        /// The stable `error.*` catalog code the refusal carried.
        code: Option<String>,
        /// English detail from the problem body.
        detail: String,
    },
    /// The credential was refused (`401`).
    #[error("the upgrade surface refused the credential: {detail}")]
    Unauthorized {
        /// The stable `error.*` catalog code the refusal carried.
        code: Option<String>,
        /// English detail from the problem body.
        detail: String,
    },
    /// The intent is not signed by a device in the caller's **published** directory (`403`),
    /// so the server cannot tell that an admin device really asked for this. Publish the
    /// directory holding the proposing device first ([`crate::directory`]).
    #[error("the upgrade intent's proposer could not be verified: {detail}")]
    NotProposer {
        /// The stable `error.*` catalog code the refusal carried.
        code: Option<String>,
        /// English detail from the problem body.
        detail: String,
    },
    /// No such album, or not this caller's (`404`) — one answer for both, deliberately, so the
    /// surface discloses nothing about albums the caller does not own.
    #[error("no such album: {detail}")]
    NotFound {
        /// The stable `error.*` catalog code the refusal carried.
        code: Option<String>,
        /// English detail from the problem body.
        detail: String,
    },
    /// A different ceremony already holds this album (`409`), and only one may. Read the phase
    /// (the generated `GET` on the same path) and either join that ceremony or wait for its
    /// deadline; a fresh proposal *replaces* an expired one rather than conflicting with it.
    #[error("album is already upgrading under {intent_id:?}: {detail}")]
    InFlight {
        /// The ceremony that holds the album, as the server reported it.
        intent_id: Option<String>,
        /// The stable `error.*` catalog code the refusal carried.
        code: Option<String>,
        /// English detail from the problem body.
        detail: String,
    },
    /// The server's protocol gate refused the request (`426`): this client speaks a protocol
    /// version outside the server's window. Nothing about the intent is wrong — update the
    /// client. The code is the server's own (`error.protocol.version_unsupported`).
    #[error("the server does not accept this client's protocol version: {detail}")]
    ProtocolUnsupported {
        /// The stable `error.*` catalog code the refusal carried.
        code: Option<String>,
        /// English detail from the problem body.
        detail: String,
    },
    /// A collaborator could not answer (`500`). Transient.
    #[error("the upgrade could not be recorded: {detail}")]
    Unavailable {
        /// The stable `error.*` catalog code the refusal carried.
        code: Option<String>,
        /// English detail from the problem body.
        detail: String,
    },
    /// The response body was not the phase document the contract declares.
    #[error("malformed upgrade phase response: {0}")]
    MalformedResponse(String),
    /// The server returned an unmodeled status.
    #[error("unexpected {status} response from the album-upgrade endpoint")]
    Unexpected {
        /// The HTTP status code the server returned.
        status: u16,
    },
}

impl UpgradeError {
    /// The stable `error.*` catalog code a client localizes, when one applies. The English
    /// [`Display`](std::fmt::Display) form stays the developer/log detail.
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::Auth(auth) => auth.error_code(),
            Self::Malformed { code, .. }
            | Self::Unauthorized { code, .. }
            | Self::NotProposer { code, .. }
            | Self::NotFound { code, .. }
            | Self::InFlight { code, .. }
            | Self::ProtocolUnsupported { code, .. }
            | Self::Unavailable { code, .. } => code.as_deref(),
            _ => None,
        }
    }
}

// ─── Refusal bodies ───────────────────────────────────────────────────────────

/// A refusal's body, read as the contract declares it: a generated [`CodedProblem`], plus the
/// `409`'s `intent_id` extension member. The contract types that member only through
/// `CodedProblem`'s `additionalProperties`, so the generated struct does not name it and it is
/// read off the same document rather than through a second hand-written body type.
///
/// `problem` is `None` when there is no coded problem to read — the `413` backstop sends no body
/// at all, and an unmodeled status may send anything.
#[derive(Debug, Default)]
struct Refusal {
    problem: Option<CodedProblem>,
    intent_id: Option<String>,
}

impl Refusal {
    /// Read a refusal body. Never fails: a body that is not a coded problem is a refusal
    /// without a code, and the status alone then picks the variant.
    fn parse(body: &[u8]) -> Self {
        let Ok(document) = serde_json::from_slice::<serde_json::Value>(body) else {
            return Self::default();
        };
        let intent_id = document
            .get("intent_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let problem = serde_json::from_value::<CodedProblem>(document).ok();
        Self { problem, intent_id }
    }
}

/// The ceremony an album is in, as the proposal answered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradePhase {
    /// The album, echoed by the server.
    pub album_id: Uuid,
    /// The ceremony now in flight, or `None` when the album is in normal operation.
    pub intent_id: Option<Uuid>,
    /// The protocol version the fork will be pinned to, when a ceremony is in flight.
    pub to_protocol_version: Option<String>,
    /// When the window closes, on the **server's** clock — never the client's.
    pub expires_at: Option<Timestamp>,
    /// How many upload sessions are still in flight against this album.
    ///
    /// The drain signal of the ceremony's step 3: the proposer waits for zero. A count and not
    /// a listing, because the proposer needs to know *whether* to wait and has no business
    /// seeing other members' upload identifiers to find out.
    pub in_flight: u64,
}

// ─── Client ───────────────────────────────────────────────────────────────────

/// The album-upgrade proposal client. Borrows an authenticated [`Session`], so every call
/// rides the SDK's bearer/refresh machinery and no token is handled here.
#[derive(Clone)]
pub struct UpgradeClient {
    session: Session,
    base_url: String,
}

impl UpgradeClient {
    /// Build a client against the **API root** — the origin the operation paths hang off (e.g.
    /// `https://api.example.com`), the same base [`crate::client::AuthenticatedClient`],
    /// [`crate::sync::SyncConsumer`] and [`crate::recovery::RecoveryClient`] take.
    ///
    /// Note that [`crate::directory`] and [`crate::verify`] take a *deeper* base instead. That
    /// divergence is real and is noticed in `capsule-server/tests/sdk_client.rs`; it is not
    /// this module's to close, and a new module choosing the root is how it narrows.
    #[must_use]
    pub fn new(session: Session, api_base_url: &str) -> Self {
        Self {
            session,
            base_url: api_base_url.trim_end_matches('/').to_owned(),
        }
    }

    /// Propose the upgrade `intent_cbor` describes for `album_id`, returning the ceremony the
    /// server now holds.
    ///
    /// `intent_cbor` is the canonical CBOR of a signed `UpgradeIntent` and is sent **verbatim**
    /// — this method never re-encodes it, because the signature is over exactly those bytes.
    ///
    /// # Errors
    ///
    /// [`UpgradeError::InFlight`] when another ceremony already holds the album (only one may),
    /// [`UpgradeError::NotProposer`] when the signing device is not in the published directory,
    /// and the rest of [`UpgradeError`] for the remaining refusals.
    #[instrument(skip(self, intent_cbor), fields(album_id = %album_id, bytes = intent_cbor.len()))]
    pub async fn begin(
        &self,
        album_id: Uuid,
        intent_cbor: &[u8],
    ) -> Result<UpgradePhase, UpgradeError> {
        let url = format!(
            "{}/v1/albums/{}/upgrade",
            self.base_url,
            album_id.hyphenated()
        );
        let body = intent_cbor.to_vec();
        let response = self
            .session
            .execute(|http| {
                http.post(&url)
                    .header(reqwest::header::CONTENT_TYPE, CBOR)
                    .header(reqwest::header::ACCEPT, JSON)
                    .body(body.clone())
            })
            .await
            .map_err(|error| match error {
                // The session screens every send through the protocol window (issue #674), so
                // the gate's `426` — or a write the advertised window already refuses — arrives
                // here typed rather than as a status; it keeps this module's own variant.
                AuthError::UpgradeRequired(refusal) => UpgradeError::ProtocolUnsupported {
                    code: Some(refusal.error_code().to_owned()),
                    detail: refusal.message,
                },
                other => UpgradeError::Auth(other),
            })?;

        let status = response.status();
        if !status.is_success() {
            // A body that cannot even be read is a refusal without a code, exactly like one
            // that is not a coded problem: the status still picks the variant.
            let body = response.bytes().await.unwrap_or_default();
            let error = refusal(status.as_u16(), Refusal::parse(&body));
            tracing::warn!(
                status = status.as_u16(),
                code = ?error.error_code(),
                "album-upgrade proposal refused"
            );
            return Err(error);
        }

        let wire: UpgradePhaseResponse = response
            .json()
            .await
            .map_err(|e| UpgradeError::MalformedResponse(e.to_string()))?;
        let phase = decode_phase(wire)?;
        tracing::info!(
            intent_id = ?phase.intent_id,
            in_flight = phase.in_flight,
            expires_at = ?phase.expires_at,
            "album upgrade proposed; the album is quiesced"
        );
        Ok(phase)
    }
}

/// Map a refusal onto its typed variant, keeping the code the server stamped.
///
/// One readable status table rather than a match buried in the request path.
///
/// `413` is the transport's body backstop and carries no problem body at all, so it has no
/// code — and this client does not mint one. Every code here is the code the *server* stamped;
/// a code invented on this side would assert that the server said something it did not, and a
/// client localizing it would read the SDK's guess as the server's judgement. The variant
/// already carries the actionable half ("these bytes will not do"), and the English detail
/// carries the reason.
fn refusal(status: u16, refusal: Refusal) -> UpgradeError {
    let Refusal { problem, intent_id } = refusal;
    let (code, detail) = problem.map_or((None, String::new()), |problem| {
        (Some(problem.code), problem.detail.unwrap_or_default())
    });
    match status {
        400 | 415 => UpgradeError::Malformed { code, detail },
        401 => UpgradeError::Unauthorized { code, detail },
        403 => UpgradeError::NotProposer { code, detail },
        404 => UpgradeError::NotFound { code, detail },
        409 => UpgradeError::InFlight {
            intent_id,
            code,
            detail,
        },
        413 => UpgradeError::Malformed {
            code: None,
            detail: "the signed upgrade intent exceeds the server's body limit".to_owned(),
        },
        426 => UpgradeError::ProtocolUnsupported { code, detail },
        500 => UpgradeError::Unavailable { code, detail },
        other => UpgradeError::Unexpected { status: other },
    }
}

/// Parse the phase document into its typed shape.
///
/// The ids become [`Uuid`]s and the deadline a [`jiff::Timestamp`], so a caller compares
/// instants rather than strings — the deadline is the one field in this ceremony where a
/// string comparison would be a correctness bug rather than an inconvenience.
fn decode_phase(wire: UpgradePhaseResponse) -> Result<UpgradePhase, UpgradeError> {
    let album_id = Uuid::parse_str(&wire.album_id)
        .map_err(|e| UpgradeError::MalformedResponse(format!("response album_id: {e}")))?;
    let intent_id = wire
        .intent_id
        .as_deref()
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|e| UpgradeError::MalformedResponse(format!("response intent_id: {e}")))?;
    let expires_at = wire
        .expires_at
        .as_deref()
        .map(str::parse::<Timestamp>)
        .transpose()
        .map_err(|e| UpgradeError::MalformedResponse(format!("response expires_at: {e}")))?;
    // The contract declares the count `uint64`; the generated type carries it signed, so a
    // negative count is a malformed answer rather than a wrapped one.
    let in_flight = u64::try_from(wire.in_flight).map_err(|_| {
        UpgradeError::MalformedResponse(format!(
            "response in_flight: {} is negative",
            wire.in_flight
        ))
    })?;
    Ok(UpgradePhase {
        album_id,
        intent_id,
        to_protocol_version: wire.to_protocol_version,
        expires_at,
        in_flight,
    })
}

#[cfg(test)]
mod tests;
