//! Proposing an **album upgrade** — the client half of the ceremony's one server-side step
//! (slice `S-C24`, over the `S-D28` wire).
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
//! # Why hand-written, and what would retire it
//!
//! The request body is `application/cbor`, and `spargen` 0.4's `classify_media` does not know
//! that media type, so `capsule-sdk/build.rs` narrows the operation out of the generated client
//! (`S-D28`) — the *surface* is narrowed, the document is never mutilated. This module is
//! therefore the orchestration `AGENTS.md` permits over a wire it cannot generate, and it is
//! the fourth and last such client: `capsule_sdk::directory` hand-writes two and
//! [`crate::verify::StorageVerifyClient::fetch_receipt`] the third. Teaching spargen the media
//! type retires all four; nothing in this repository can.
//!
//! The **other two** operations on this path — `GET` (read the phase) and `DELETE` (end the
//! ceremony) — are plain JSON and *are* generated. Call them through
//! [`crate::client::AuthenticatedClient`]; this module deliberately does not duplicate them.
//!
//! [Versioning — Album Upgrade Ceremony]: https://docs/design/versioning/#album-upgrade-ceremony

use jiff::Timestamp;
use serde::Deserialize;
use tracing::instrument;
use uuid::Uuid;

use crate::auth::{AuthError, Session};

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
            | Self::Unavailable { code, .. } => code.as_deref(),
            _ => None,
        }
    }
}

// ─── Wire DTOs (mirror the server's transport JSON) ───────────────────────────

/// `UpgradePhaseResponse`, as the server serializes it. The three optional members are absent
/// when no ceremony is in flight — which also covers *expired*, because the deadline passing
/// aborts the upgrade and leaves nothing to be in.
#[derive(Debug, Deserialize)]
struct UpgradePhaseWire {
    album_id: String,
    #[serde(default)]
    intent_id: Option<String>,
    #[serde(default)]
    to_protocol_version: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
    in_flight: u64,
}

/// The members this module reads off an RFC 9457 problem body. `intent_id` is the `409`'s
/// extension; the rest are the coded-problem shape every Capsule refusal renders.
#[derive(Debug, Default, Deserialize)]
struct ProblemWire {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    intent_id: Option<String>,
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
            .await?;

        let status = response.status();
        if !status.is_success() {
            let problem = response.json::<ProblemWire>().await.unwrap_or_default();
            let error = refusal(status.as_u16(), problem);
            tracing::warn!(
                status = status.as_u16(),
                code = ?error.error_code(),
                "album-upgrade proposal refused"
            );
            return Err(error);
        }

        let wire: UpgradePhaseWire = response
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
fn refusal(status: u16, problem: ProblemWire) -> UpgradeError {
    let ProblemWire {
        code,
        detail,
        intent_id,
    } = problem;
    let detail = detail.unwrap_or_default();
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
        500 => UpgradeError::Unavailable { code, detail },
        other => UpgradeError::Unexpected { status: other },
    }
}

/// Parse the phase document into its typed shape.
///
/// The ids become [`Uuid`]s and the deadline a [`jiff::Timestamp`], so a caller compares
/// instants rather than strings — the deadline is the one field in this ceremony where a
/// string comparison would be a correctness bug rather than an inconvenience.
fn decode_phase(wire: UpgradePhaseWire) -> Result<UpgradePhase, UpgradeError> {
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
    Ok(UpgradePhase {
        album_id,
        intent_id,
        to_protocol_version: wire.to_protocol_version,
        expires_at,
        in_flight: wire.in_flight,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use capsule_i18n::error_codes;

    use super::*;
    use crate::auth::{AuthClient, PersistedSession};
    use crate::testmock::{MockRequest, MockResponse, MockServer};

    const ALBUM: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e60";
    const INTENT: &str = "019a0000-0000-7000-8000-00000000cafe";

    /// The bytes a real client would hand over: opaque, and deliberately not valid UTF-8 so a
    /// re-encoding anywhere on the path would show up.
    fn signed_intent() -> Vec<u8> {
        let mut bytes = b"signed-upgrade-intent".to_vec();
        bytes.extend_from_slice(&[0x00, 0xff, 0xa5]);
        bytes
    }

    fn album() -> Uuid {
        Uuid::parse_str(ALBUM).expect("the literal is a uuid")
    }

    /// A session over `base` with a far-future token, so no refresh ever fires and the mock
    /// needs no `/refresh` endpoint.
    fn session_for(base: &str) -> Session {
        AuthClient::new(base)
            .expect("a base url")
            .resume(PersistedSession {
                access_token: "test-access".to_string().into(),
                refresh_token: "test-refresh".to_string().into(),
                access_expires_at_unix: jiff::Timestamp::now().as_second() + 3_600,
            })
            .expect("a session resumes from any pair")
    }

    fn phase_json() -> String {
        serde_json::json!({
            "album_id": ALBUM,
            "intent_id": INTENT,
            "to_protocol_version": "2030-01-01",
            "expires_at": "2030-01-01T00:05:00Z",
            "in_flight": 0,
        })
        .to_string()
    }

    /// An RFC 9457 problem, as `capsule-server`'s interceptor renders one.
    fn problem(status: u16, reason: &str, code: &str, detail: &str) -> MockResponse {
        MockResponse::new(status, reason).json_body(
            serde_json::json!({
                "type": "about:blank",
                "title": reason,
                "status": status,
                "detail": detail,
                "code": code,
            })
            .to_string(),
        )
    }

    /// The signed bytes reach the documented path, in the documented media type, unchanged —
    /// and the phase decodes into typed ids and a real instant.
    #[tokio::test]
    async fn a_proposal_sends_the_signed_bytes_verbatim_and_decodes_the_phase() {
        let intent = signed_intent();
        let expected = intent.clone();
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = seen.clone();
        let server = MockServer::start(move |req: &MockRequest| {
            counter.fetch_add(1, Ordering::SeqCst);
            assert_eq!(req.method, "POST");
            assert_eq!(req.path, format!("/v1/albums/{ALBUM}/upgrade"));
            assert_eq!(
                req.header("content-type"),
                Some(CBOR),
                "the intent must be sent in the media type it was signed in"
            );
            assert_eq!(
                req.body, expected,
                "a re-encoded intent no longer verifies under the proposer's DSK"
            );
            assert!(
                req.header("authorization")
                    .is_some_and(|v| v.starts_with("Bearer ")),
                "the proposal is owner-scoped"
            );
            MockResponse::new(200, "OK").json_body(phase_json())
        })
        .await;

        let client = UpgradeClient::new(session_for(&server.base_url()), &server.base_url());
        let phase = client
            .begin(album(), &intent)
            .await
            .expect("the proposal is accepted");

        assert_eq!(seen.load(Ordering::SeqCst), 1, "exactly one request");
        assert_eq!(phase.album_id, album());
        assert_eq!(
            phase.intent_id,
            Some(Uuid::parse_str(INTENT).expect("a uuid"))
        );
        assert_eq!(phase.to_protocol_version.as_deref(), Some("2030-01-01"));
        assert_eq!(
            phase.expires_at,
            Some("2030-01-01T00:05:00Z".parse().expect("an instant"))
        );
        assert_eq!(phase.in_flight, 0);
    }

    /// A second ceremony is refused with the live `intent_id` and the code a client localizes
    /// — the refusal an admin actually reads, so neither may be flattened into a status.
    #[tokio::test]
    async fn a_second_proposal_is_refused_with_the_live_ceremony() {
        let server = MockServer::start(move |_req: &MockRequest| {
            MockResponse::new(409, "Conflict").json_body(
                serde_json::json!({
                    "type": "about:blank",
                    "title": "Upgrade in flight",
                    "status": 409,
                    "detail": format!("album is already upgrading under {INTENT}"),
                    "code": error_codes::ALBUM_UPGRADE_IN_FLIGHT,
                    "intent_id": INTENT,
                })
                .to_string(),
            )
        })
        .await;

        let client = UpgradeClient::new(session_for(&server.base_url()), &server.base_url());
        let error = client
            .begin(album(), &signed_intent())
            .await
            .expect_err("only one ceremony may hold an album");
        let UpgradeError::InFlight { intent_id, .. } = &error else {
            panic!("expected an in-flight refusal, got {error:?}");
        };
        assert_eq!(intent_id.as_deref(), Some(INTENT));
        assert_eq!(
            error.error_code(),
            Some(error_codes::ALBUM_UPGRADE_IN_FLIGHT)
        );
    }

    /// Every refusal the operation declares maps to its own variant and keeps the server's
    /// code. A status collapsed into the wrong variant would tell an admin to fix the wrong
    /// thing — re-sign an intent that was fine, or wait out a ceremony that does not exist.
    #[tokio::test]
    async fn each_declared_refusal_keeps_its_own_identity() {
        for (status, reason, code) in [
            (400u16, "Bad Request", error_codes::ALBUM_UPGRADE_MALFORMED),
            (403, "Forbidden", error_codes::ALBUM_UPGRADE_PROPOSER),
            (404, "Not Found", error_codes::ALBUM_UPGRADE_NOT_FOUND),
            (500, "Internal Server Error", error_codes::ALBUM_UNAVAILABLE),
        ] {
            let server =
                MockServer::start(move |_req: &MockRequest| problem(status, reason, code, "no"))
                    .await;
            let client = UpgradeClient::new(session_for(&server.base_url()), &server.base_url());
            let error = client
                .begin(album(), &signed_intent())
                .await
                .expect_err("the server refused");
            assert_eq!(error.error_code(), Some(code), "status {status}: {error:?}");
            let matched = match status {
                400 => matches!(error, UpgradeError::Malformed { .. }),
                403 => matches!(error, UpgradeError::NotProposer { .. }),
                404 => matches!(error, UpgradeError::NotFound { .. }),
                500 => matches!(error, UpgradeError::Unavailable { .. }),
                _ => false,
            };
            assert!(matched, "status {status} took the wrong variant: {error:?}");
        }
    }

    /// The body-size backstop carries no problem body, so the client supplies the variant —
    /// and **no code**. Minting one here would put words in the server's mouth, and every other
    /// code this module reports is the server's own.
    #[tokio::test]
    async fn a_body_too_large_carries_no_invented_code() {
        let server = MockServer::start(move |_req: &MockRequest| {
            MockResponse::new(413, "Payload Too Large")
        })
        .await;
        let client = UpgradeClient::new(session_for(&server.base_url()), &server.base_url());
        let error = client
            .begin(album(), &signed_intent())
            .await
            .expect_err("the server refused the size");
        assert!(
            matches!(error, UpgradeError::Malformed { code: None, .. }),
            "got {error:?}"
        );
        assert_eq!(
            error.error_code(),
            None,
            "a code the server never sent is not this client's to supply"
        );
    }

    /// An undeclared status is surfaced as itself rather than guessed at.
    #[tokio::test]
    async fn an_undeclared_status_is_surfaced_as_unexpected() {
        let server =
            MockServer::start(move |_req: &MockRequest| MockResponse::new(418, "I'm a teapot"))
                .await;
        let client = UpgradeClient::new(session_for(&server.base_url()), &server.base_url());
        let error = client
            .begin(album(), &signed_intent())
            .await
            .expect_err("418 is not in the contract");
        assert!(
            matches!(error, UpgradeError::Unexpected { status: 418 }),
            "got {error:?}"
        );
        assert_eq!(error.error_code(), None);
    }

    /// A phase document whose deadline is not an instant is a malformed *response*, not a
    /// silent `None` — a dropped deadline would make a client think the ceremony never expires.
    #[tokio::test]
    async fn an_unparseable_deadline_is_a_malformed_response() {
        let server = MockServer::start(move |_req: &MockRequest| {
            MockResponse::new(200, "OK").json_body(
                serde_json::json!({
                    "album_id": ALBUM,
                    "intent_id": INTENT,
                    "expires_at": "next tuesday",
                    "in_flight": 0,
                })
                .to_string(),
            )
        })
        .await;
        let client = UpgradeClient::new(session_for(&server.base_url()), &server.base_url());
        let error = client
            .begin(album(), &signed_intent())
            .await
            .expect_err("a deadline that is not an instant is not a deadline");
        assert!(
            matches!(error, UpgradeError::MalformedResponse(_)),
            "got {error:?}"
        );
    }
}
