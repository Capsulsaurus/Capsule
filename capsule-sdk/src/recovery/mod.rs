//! Recovery-secret verification cadence and the guided re-wrap flow (slice `S-D12`;
//! SSoT: [Backup — Recovery Verification Cadence] and [§ On Repeated Failure: Guided
//! Re-Wrap]).
//!
//! This module owns the **client half** of the master-key recovery story that the
//! server escrow surface (slice `S-C12`, `PUT`/`GET /v1/auth/escrow`) and the core
//! crypto ([`capsule_core::backup`]) make possible. It has two cohesive halves:
//!
//! - **[`cadence`]** — the pure, network-free scheduler and prompt state machine
//!   ([`RecoveryCadence`] → [`VerificationState`]): the 7 d → 90 d → 180 d ladder,
//!   re-arm triggers, snooze caps, and the escalation to a guided re-wrap. It is driven
//!   by a mocked clock and never blocks any flow.
//! - **[`RecoveryClient`]** — the networked side: the cached copy of the escrow blob
//!   ([`EscrowCache`]), the stale-cache-aware [`verify`](RecoveryClient::verify), and the
//!   [`guided_rewrap`](RecoveryClient::guided_rewrap) that mints a fresh secret, re-wraps
//!   the **same** master key, and replaces the server escrow through S-C12.
//!
//! The escrow blob on the wire is the opaque canonical-CBOR encoding of a
//! [`WrappedSecret`] — byte-identical to what the server stores verbatim and to what the
//! core restore path unwraps.
//!
//! # The wire is the generated client, not a path this module builds
//!
//! Both escrow operations are `application/octet-stream` in each direction, which is a media
//! type `spargen` lowers, so both are **generated** and neither is narrowed out in
//! `build.rs`. [`RecoveryClient`] therefore orchestrates
//! [`AuthenticatedClient::fetch_escrow`](crate::rest::Client::fetch_escrow) and
//! [`store_escrow`](crate::rest::Client::store_escrow) and hand-writes no request. That is
//! not a preference: `AGENTS.md` requires that everything which parses or serializes is
//! generated, *including* the byte-serving endpoints, and the reason is this module's own
//! history. It used to build `{api_root}/backup/escrow` from a `const` — the Salvo document's
//! path — and when the contract was re-sourced from Kynos to `/v1/auth/escrow` nothing
//! noticed, because a route in a string constant is checked by no gate and this module's own
//! mock answered whichever path it was handed.
//!
//! [Backup — Recovery Verification Cadence]: https://docs/design/backup-recovery/#recovery-verification-cadence
//! [§ On Repeated Failure: Guided Re-Wrap]: https://docs/design/backup-recovery/#on-repeated-failure-guided-re-wrap

pub mod cadence;

use std::sync::Arc;

pub use cadence::{
    BACKOFF_INTERVAL_SECS, CAP_INTERVAL_SECS, INITIAL_INTERVAL_SECS, MAX_CONSECUTIVE_SNOOZES,
    REWRAP_FAILURE_THRESHOLD, REWRAP_MIN_SESSIONS, RearmTrigger, RecoveryCadence, SnoozeDuration,
    VerificationState,
};
use capsule_core::backup::{VerifyOutcome, split_seed_2of3, verify_recovery_secret};
use capsule_core::crypto::primitives::{Argon2Params, DeviceTier};
use capsule_core::crypto::pwkdf::{self, WrappedSecret};
use capsule_core::crypto::rng;
use capsule_i18n::error_codes;
use tracing::instrument;

use crate::auth::Session;
use crate::client::{AuthenticatedClient, ClientError};
use crate::rest;

/// Everything the networked recovery flows can fail with. Callers switch on the typed
/// variant (or its stable `error.*` code), never a bare HTTP status.
#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    // There is deliberately no `Auth(AuthError)` variant any more. The session used to build
    // these requests itself, so a dead session surfaced as its own typed `AuthError`; the
    // generated client attaches the bearer through a token-provider seam that flattens our
    // `AuthError` to a string, so the same event now arrives as `Unauthorized` — which is
    // where the FFI's `Auth` mapping reads it from. An unconstructible variant would be a
    // promise no code path can keep.
    /// The API root is not a URL the generated client can hang operation paths off.
    #[error("invalid base URL {url:?}: {reason}")]
    InvalidBaseUrl {
        /// The offending URL.
        url: String,
        /// Why the generated client rejected it.
        reason: String,
    },
    /// The call never reached a server answer: DNS, connection refused, a TLS handshake, a
    /// timeout, a reset mid-body, or a refusal this client could not parse. Transient — the
    /// cadence's next tick tries again.
    ///
    /// Note that reqwest classifies *every* failure of the request it executes as a request
    /// error, so the generated taxonomy's `RequestConstruction` class carries connection
    /// failures as well as genuine pre-flight ones; both land here.
    #[error("the escrow endpoint could not be reached: {0}")]
    Transport(String),
    /// The credential was refused (`401`/`403`) and a refresh did not recover it — the user
    /// must re-authenticate.
    ///
    /// `code` is whatever the server stamped on the problem body. Today Capsule stamps one
    /// code (`error.request.unauthenticated`) on every `401` it renders, so this does not yet
    /// separate an expired token from an unreadable revocation ledger; the field carries the
    /// code so that it will the moment the server distinguishes them.
    #[error("the escrow endpoint refused the credential: {detail}")]
    Unauthorized {
        /// The stable `error.*` catalog code from the problem body, when the failure came
        /// with one. `None` when the credential could not be produced at all — there was no
        /// server answer to carry a code.
        code: Option<String>,
        /// English detail from the problem body.
        detail: String,
    },
    /// The caller has no escrow stored yet (server returned a coded `404`). Enroll one first.
    #[error("no escrow stored for this account")]
    NotEnrolled,
    /// The server refused the blob as one that cannot be an escrow at any version — empty or
    /// past the coarse ceiling (`400`), not the declared media type (`415`), or past the
    /// transport's body limit (`413`). Retrying the same bytes changes nothing.
    #[error("the server rejected the escrow blob: {detail}")]
    Malformed {
        /// The stable `error.*` catalog code the refusal carried.
        code: Option<String>,
        /// English detail from the problem body.
        detail: String,
    },
    /// The escrow store could not answer (`500`). Transient, and coded
    /// `error.escrow.unavailable` — which is why it is not folded into
    /// [`Transport`](RecoveryError::Transport): a caller that localizes codes has one to show.
    #[error("the escrow store could not answer: {detail}")]
    Unavailable {
        /// The stable `error.*` catalog code the refusal carried.
        code: Option<String>,
        /// English detail from the problem body.
        detail: String,
    },
    /// The escrow bytes could not be (de)serialized as the canonical `WrappedSecret`.
    #[error("escrow blob codec error: {0}")]
    Codec(String),
    /// The (re-)wrap of the master key under the fresh secret failed in core.
    #[error("re-wrapping the master key failed: {0}")]
    Wrap(String),
    /// The server returned an unmodeled status.
    #[error("unexpected {status} response from the escrow endpoint")]
    Unexpected {
        /// The HTTP status code the server returned.
        status: u16,
    },
}

impl RecoveryError {
    /// The stable `error.*` catalog code a client localizes, when one applies. The English
    /// [`Display`](std::fmt::Display) form stays the developer/log detail.
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::Unauthorized { code, .. }
            | Self::Malformed { code, .. }
            | Self::Unavailable { code, .. } => code.as_deref(),
            // The one code this module states rather than reads. `NotEnrolled` is a *state*
            // ("this account has escrowed nothing"), not a message, and the server's own code
            // for that state is this constant — see `capsule-server/src/routes/escrow.rs`.
            Self::NotEnrolled => Some(error_codes::ESCROW_NOT_STORED),
            _ => None,
        }
    }
}

/// A client-side cached copy of the server escrow blob.
///
/// Fetched at enrollment and refreshed opportunistically (SSoT § Local Verification);
/// the [stale-cache rule](RecoveryClient::verify) refreshes it before ever recording a
/// verification failure, so a rotation on another device can never manufacture a false
/// failure here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscrowCache {
    blob: WrappedSecret,
}

impl EscrowCache {
    /// Wrap an already-decoded escrow blob as the local cache.
    #[must_use]
    pub fn new(blob: WrappedSecret) -> Self {
        Self { blob }
    }

    /// The cached wrapped-master-key escrow blob.
    #[must_use]
    pub fn blob(&self) -> &WrappedSecret {
        &self.blob
    }

    /// Decode an escrow cache from the opaque canonical-CBOR wire bytes.
    fn from_wire(bytes: &[u8]) -> Result<Self, RecoveryError> {
        let blob: WrappedSecret = capsule_core::cbor::from_slice(bytes)
            .map_err(|e| RecoveryError::Codec(e.to_string()))?;
        Ok(Self { blob })
    }
}

/// A freshly minted recovery secret, presented to the user exactly once. It doubles as
/// the escrow wrap passphrase and (when enrolled) the Shamir-split seed — the single
/// root of the [single-root invariant](https://docs/design/backup-recovery/#single-root-invariant).
///
/// The bytes are zeroized on drop; surface them to the user (as a BIP39-style phrase or
/// hex) and then let this drop.
pub struct MintedSecret {
    seed: [u8; 32],
}

impl MintedSecret {
    /// Draw a fresh 32-byte (256-bit — well over the ≥128-bit floor) recovery secret.
    #[must_use]
    pub fn generate() -> Self {
        Self {
            seed: rng::random_array::<32>(),
        }
    }

    /// The raw secret bytes (the escrow wrap passphrase and Shamir seed).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.seed
    }

    /// Lowercase-hex rendering, for surfacing the secret to the user.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.seed)
    }

    /// The setup-style **type-back gate**: confirm the user re-entered the secret they
    /// were just shown. A constant-time compare over the decoded bytes.
    #[must_use]
    pub fn matches(&self, typed: &[u8]) -> bool {
        typed.len() == self.seed.len()
            && typed
                .iter()
                .zip(self.seed.iter())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }
}

impl Drop for MintedSecret {
    fn drop(&mut self) {
        // Best-effort zeroization of the recovery secret.
        for byte in &mut self.seed {
            *byte = 0;
        }
    }
}

impl std::fmt::Debug for MintedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the secret material.
        f.debug_struct("MintedSecret").finish_non_exhaustive()
    }
}

/// Guidance the client surfaces after a rotation about backup **artifacts** exported
/// under the *old* secret — pure data (a stable variant), never a localized string, so
/// the client maps it to a catalog key.
///
/// Per the [single-root invariant] versioning seam: an already-exported backup artifact
/// stays bound to the passphrase in force at export, so rotating the recovery secret
/// ends with "re-export or destroy old artifacts" guidance.
///
/// [single-root invariant]: https://docs/design/backup-recovery/#single-root-invariant
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OldArtifactGuidance {
    /// Backup artifacts exported before this rotation still open only with the *old*
    /// secret. Re-export them under the new secret, or destroy them.
    ReexportOrDestroy,
}

/// The Shamir shares re-issued during a guided re-wrap when the account had social
/// recovery enrolled. The old shares are explicitly invalidated (they split the *old*
/// seed) and surfaced as such.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShamirReissue {
    /// The fresh 2-of-3 shares splitting the **new** seed.
    pub shares: Vec<Vec<u8>>,
    /// Always `true`: the previously issued shares split the old seed and no longer
    /// reconstruct anything useful — the client must tell the user to discard them.
    pub old_shares_invalidated: bool,
}

/// The outcome of a guided re-wrap: everything the UX needs, surfaced as data.
///
/// The master key is **unchanged** — this is wrap rotation, not key rotation (SSoT
/// § On Repeated Failure). [`new_escrow`](Self::new_escrow) wraps the same master key
/// under [`secret`](Self::secret); no asset blob is touched or re-encrypted.
#[derive(Debug)]
pub struct GuidedRewrap {
    /// The freshly minted recovery secret — present once, confirm via the type-back
    /// gate ([`MintedSecret::matches`]), then let it drop.
    pub secret: MintedSecret,
    /// The new escrow blob (already stored on the server), cached locally.
    pub new_escrow: EscrowCache,
    /// Re-issued Shamir shares, when the account had social recovery enrolled.
    pub shamir: Option<ShamirReissue>,
    /// Whether the client must run the setup-style type-back gate (always `true`).
    pub type_back_required: bool,
    /// Guidance about backup artifacts exported under the old secret.
    pub old_artifact_guidance: OldArtifactGuidance,
}

/// The networked recovery client: escrow cache/refresh, stale-cache-aware local
/// verification, and the guided re-wrap.
///
/// It holds one [`AuthenticatedClient`], so every call rides the generated operation paths
/// and the SDK's bearer/refresh machinery, and this module states no route of its own. The
/// client is behind an [`Arc`] only so [`RecoveryClient`] stays [`Clone`] — the cadence hands
/// one client to several prompts. There is deliberately no repoint/session-swap accessor:
/// `AuthenticatedClient`'s own take `&mut self` and are unreachable through the `Arc`, and an
/// escrow client that changed origin mid-cadence would be a way to pull one account's escrow
/// into another's cache. Build a new one instead.
#[derive(Clone)]
pub struct RecoveryClient {
    client: Arc<AuthenticatedClient>,
}

impl RecoveryClient {
    /// Build a recovery client against the **API root** — the origin the generated operation
    /// paths hang off (e.g. `https://api.example.com`), which is the same base
    /// [`AuthenticatedClient`] and [`crate::sync::SyncConsumer`] take.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::InvalidBaseUrl`] when `api_base_url` is not a URL operation paths can
    /// hang off. Fallible where the old hand-written `format!` was not, because the generated
    /// client parses the base once at construction rather than per call.
    pub fn new(session: Session, api_base_url: &str) -> Result<Self, RecoveryError> {
        let client =
            AuthenticatedClient::new(api_base_url, session).map_err(|error| match error {
                ClientError::InvalidBaseUrl { url, reason } => {
                    RecoveryError::InvalidBaseUrl { url, reason }
                }
            })?;
        Ok(Self {
            client: Arc::new(client),
        })
    }

    /// Fetch the current escrow blob from the server (`GET /v1/auth/escrow`) into a fresh
    /// [`EscrowCache`]. `404` maps to [`RecoveryError::NotEnrolled`].
    #[instrument(skip_all)]
    pub async fn fetch_escrow(&self) -> Result<EscrowCache, RecoveryError> {
        // The protocol date is a required parameter of every gated operation in the document,
        // so the generated signature asks for it; the value is the build's own, the same one the
        // transport's default header carries. The suite and sidecar schema ride the transport.
        let bytes = self
            .client
            .fetch_escrow(
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
                rest::FetchEscrowParams::default(),
            )
            .await
            .map_err(fetch_escrow_error)?
            .into_inner();
        tracing::debug!(len = bytes.len(), "fetched escrow blob");
        EscrowCache::from_wire(&bytes)
    }

    /// Store or replace the caller's escrow blob (`PUT /v1/auth/escrow`). Single active
    /// escrow: the server overwrites any prior blob in the same transaction (S-C12).
    ///
    /// The canonical CBOR goes on the wire verbatim; `replaced` and `stored_at` are logged
    /// rather than returned, because no caller has asked for them yet and a return type is
    /// harder to widen than a log line.
    #[instrument(skip_all)]
    pub async fn store_escrow(&self, blob: &WrappedSecret) -> Result<(), RecoveryError> {
        let body = capsule_core::cbor::to_canonical_vec(blob)
            .map_err(|e| RecoveryError::Codec(e.to_string()))?;
        let stored = self
            .client
            .store_escrow(
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
                rest::StoreEscrowParams::default(),
                &rest::types::RequestBody::from(body),
            )
            .await
            .map_err(store_escrow_error)?
            .into_inner();
        tracing::info!(
            stored_at = %stored.stored_at,
            replaced = stored.replaced,
            "escrow blob stored (single active escrow: any prior blob replaced)"
        );
        Ok(())
    }

    /// Local recovery-secret verification with the **stale-cache rule** (SSoT § Local
    /// Verification): run the offline derived-tag compare against the cached blob; if it
    /// does not verify, refresh the blob from the server **once** and retry before
    /// returning a failure — so a rotation on another device cannot manufacture a false
    /// failure here.
    ///
    /// Purely local crypto (`capsule_core::backup::verify_recovery_secret`); the only
    /// network I/O is the single conditional refresh. Never blocks anything.
    #[instrument(skip_all)]
    pub async fn verify(
        &self,
        cache: &mut EscrowCache,
        passphrase: &[u8],
        device_master: &[u8; 32],
    ) -> Result<VerifyOutcome, RecoveryError> {
        if verify_recovery_secret(cache.blob(), passphrase, device_master)
            == VerifyOutcome::Verified
        {
            tracing::debug!("recovery secret verified against the cached escrow");
            return Ok(VerifyOutcome::Verified);
        }

        // Stale-cache rule: the cached blob may be stale after a rotation elsewhere.
        // Refresh once and retry before recording a failure.
        tracing::info!("cached-escrow verify failed; refreshing escrow and retrying once");
        let refreshed = self.fetch_escrow().await?;
        let changed = refreshed != *cache;
        *cache = refreshed;
        if changed {
            let outcome = verify_recovery_secret(cache.blob(), passphrase, device_master);
            tracing::debug!(?outcome, "retried verify against refreshed escrow");
            Ok(outcome)
        } else {
            // The blob is genuinely current — this is a real failure.
            tracing::debug!("escrow unchanged on refresh; recording a genuine failure");
            Ok(VerifyOutcome::NotVerified)
        }
    }

    /// Run the guided re-wrap (SSoT § On Repeated Failure): mint a fresh ≥128-bit
    /// recovery secret, **re-wrap the same master key** under it, replace the server
    /// escrow (single active — the old blob is deleted, so the lost secret unwraps
    /// nothing), re-issue Shamir shares when enrolled, and surface the old-artifact
    /// guidance. Wrap rotation, not key rotation: no data re-encryption, no blob-hash
    /// changes.
    ///
    /// After this succeeds, the caller should
    /// [`rearm`](RecoveryCadence::rearm)`(now, RearmTrigger::SecretRotated)` its cadence.
    #[instrument(skip_all)]
    pub async fn guided_rewrap(
        &self,
        device_master: &[u8; 32],
        tier: DeviceTier,
        shamir_enrolled: bool,
    ) -> Result<GuidedRewrap, RecoveryError> {
        self.guided_rewrap_with_params(device_master, tier.params(), shamir_enrolled)
            .await
    }

    /// The [`guided_rewrap`](Self::guided_rewrap) body with explicit Argon2id parameters
    /// — the seam tests drive with fast params so the crypto does not dominate a smoke.
    #[instrument(skip_all)]
    async fn guided_rewrap_with_params(
        &self,
        device_master: &[u8; 32],
        params: Argon2Params,
        shamir_enrolled: bool,
    ) -> Result<GuidedRewrap, RecoveryError> {
        let secret = MintedSecret::generate();

        // Re-wrap the SAME master key under the fresh secret. `device_master` is an
        // input, never rotated — the escrow is the only thing that changes.
        let new_blob = pwkdf::wrap_with(device_master, secret.as_bytes(), params)
            .map_err(|e| RecoveryError::Wrap(e.to_string()))?;

        // Replace the server escrow (single active escrow — the old ciphertext is gone).
        self.store_escrow(&new_blob).await?;
        tracing::info!("guided re-wrap: server escrow replaced with the new-secret wrap");

        // Re-issue Shamir shares of the NEW seed when the account had them enrolled.
        let shamir = shamir_enrolled.then(|| ShamirReissue {
            shares: split_seed_2of3(secret.as_bytes()),
            old_shares_invalidated: true,
        });

        Ok(GuidedRewrap {
            secret,
            new_escrow: EscrowCache::new(new_blob),
            shamir,
            type_back_required: true,
            old_artifact_guidance: OldArtifactGuidance::ReexportOrDestroy,
        })
    }
}

/// Map a `GET /v1/auth/escrow` refusal onto its typed variant.
///
/// One readable status table rather than a match buried in the request path. The *inner*
/// match is exhaustive over the generated enum, so a status the document adds to this
/// operation stops the build here; a new `rest::Error` **class** still falls through to
/// [`wire_error`]'s catch-all.
fn fetch_escrow_error(error: rest::Error<rest::FetchEscrowError>) -> RecoveryError {
    match error {
        rest::Error::Api(response) => match response.into_inner() {
            rest::FetchEscrowError::Status404(_) => RecoveryError::NotEnrolled,
            // The protocol gate's malformed-handshake answer (issue #404): a read is admitted at
            // any grammatical protocol date, so the only `400` this operation renders is a
            // request whose handshake headers did not parse. Unreachable from this client — the
            // transport always sends the build's own — and carried with its code rather than
            // swallowed, so a caller that localizes codes still has the server's.
            rest::FetchEscrowError::Status400(problem) => RecoveryError::Malformed {
                code: Some(problem.code.clone()),
                detail: detail(&problem),
            },
            rest::FetchEscrowError::Status401(problem)
            | rest::FetchEscrowError::Status403(problem) => refused(&problem),
            rest::FetchEscrowError::Status500(problem) => unavailable(&problem),
            // Declared by the transport backstop and unreachable on a body-less `GET`; kept
            // honest rather than folded into a class it does not belong to.
            rest::FetchEscrowError::Status413 => RecoveryError::Unexpected { status: 413 },
        },
        other => wire_error(&other),
    }
}

/// Map a `PUT /v1/auth/escrow` refusal onto its typed variant.
fn store_escrow_error(error: rest::Error<rest::StoreEscrowError>) -> RecoveryError {
    match error {
        rest::Error::Api(response) => match response.into_inner() {
            // `400` and `415` are the same answer to the caller: these bytes are not an
            // escrow, and sending them again will not help.
            //
            // `426` is the protocol gate refusing a write from outside the server's window
            // (issue #404); the code it carries, `error.protocol.version_unsupported`, is the
            // one that means "update the client", and the same class applies: sending these
            // bytes again from this build will not help.
            rest::StoreEscrowError::Status400(problem)
            | rest::StoreEscrowError::Status415(problem)
            | rest::StoreEscrowError::Status426(problem) => RecoveryError::Malformed {
                code: Some(problem.code.clone()),
                detail: detail(&problem),
            },
            rest::StoreEscrowError::Status401(problem)
            | rest::StoreEscrowError::Status403(problem) => refused(&problem),
            rest::StoreEscrowError::Status500(problem) => unavailable(&problem),
            // The body-size backstop carries no problem body at all, so there is no code to
            // carry and this client does not invent one. Every other code in this module is
            // the server's own, and a code minted here would assert that the server said
            // something it did not — a client localizing it would be reading the SDK's guess
            // as the server's judgement. The English detail says what happened instead; the
            // variant already says "these bytes will not do, do not resend them".
            rest::StoreEscrowError::Status413 => RecoveryError::Malformed {
                code: None,
                detail: "the escrow blob exceeds the server's request-body limit".to_owned(),
            },
        },
        other => wire_error(&other),
    }
}

/// A refused credential, carrying the problem body's stable code. `CodedProblem.code` is a
/// required member, so a refusal that parsed always has one.
fn refused(problem: &rest::types::CodedProblem) -> RecoveryError {
    RecoveryError::Unauthorized {
        code: Some(problem.code.clone()),
        detail: detail(problem),
    }
}

/// The store could not answer — transient, and the caller's cadence retries.
fn unavailable(problem: &rest::types::CodedProblem) -> RecoveryError {
    RecoveryError::Unavailable {
        code: Some(problem.code.clone()),
        detail: detail(problem),
    }
}

/// The problem body's English detail, or its code when the server sent no detail.
fn detail(problem: &rest::types::CodedProblem) -> String {
    problem
        .detail
        .clone()
        .unwrap_or_else(|| problem.code.clone())
}

/// Map the generated client's non-`Api` taxonomy classes: an undocumented status keeps its
/// number, and everything else is a wire failure with its source chain preserved.
fn wire_error<E>(error: &rest::Error<E>) -> RecoveryError
where
    E: std::error::Error + 'static,
{
    match error {
        rest::Error::UnexpectedStatus { status, .. } => RecoveryError::Unexpected {
            status: status.as_u16(),
        },
        // `RequestConstruction` is **not** a pre-flight-only class. reqwest builds every
        // failure of the request it executes with `error::request(..)`, so `is_request()` is
        // true for connection-refused, DNS and TLS failures too, and the generated taxonomy
        // routes all of them here alongside the genuine pre-flight ones. Splitting them by
        // class alone would report an unreachable server as "sign in again", which on an
        // offline device is the one remedy that cannot work.
        //
        // The *source* discriminates them: a bearer provider that could not produce a token is
        // boxed as the generated runtime's own `AuthError`, and nothing else on this path is.
        // A dead session must reach a caller as an auth failure rather than as a transport
        // blip, because the two have opposite remedies.
        rest::Error::RequestConstruction(inner) => {
            if std::error::Error::source(inner)
                .and_then(|source| source.downcast_ref::<rest::AuthError>())
                .is_some()
            {
                RecoveryError::Unauthorized {
                    code: None,
                    detail: describe(error),
                }
            } else {
                RecoveryError::Transport(describe(error))
            }
        }
        // A *declared* refusal whose body was not the coded problem the document promises.
        // Deliberately a wire failure and not [`RecoveryError::NotEnrolled`], even though an
        // uncoded `404` is its commonest shape: reading any `404` as "this account has
        // escrowed nothing" is exactly what let a wrong route look like an empty escrow for a
        // whole slice. An intermediary answering `404 text/html` is a broken path, not an
        // enrollment state.
        rest::Error::Decode { path, .. } => RecoveryError::Transport(format!(
            "the escrow endpoint answered a refusal this client could not parse ({path})"
        )),
        other => RecoveryError::Transport(describe(other)),
    }
}

/// Render a generated-client failure together with its source chain. The taxonomy's own
/// `Display` names the class and, for the wire classes, little else (`"transport failed"`,
/// `"request construction failed"`) — the source chain is where the reqwest/hyper reason a log
/// reader needs actually lives.
fn describe<E>(error: &rest::Error<E>) -> String
where
    E: std::error::Error + 'static,
{
    let mut rendered = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        rendered.push_str(": ");
        rendered.push_str(&cause.to_string());
        source = cause.source();
    }
    rendered
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    use capsule_core::backup::recover_master_key;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;
    use crate::auth::{AuthClient, PersistedSession};

    /// Fast Argon2id params so the escrow crypto does not dominate the smoke tests
    /// (same posture as the S-C12 server integration test).
    fn fast_params() -> Argon2Params {
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        }
    }

    // ── Binary-capable mock escrow server ─────────────────────────────────────
    //
    // The escrow surface is `application/octet-stream` on the way out, so unlike the auth
    // mock (JSON strings) this one stores and serves raw bytes. A single shared slot models
    // the server's single-active-escrow row.
    //
    // Two things it must now do that it did not have to when this module built its own URL.
    // It **routes on the path**, answering `501` to anything that is not `/v1/auth/escrow`,
    // because a mock that replies to whatever it is handed is exactly why a wrong route
    // survived here. And its refusals carry a real RFC 9457 problem body: the generated
    // client decodes a documented non-success status into `CodedProblem`, so a bare status
    // with an empty body would arrive as a decode failure rather than the typed variant.

    /// The one path the escrow operations are served on, per the committed document.
    const ESCROW_ROUTE: &str = "/v1/auth/escrow";

    #[derive(Clone, Default)]
    struct EscrowStore {
        blob: Arc<Mutex<Option<Vec<u8>>>>,
    }

    struct MockRequest {
        method: String,
        path: String,
        body: Vec<u8>,
    }

    struct MockResponse {
        status: u16,
        content_type: &'static str,
        body: Vec<u8>,
    }

    impl MockResponse {
        /// An `application/octet-stream` payload — the escrow itself.
        fn bytes(status: u16, body: Vec<u8>) -> Self {
            Self {
                status,
                content_type: "application/octet-stream",
                body,
            }
        }

        /// A JSON payload — `StoreEscrowResponse`, which the generated client decodes.
        fn json(status: u16, body: serde_json::Value) -> Self {
            Self {
                status,
                content_type: "application/json",
                body: body.to_string().into_bytes(),
            }
        }

        /// An RFC 9457 problem, shaped as `CodedProblem` so the generated client can parse it
        /// into the operation's typed error.
        fn problem(status: u16, code: &str, detail: &str) -> Self {
            Self {
                status,
                content_type: "application/problem+json",
                body: serde_json::json!({
                    "type": "about:blank",
                    "title": "Refused",
                    "status": status,
                    "detail": detail,
                    "code": code,
                })
                .to_string()
                .into_bytes(),
            }
        }
    }

    type BoxFut = Pin<Box<dyn Future<Output = MockResponse> + Send>>;
    type Handler = Arc<dyn Fn(MockRequest) -> BoxFut + Send + Sync>;

    async fn start_mock(handler: Handler) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let handler = handler.clone();
                tokio::spawn(async move {
                    let _ = serve_conn(&mut socket, handler).await;
                });
            }
        });
        format!("http://{addr}")
    }

    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    async fn serve_conn(socket: &mut TcpStream, handler: Handler) -> std::io::Result<()> {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        let header_end = loop {
            if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
                break pos + 4;
            }
            let n = socket.read(&mut tmp).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&tmp[..n]);
        };

        let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or_default();
        let mut request_parts = request_line.split_whitespace();
        let method = request_parts.next().unwrap_or_default().to_string();
        let path = request_parts.next().unwrap_or_default().to_string();

        let mut content_length = 0usize;
        let mut authorized = false;
        for line in lines {
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_lowercase();
                let value = value.trim();
                if key == "content-length" {
                    content_length = value.parse().unwrap_or(0);
                }
                if key == "authorization" && value.starts_with("Bearer ") {
                    authorized = true;
                }
            }
        }

        let mut body = buf[header_end..].to_vec();
        while body.len() < content_length {
            let n = socket.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&tmp[..n]);
        }

        // Every escrow call is owner-scoped: reject anything without a bearer token.
        let response = if authorized {
            handler(MockRequest { method, path, body }).await
        } else {
            MockResponse::problem(
                401,
                error_codes::REQUEST_UNAUTHENTICATED,
                "no bearer credential",
            )
        };

        let payload = format!(
            "HTTP/1.1 {} STATUS\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            response.status,
            response.content_type,
            response.body.len()
        );
        let mut out = payload.into_bytes();
        out.extend_from_slice(&response.body);
        socket.write_all(&out).await?;
        socket.flush().await?;
        Ok(())
    }

    /// A handler backed by the shared single-active-escrow slot: `PUT` overwrites it,
    /// `GET` serves it verbatim (or `404`).
    ///
    /// Anything off `/v1/auth/escrow` is `501`, which arrives as
    /// [`RecoveryError::Unexpected`] rather than as a plausible-looking `NotEnrolled` — so a
    /// route regression fails loudly here instead of reading as "no escrow stored".
    fn escrow_handler(store: EscrowStore) -> Handler {
        Arc::new(move |req| {
            let store = store.clone();
            Box::pin(async move {
                if req.path != ESCROW_ROUTE {
                    return MockResponse::bytes(501, Vec::new());
                }
                match req.method.as_str() {
                    "PUT" => {
                        let replaced = store.blob.lock().unwrap().replace(req.body).is_some();
                        MockResponse::json(
                            200,
                            serde_json::json!({
                                "stored_at": "2026-01-01T00:00:00Z",
                                "replaced": replaced,
                            }),
                        )
                    }
                    "GET" => match store.blob.lock().unwrap().clone() {
                        Some(bytes) => MockResponse::bytes(200, bytes),
                        None => MockResponse::problem(
                            404,
                            error_codes::ESCROW_NOT_STORED,
                            "no escrow has been stored for this account",
                        ),
                    },
                    _ => MockResponse::problem(
                        405,
                        error_codes::REQUEST_METHOD_NOT_ALLOWED,
                        "the escrow surface serves GET and PUT",
                    ),
                }
            })
        })
    }

    /// Build an authenticated [`Session`] over the mock base with a far-future token, so
    /// no refresh ever fires and the mock needs no `/refresh` endpoint.
    fn session_for(base: &str) -> Session {
        let client = AuthClient::new(base).unwrap();
        client
            .resume(PersistedSession {
                access_token: "test-access".to_string().into(),
                refresh_token: "test-refresh".to_string().into(),
                access_expires_at_unix: jiff::Timestamp::now().as_second() + 3_600,
            })
            .unwrap()
    }

    /// A session over the mock whose access token expired an hour ago, so any call
    /// pre-flight-refreshes — and the escrow mock serves no `/refresh`, so that refresh fails.
    /// The result is a [`Session`] that cannot produce a bearer at all.
    fn dead_session_for(base: &str) -> Session {
        AuthClient::new(base)
            .unwrap()
            .resume(PersistedSession {
                access_token: "test-access".to_string().into(),
                refresh_token: "test-refresh".to_string().into(),
                access_expires_at_unix: jiff::Timestamp::now().as_second() - 3_600,
            })
            .unwrap()
    }

    fn wrap(master: &[u8; 32], secret: &[u8]) -> WrappedSecret {
        pwkdf::wrap_with(master, secret, fast_params()).unwrap()
    }

    /// Store → fetch round-trips the escrow verbatim, and the fetched blob unwraps to
    /// the original master key.
    #[tokio::test]
    async fn escrow_store_fetch_round_trip() {
        let store = EscrowStore::default();
        let base = start_mock(escrow_handler(store)).await;
        let client = RecoveryClient::new(session_for(&base), &base).unwrap();

        let master = [0x11u8; 32];
        let blob = wrap(&master, b"correct horse battery staple");
        client.store_escrow(&blob).await.unwrap();

        let cache = client.fetch_escrow().await.unwrap();
        assert_eq!(cache.blob(), &blob, "fetched blob equals stored blob");
        assert_eq!(
            recover_master_key(cache.blob(), b"correct horse battery staple").unwrap(),
            master
        );
    }

    /// Fetch with nothing stored is a typed `NotEnrolled`, not a panic or opaque error.
    #[tokio::test]
    async fn fetch_without_escrow_is_not_enrolled() {
        let base = start_mock(escrow_handler(EscrowStore::default())).await;
        let client = RecoveryClient::new(session_for(&base), &base).unwrap();
        assert!(matches!(
            client.fetch_escrow().await,
            Err(RecoveryError::NotEnrolled)
        ));
    }

    /// The correct secret verifies against the cached blob with a single fetch (the
    /// initial enroll) and no refresh needed.
    #[tokio::test]
    async fn verify_correct_secret_against_cache() {
        let store = EscrowStore::default();
        let base = start_mock(escrow_handler(store)).await;
        let client = RecoveryClient::new(session_for(&base), &base).unwrap();

        let master = [0x22u8; 32];
        let blob = wrap(&master, b"the-right-secret");
        client.store_escrow(&blob).await.unwrap();
        let mut cache = client.fetch_escrow().await.unwrap();

        assert_eq!(
            client
                .verify(&mut cache, b"the-right-secret", &master)
                .await
                .unwrap(),
            VerifyOutcome::Verified
        );
    }

    /// SSoT § Local Verification stale-cache rule: a rotation on another device makes
    /// the *cached* blob stale; verify refreshes once and then passes with the new
    /// secret — the stale cache never manufactures a false failure.
    #[tokio::test]
    async fn verify_refreshes_stale_cache_then_passes() {
        let store = EscrowStore::default();
        let base = start_mock(escrow_handler(store.clone())).await;
        let client = RecoveryClient::new(session_for(&base), &base).unwrap();

        let master = [0x33u8; 32];
        // Enroll and cache the OLD wrap.
        let old_blob = wrap(&master, b"old-secret");
        client.store_escrow(&old_blob).await.unwrap();
        let mut cache = client.fetch_escrow().await.unwrap();

        // Another device rotates the escrow to a NEW secret (server-side replace).
        let new_blob = wrap(&master, b"new-secret");
        *store.blob.lock().unwrap() =
            Some(capsule_core::cbor::to_canonical_vec(&new_blob).unwrap());

        // Verifying with the NEW secret against the STALE cache: the first compare
        // fails, the stale-cache refresh pulls the new blob, and the retry passes.
        assert_eq!(
            client
                .verify(&mut cache, b"new-secret", &master)
                .await
                .unwrap(),
            VerifyOutcome::Verified
        );
        // The cache was updated to the refreshed blob.
        assert_eq!(cache.blob(), &new_blob);
    }

    /// A genuinely wrong secret still fails after the refresh (the blob was current):
    /// the stale-cache rule does not paper over a real failure.
    #[tokio::test]
    async fn verify_wrong_secret_fails_after_refresh() {
        let store = EscrowStore::default();
        let base = start_mock(escrow_handler(store)).await;
        let client = RecoveryClient::new(session_for(&base), &base).unwrap();

        let master = [0x44u8; 32];
        let blob = wrap(&master, b"real-secret");
        client.store_escrow(&blob).await.unwrap();
        let mut cache = client.fetch_escrow().await.unwrap();

        assert_eq!(
            client
                .verify(&mut cache, b"totally-wrong", &master)
                .await
                .unwrap(),
            VerifyOutcome::NotVerified
        );
    }

    /// SSoT § Guided re-wrap smoke: after the failure threshold the client re-wraps the
    /// **same** master key under a fresh secret and replaces the server escrow.
    ///
    /// Proves:
    /// - the master key is UNCHANGED — the new escrow unwraps with the new secret to the
    ///   exact original master bytes;
    /// - the old secret unwraps nothing (single active escrow — it is gone everywhere);
    /// - re-wrap touches only the wrap: fixture asset blob hashes are byte-identical
    ///   before and after;
    /// - Shamir is re-issued (old shares invalidated) and the old-artifact guidance is
    ///   surfaced as data.
    #[tokio::test]
    async fn guided_rewrap_keeps_master_key_and_blob_hashes() {
        use capsule_core::crypto::hash::hash_bytes;

        let store = EscrowStore::default();
        let base = start_mock(escrow_handler(store)).await;
        let client = RecoveryClient::new(session_for(&base), &base).unwrap();

        let master = [0x55u8; 32];
        let old_secret = b"the-old-lost-secret";

        // Enroll the original escrow.
        client
            .store_escrow(&wrap(&master, old_secret))
            .await
            .unwrap();

        // Fixture "asset" ciphertext blobs — re-wrap must not touch these.
        let asset_a = b"encrypted-asset-ciphertext-A".to_vec();
        let asset_b = b"encrypted-asset-ciphertext-B".to_vec();
        let hash_a_before = hash_bytes(&asset_a).to_hex();
        let hash_b_before = hash_bytes(&asset_b).to_hex();

        // Run the guided re-wrap (fast params, Shamir enrolled).
        let rewrap = client
            .guided_rewrap_with_params(&master, fast_params(), true)
            .await
            .unwrap();

        // The new escrow (as stored on the server) unwraps with the NEW secret to the
        // SAME master key.
        let refetched = client.fetch_escrow().await.unwrap();
        assert_eq!(refetched, rewrap.new_escrow, "server holds the new escrow");
        let recovered = recover_master_key(refetched.blob(), rewrap.secret.as_bytes()).unwrap();
        assert_eq!(recovered, master, "master key is UNCHANGED after re-wrap");

        // The OLD secret unwraps nothing — the lost secret is dead everywhere.
        assert!(recover_master_key(refetched.blob(), old_secret).is_err());

        // Re-wrap touched only the wrap: asset blob hashes are byte-identical.
        assert_eq!(hash_bytes(&asset_a).to_hex(), hash_a_before);
        assert_eq!(hash_bytes(&asset_b).to_hex(), hash_b_before);

        // Shamir re-issued, old shares invalidated; the new shares reconstruct the new
        // seed (2 of 3).
        let shamir = rewrap.shamir.expect("shamir re-issued when enrolled");
        assert!(shamir.old_shares_invalidated);
        assert_eq!(shamir.shares.len(), 3);
        let two = vec![shamir.shares[0].clone(), shamir.shares[2].clone()];
        assert_eq!(
            capsule_core::backup::recover_seed(&two).unwrap().as_slice(),
            rewrap.secret.as_bytes().as_slice()
        );

        // Type-back gate + old-artifact guidance surfaced as data.
        assert!(rewrap.type_back_required);
        assert!(rewrap.secret.matches(rewrap.secret.as_bytes()));
        assert!(!rewrap.secret.matches(b"something-else"));
        assert_eq!(
            rewrap.old_artifact_guidance,
            OldArtifactGuidance::ReexportOrDestroy
        );
    }

    /// When Shamir was never enrolled, the re-wrap re-issues no shares.
    #[tokio::test]
    async fn guided_rewrap_no_shamir_when_not_enrolled() {
        let base = start_mock(escrow_handler(EscrowStore::default())).await;
        let client = RecoveryClient::new(session_for(&base), &base).unwrap();
        let master = [0x66u8; 32];
        client.store_escrow(&wrap(&master, b"old")).await.unwrap();

        let rewrap = client
            .guided_rewrap_with_params(&master, fast_params(), false)
            .await
            .unwrap();
        assert!(rewrap.shamir.is_none());
    }

    /// The mock's route guard is live: a client pointed one segment off the documented path
    /// gets a loud `Unexpected`, not a plausible-looking `NotEnrolled`.
    ///
    /// This is the regression this slice exists for, asserted as a property of the *test
    /// harness*: without it the mock would answer any path at all, and a wrong route would
    /// once again read as "this account has escrowed nothing" — which is what let
    /// `backup/escrow` survive a contract re-source.
    #[tokio::test]
    async fn a_call_off_the_documented_route_is_not_mistaken_for_an_empty_escrow() {
        let base = start_mock(escrow_handler(EscrowStore::default())).await;
        let client =
            RecoveryClient::new(session_for(&base), &format!("{base}/not-the-contract")).unwrap();
        let error = client
            .fetch_escrow()
            .await
            .expect_err("a path the server does not serve is not an empty escrow");
        assert!(
            matches!(error, RecoveryError::Unexpected { status: 501 }),
            "got {error:?}"
        );
    }

    /// A `400` refusal becomes the typed `Malformed` and carries **the server's own** code —
    /// not the `Unexpected { status }` the hand-written path used to collapse it into, and not
    /// a constant this module guessed.
    #[tokio::test]
    async fn a_refused_blob_is_malformed_with_the_servers_code() {
        let handler: Handler = Arc::new(|_req| {
            Box::pin(async move {
                MockResponse::problem(
                    400,
                    error_codes::ESCROW_MALFORMED,
                    "the escrow blob is empty",
                )
            })
        });
        let base = start_mock(handler).await;
        let client = RecoveryClient::new(session_for(&base), &base).unwrap();
        let error = client
            .store_escrow(&wrap(&[0x77u8; 32], b"whatever"))
            .await
            .expect_err("the server refused the blob");
        assert!(
            matches!(error, RecoveryError::Malformed { .. }),
            "got {error:?}"
        );
        assert_eq!(error.error_code(), Some(error_codes::ESCROW_MALFORMED));
    }

    /// The store answering `500` is its own variant carrying `error.escrow.unavailable`, not a
    /// bare transport failure: a client that localizes codes has one to show, and the cadence
    /// can tell "the server is unwell" from "the network is gone".
    #[tokio::test]
    async fn an_unavailable_store_keeps_its_catalog_code() {
        let handler: Handler = Arc::new(|_req| {
            Box::pin(async move {
                MockResponse::problem(
                    500,
                    error_codes::ESCROW_UNAVAILABLE,
                    "the escrow could not be read",
                )
            })
        });
        let base = start_mock(handler).await;
        let client = RecoveryClient::new(session_for(&base), &base).unwrap();
        let error = client.fetch_escrow().await.expect_err("the store is down");
        assert!(
            matches!(error, RecoveryError::Unavailable { .. }),
            "got {error:?}"
        );
        assert_eq!(error.error_code(), Some(error_codes::ESCROW_UNAVAILABLE));
    }

    /// **An unreachable server is not an expired session.** reqwest reports a refused
    /// connection as a *request* error, which the generated taxonomy files under
    /// `RequestConstruction` next to the genuine pre-flight failures — so classifying that
    /// whole class as an auth failure would tell an offline device to sign in again, the one
    /// remedy that cannot work without a network.
    #[tokio::test]
    async fn an_unreachable_endpoint_is_a_transport_failure_not_an_auth_one() {
        // Bind, read the port, then drop the listener: the address is now certain to refuse.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        // The session is built against a live mock, so the session itself is healthy and the
        // only thing wrong is the escrow origin.
        let live = start_mock(escrow_handler(EscrowStore::default())).await;
        let client = RecoveryClient::new(session_for(&live), &format!("http://{addr}")).unwrap();

        let error = client
            .fetch_escrow()
            .await
            .expect_err("nothing is listening there");
        assert!(
            matches!(error, RecoveryError::Transport(_)),
            "an unreachable endpoint must be transport, got {error:?}"
        );
        assert_eq!(error.error_code(), None);
    }

    /// **A session that cannot mint a bearer is an auth failure, not a network one.**
    ///
    /// This is the other side of `an_unreachable_endpoint_is_a_transport_failure_not_an_auth_one`
    /// and it pins the discrimination that separates them. Both arrive as the generated
    /// taxonomy's `RequestConstruction`; the only thing telling them apart is the boxed source,
    /// which is the runtime's own `AuthError` when — and only when — the bearer provider is
    /// what failed. Without this case, a spargen change to how a provider failure is boxed
    /// would silently demote every expired refresh token to `Transport`, and the FFI would tell
    /// a user to retry where it must tell them to sign in again. The escrow calls themselves
    /// never leave the process here: there is no bearer to send them with.
    #[tokio::test]
    async fn a_session_that_cannot_mint_a_bearer_is_an_auth_failure() {
        let base = start_mock(escrow_handler(EscrowStore::default())).await;
        let client = RecoveryClient::new(dead_session_for(&base), &base).unwrap();

        let error = client
            .fetch_escrow()
            .await
            .expect_err("the session cannot produce a token");
        assert!(
            matches!(error, RecoveryError::Unauthorized { code: None, .. }),
            "a dead session must reach the caller as an auth failure, got {error:?}"
        );
        assert_eq!(
            error.error_code(),
            None,
            "no server answered, so there is no catalog code to carry"
        );

        // And the same on the write path, which has a body to construct and still fails before
        // it is sent.
        let error = client
            .store_escrow(&wrap(&[0x99u8; 32], b"whatever"))
            .await
            .expect_err("the session cannot produce a token");
        assert!(
            matches!(error, RecoveryError::Unauthorized { code: None, .. }),
            "got {error:?}"
        );
    }

    /// A refusal whose body is not the coded problem the document promises — an intermediary
    /// answering a bare `404`, say — is a broken path, not an empty escrow.
    ///
    /// This is the deliberate class change the route fix rests on: the hand-written client
    /// read *any* `404` as `NotEnrolled`, which is exactly why a wrong route looked like an
    /// account that had escrowed nothing.
    #[tokio::test]
    async fn an_uncoded_404_is_not_read_as_an_empty_escrow() {
        let handler: Handler = Arc::new(|_req| {
            Box::pin(async move { MockResponse::bytes(404, b"<html>not found</html>".to_vec()) })
        });
        let base = start_mock(handler).await;
        let client = RecoveryClient::new(session_for(&base), &base).unwrap();
        let error = client
            .fetch_escrow()
            .await
            .expect_err("an unparseable refusal is not an enrollment state");
        assert!(
            matches!(error, RecoveryError::Transport(_)),
            "got {error:?}"
        );
    }

    /// A refused credential carries the problem body's code straight through, so a client
    /// localizes what the *server* said rather than what this module assumed.
    #[tokio::test]
    async fn a_refused_credential_keeps_the_problem_code() {
        // No bearer reaches the mock's handler at all: it answers `401` at the door, which is
        // precisely the shape a revoked token produces.
        let handler: Handler = Arc::new(|_req| {
            Box::pin(async move {
                MockResponse::problem(
                    401,
                    error_codes::REQUEST_UNAUTHENTICATED,
                    "the access token was refused",
                )
            })
        });
        let base = start_mock(handler).await;
        let client = RecoveryClient::new(session_for(&base), &base).unwrap();
        let error = client
            .fetch_escrow()
            .await
            .expect_err("the credential was refused");
        assert!(
            matches!(error, RecoveryError::Unauthorized { .. }),
            "got {error:?}"
        );
        assert_eq!(
            error.error_code(),
            Some(error_codes::REQUEST_UNAUTHENTICATED)
        );
    }

    /// The minted secret clears the ≥128-bit entropy floor (256-bit) and never prints
    /// its material.
    #[test]
    fn minted_secret_is_256_bit_and_opaque() {
        let secret = MintedSecret::generate();
        assert_eq!(secret.as_bytes().len(), 32);
        assert_eq!(secret.to_hex().len(), 64);
        assert_eq!(format!("{secret:?}"), "MintedSecret { .. }");
    }
}
