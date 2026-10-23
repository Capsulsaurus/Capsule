//! Recovery-secret verification cadence and the guided re-wrap flow (
//! SSoT: [Backup — Recovery Verification Cadence] and [§ On Repeated Failure: Guided
//! Re-Wrap]).
//!
//! This module owns the **client half** of the master-key recovery story that the
//! server escrow surface (`PUT`/`GET /v1/auth/escrow`) and the core
//! crypto ([`capsule_core::backup`]) make possible. It has two cohesive halves:
//!
//! - **[`cadence`]** — the pure, network-free scheduler and prompt state machine
//!   ([`RecoveryCadence`] → [`VerificationState`]): the 7 d → 90 d → 180 d ladder,
//!   re-arm triggers, snooze caps, and the escalation to a guided re-wrap. It is driven
//!   by a mocked clock and never blocks any flow.
//! - **[`RecoveryClient`]** — the networked side: the cached copy of the escrow blob
//!   ([`EscrowCache`]), the stale-cache-aware [`verify`](RecoveryClient::verify), and the
//!   [`guided_rewrap`](RecoveryClient::guided_rewrap) that mints a fresh secret, re-wraps
//!   the **same** master key, and replaces the server escrow through the server's
//!   escrow endpoint. It also stores and fetches the **sealed identity** beside the escrow
//!   ([`store_sealed_identity`](RecoveryClient::store_sealed_identity),
//!   [`fetch_sealed_identity`](RecoveryClient::fetch_sealed_identity),
//!   `/v1/auth/escrow/identity`, #544): the user identity key sealed under the master key,
//!   which a recovered device opens beside the recovered master key to come up as the same
//!   account. And it stores and lists the account's **album key escrows**
//!   ([`store_album_keys`](RecoveryClient::store_album_keys),
//!   [`list_album_keys`](RecoveryClient::list_album_keys), `/v1/auth/escrow/albums`, #741):
//!   each album's keys sealed under the master key, which a recovered device adopts so it can
//!   open the account's assets.
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

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
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
use jiff::Timestamp;
use tracing::instrument;
use uuid::Uuid;

use crate::auth::Session;
use crate::client::{
    AuthenticatedClient, ClientError, describe_error, is_token_failure, upgrade_required_of,
};
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
    /// The caller has stored no sealed identity (`GET /v1/auth/escrow/identity` answered a coded
    /// `404`). The escrow alone cannot open the account as itself: a device that still holds the
    /// identity has to store it ([`RecoveryClient::store_sealed_identity`]) first.
    #[error("no sealed identity stored for this account")]
    IdentityNotStored,
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
    /// The account's album key escrows are at the server's per-account bound (`409`, coded
    /// `error.escrow.album_keys_full`): nothing was stored, and resending will not fit either.
    #[error("the account's album key escrows are full: {detail}")]
    AlbumKeysFull {
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
    /// A success answer this client could not read — a `stored_at` that is not an RFC 3339
    /// instant. The server *did* answer `200`, so the escrow may well be stored; this is the
    /// answer being unreadable, not the write being refused.
    #[error("the escrow endpoint's answer could not be read: {0}")]
    InvalidResponse(String),
    /// The (re-)wrap of the master key under the fresh secret failed in core.
    #[error("re-wrapping the master key failed: {0}")]
    Wrap(String),
    /// The server returned a status this client does not model as an escrow outcome — an
    /// unmodeled status, or the body-size backstop's `413` on a read.
    #[error("unexpected {status} response from the escrow endpoint")]
    Unexpected {
        /// The HTTP status code the server returned.
        status: u16,
        /// The stable `error.*` catalog code the response carried, when it came with a coded
        /// problem body. `None` when there was no body to read a code from.
        code: Option<String>,
    },
    /// The server will not take this client's writes: the protocol gate's `426`, or a window
    /// it advertised that already refuses this build, so the write never left (issue #674).
    /// Nothing about the escrow is wrong — update the client.
    #[error(transparent)]
    UpgradeRequired(Box<crate::net::negotiation::UpgradeRequired>),
}

impl RecoveryError {
    /// The stable `error.*` catalog code a client localizes, when one applies. The English
    /// [`Display`](std::fmt::Display) form stays the developer/log detail.
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::Unauthorized { code, .. }
            | Self::Malformed { code, .. }
            | Self::AlbumKeysFull { code, .. }
            | Self::Unavailable { code, .. }
            | Self::Unexpected { code, .. } => code.as_deref(),
            Self::UpgradeRequired(refusal) => Some(refusal.error_code()),
            // The one code this module states rather than reads. `NotEnrolled` is a *state*
            // ("this account has escrowed nothing"), not a message, and the server's own code
            // for that state is this constant — see `capsule-server/src/routes/escrow.rs`.
            Self::NotEnrolled => Some(error_codes::ESCROW_NOT_STORED),
            // The same reasoning, for the sealed identity's own `404` state.
            Self::IdentityNotStored => Some(error_codes::ESCROW_IDENTITY_NOT_STORED),
            _ => None,
        }
    }
}

/// What storing an escrow did: the server's `StoreEscrowResponse`, read.
///
/// Both members are for a client. [`stored_at`](Self::stored_at) is what lets it tell
/// whether a cached copy is current (the [stale-cache rule](RecoveryClient::verify));
/// [`replaced`](Self::replaced) separates a first escrow, which completes account setup,
/// from a replacement, which means the *previous* recovery secret has stopped working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EscrowStored {
    /// When the server accepted the blob, on the server's clock.
    pub stored_at: Timestamp,
    /// Whether this displaced an earlier escrow.
    pub replaced: bool,
}

impl EscrowStored {
    /// The guidance about backup artifacts under the old secret that this store calls for:
    /// [`ReexportOrDestroy`](OldArtifactGuidance::ReexportOrDestroy) when it displaced an
    /// earlier escrow, and none for a first enrollment — there was no earlier secret for an
    /// artifact to be bound to.
    #[must_use]
    pub fn old_artifact_guidance(&self) -> Option<OldArtifactGuidance> {
        self.replaced
            .then_some(OldArtifactGuidance::ReexportOrDestroy)
    }
}

/// What storing a sealed identity did: the server's `StoreSealedIdentityResponse`, read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealedIdentityStored {
    /// When the server accepted the blob, on the server's clock.
    pub stored_at: Timestamp,
    /// Whether this displaced an earlier sealed identity. Unlike an escrow replacement it retires
    /// no secret: the identity key is the same one, sealed again.
    pub replaced: bool,
}

/// What storing an album key escrow did: the server's `StoreAlbumKeysResponse`, read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlbumKeysStored {
    /// When the server accepted the blob, on the server's clock.
    pub stored_at: Timestamp,
    /// Whether this displaced an earlier escrow of the same album's keys.
    pub replaced: bool,
}

/// One album's key escrow as the server holds it: the album's whole key state, sealed under the
/// master key by `Workspace::album_key_escrow`, and opened by
/// `Workspace::adopt_album_key_escrows`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscrowedAlbumKeys {
    /// The album the keys are of.
    pub album_id: Uuid,
    /// The sealed keys, exactly as stored.
    pub blob: Vec<u8>,
    /// When the server accepted them.
    pub stored_at: Timestamp,
}

/// A client-side cached copy of the server escrow blob.
///
/// Fetched at enrollment and refreshed opportunistically (SSoT § Local Verification);
/// the [stale-cache rule](RecoveryClient::verify) refreshes it before ever recording a
/// verification failure, so a rotation on another device can never manufacture a false
/// failure here.
///
/// It carries the blob's [`stored_at`](Self::stored_at) when this client knows it — from
/// its own [`store_escrow`](RecoveryClient::store_escrow), or from a
/// [`verify`](RecoveryClient::verify) that refreshed against a known instant. A blob
/// fetched with `GET /v1/auth/escrow` alone has none: that operation serves the bytes and
/// nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscrowCache {
    blob: WrappedSecret,
    stored_at: Option<Timestamp>,
}

impl EscrowCache {
    /// Wrap an already-decoded escrow blob as the local cache, with no known `stored_at`.
    #[must_use]
    pub fn new(blob: WrappedSecret) -> Self {
        Self {
            blob,
            stored_at: None,
        }
    }

    /// Wrap an escrow blob whose server `stored_at` is known — the blob this client just
    /// stored, paired with the [`EscrowStored::stored_at`] the server answered.
    #[must_use]
    pub fn with_stored_at(blob: WrappedSecret, stored_at: Timestamp) -> Self {
        Self {
            blob,
            stored_at: Some(stored_at),
        }
    }

    /// The cached wrapped-master-key escrow blob.
    #[must_use]
    pub fn blob(&self) -> &WrappedSecret {
        &self.blob
    }

    /// When the server stored the cached blob, when this client knows it.
    #[must_use]
    pub fn stored_at(&self) -> Option<Timestamp> {
        self.stored_at
    }

    /// Whether an escrow stored at `latest` is newer than this copy. A copy with no known
    /// `stored_at` cannot prove it is current, so it counts as older.
    #[must_use]
    pub fn is_older_than(&self, latest: Timestamp) -> bool {
        self.stored_at.is_none_or(|stored_at| stored_at < latest)
    }

    /// Decode an escrow cache from the opaque canonical-CBOR wire bytes.
    fn from_wire(bytes: &[u8]) -> Result<Self, RecoveryError> {
        let blob: WrappedSecret = capsule_core::cbor::from_slice(bytes)
            .map_err(|e| RecoveryError::Codec(e.to_string()))?;
        Ok(Self::new(blob))
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
    /// The new escrow blob (already stored on the server), cached locally with the
    /// server's `stored_at`.
    pub new_escrow: EscrowCache,
    /// What the store did — in particular whether it displaced an earlier escrow.
    pub stored: EscrowStored,
    /// Re-issued Shamir shares, when the account had social recovery enrolled.
    pub shamir: Option<ShamirReissue>,
    /// Whether the client must run the setup-style type-back gate (always `true`).
    pub type_back_required: bool,
    /// Guidance about backup artifacts exported under the old secret: present only when the
    /// store [replaced](EscrowStored::replaced) an earlier escrow. A re-wrap on an account
    /// that had none retired no secret, so there is no old artifact to warn about.
    pub old_artifact_guidance: Option<OldArtifactGuidance>,
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
    /// escrow: the server overwrites any prior blob in the same transaction.
    ///
    /// The canonical CBOR goes on the wire verbatim. The answer comes back as
    /// [`EscrowStored`]: `stored_at` for the stale-cache rule, `replaced` to tell a first
    /// enrollment from a rotation.
    ///
    /// # Errors
    ///
    /// The refusals `store_escrow_error` maps, and [`RecoveryError::InvalidResponse`] when
    /// the server's `stored_at` is not an RFC 3339 instant.
    #[instrument(skip_all)]
    pub async fn store_escrow(&self, blob: &WrappedSecret) -> Result<EscrowStored, RecoveryError> {
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
        let stored_at = stored_instant(&stored.stored_at, "escrow")?;
        tracing::info!(
            %stored_at,
            replaced = stored.replaced,
            "escrow blob stored (single active escrow: any prior blob replaced)"
        );
        Ok(EscrowStored {
            stored_at,
            replaced: stored.replaced,
        })
    }

    /// Store or replace the caller's **sealed identity** (`PUT /v1/auth/escrow/identity`): the
    /// user identity key sealed under the master key, as `Workspace::sealed_identity` returns
    /// it. The other half of what a recovered device needs beside the escrow — the identity is
    /// random rather than master-derived, and the server pins it on first directory publish, so
    /// a recovered device must carry this one rather than mint its own.
    ///
    /// Opaque on the wire and stored independently of the escrow: a
    /// [`guided_rewrap`](Self::guided_rewrap) replaces the escrow and leaves this alone, because
    /// re-wrapping the master key under a new recovery secret does not change the master key
    /// the identity is sealed under.
    ///
    /// # Errors
    ///
    /// The same refusals [`store_escrow`](Self::store_escrow) maps, under this operation's
    /// statuses, and [`RecoveryError::InvalidResponse`] for an unreadable `stored_at`.
    #[instrument(skip_all, fields(len = sealed.len()))]
    pub async fn store_sealed_identity(
        &self,
        sealed: &[u8],
    ) -> Result<SealedIdentityStored, RecoveryError> {
        let stored = self
            .client
            .store_sealed_identity(
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
                rest::StoreSealedIdentityParams::default(),
                &rest::types::RequestBody::from(sealed.to_vec()),
            )
            .await
            .map_err(store_sealed_identity_error)?
            .into_inner();
        let stored_at = stored_instant(&stored.stored_at, "sealed identity")?;
        tracing::info!(
            %stored_at,
            replaced = stored.replaced,
            "sealed identity stored (one per account: any prior one replaced)"
        );
        Ok(SealedIdentityStored {
            stored_at,
            replaced: stored.replaced,
        })
    }

    /// Fetch the caller's sealed identity (`GET /v1/auth/escrow/identity`), verbatim — the
    /// bytes `Workspace::create_from_master` opens under the recovered master key.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::IdentityNotStored`] on the coded `404`; otherwise the refusals
    /// [`fetch_escrow`](Self::fetch_escrow) maps.
    #[instrument(skip_all)]
    pub async fn fetch_sealed_identity(&self) -> Result<Vec<u8>, RecoveryError> {
        let bytes = self
            .client
            .fetch_sealed_identity(
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
                rest::FetchSealedIdentityParams::default(),
            )
            .await
            .map_err(fetch_sealed_identity_error)?
            .into_inner();
        tracing::debug!(len = bytes.len(), "fetched the sealed identity");
        Ok(bytes.to_vec())
    }

    /// Store or replace one album's **key escrow** (`PUT /v1/auth/escrow/albums/{album_id}`,
    /// #741): the album's whole key state sealed under the master key, as
    /// `Workspace::album_key_escrow` returns it. One slot per album; storing it never touches
    /// another album's, the escrow or the sealed identity.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::Malformed`] for a blob the server refuses (empty, past the album keys'
    /// ceiling, or an album id that is not canonical), the credential and store refusals
    /// [`store_escrow`](Self::store_escrow) maps, and [`RecoveryError::InvalidResponse`] for an
    /// unreadable `stored_at`.
    #[instrument(skip(self, sealed), fields(len = sealed.len()))]
    pub async fn store_album_keys(
        &self,
        album_id: Uuid,
        sealed: &[u8],
    ) -> Result<AlbumKeysStored, RecoveryError> {
        let stored = self
            .client
            .store_album_keys(
                album_id.to_string(),
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
                rest::StoreAlbumKeysParams::default(),
                &rest::types::RequestBody::from(sealed.to_vec()),
            )
            .await
            .map_err(store_album_keys_error)?
            .into_inner();
        let stored_at = stored_instant(&stored.stored_at, "album key escrow")?;
        tracing::info!(
            %album_id,
            %stored_at,
            replaced = stored.replaced,
            "album key escrow stored (one per album: any prior one replaced)"
        );
        Ok(AlbumKeysStored {
            stored_at,
            replaced: stored.replaced,
        })
    }

    /// Every album key escrow the account holds (`GET /v1/auth/escrow/albums`), each blob
    /// verbatim, in album order. Empty for an account that has escrowed no album.
    ///
    /// # Errors
    ///
    /// The credential and store refusals [`fetch_escrow`](Self::fetch_escrow) maps, and
    /// [`RecoveryError::InvalidResponse`] when an entry's album id, blob or `stored_at` cannot
    /// be read.
    #[instrument(skip_all)]
    pub async fn list_album_keys(&self) -> Result<Vec<EscrowedAlbumKeys>, RecoveryError> {
        let listing = self
            .client
            .list_album_keys(
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
                rest::ListAlbumKeysParams::default(),
            )
            .await
            .map_err(list_album_keys_error)?
            .into_inner();
        let albums = listing
            .albums
            .into_iter()
            .map(read_escrowed_album_keys)
            .collect::<Result<Vec<_>, RecoveryError>>()?;
        tracing::debug!(albums = albums.len(), "fetched the album key escrows");
        Ok(albums)
    }

    /// Local recovery-secret verification with the **stale-cache rule** (SSoT § Local
    /// Verification): run the offline derived-tag compare against the cached blob; if it
    /// does not verify, refresh the blob from the server **once** and retry before
    /// returning a failure — so a rotation on another device cannot manufacture a false
    /// failure here.
    ///
    /// `latest_stored_at` is the newest escrow `stored_at` the caller has seen — an
    /// [`EscrowStored`] from any device, however it reached this one. When the cache is
    /// [older](EscrowCache::is_older_than) than it, the cache is known stale: it is refreshed
    /// **before** the compare and stamped with `latest_stored_at`, and the compare against
    /// the fresh blob is the answer, with no second fetch on a failure. `None`, or a cache at
    /// least that new, keeps the refresh-on-failure rule above.
    ///
    /// Purely local crypto (`capsule_core::backup::verify_recovery_secret`); the only
    /// network I/O is the single conditional refresh. Never blocks anything.
    #[instrument(skip_all, fields(cache_stored_at = ?cache.stored_at(), ?latest_stored_at))]
    pub async fn verify(
        &self,
        cache: &mut EscrowCache,
        passphrase: &[u8],
        device_master: &[u8; 32],
        latest_stored_at: Option<Timestamp>,
    ) -> Result<VerifyOutcome, RecoveryError> {
        if let Some(latest) = latest_stored_at.filter(|latest| cache.is_older_than(*latest)) {
            // Known stale: refresh before comparing, so the compare is against the current
            // blob and a failure is genuine on the first try.
            tracing::info!(%latest, "cached escrow predates the latest stored_at; refreshing first");
            let refreshed = self.fetch_escrow().await?;
            *cache = EscrowCache::with_stored_at(refreshed.blob, latest);
            let outcome = verify_recovery_secret(cache.blob(), passphrase, device_master);
            tracing::debug!(
                ?outcome,
                "verified against the proactively refreshed escrow"
            );
            return Ok(outcome);
        }

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
        let changed = refreshed.blob != cache.blob;
        if changed {
            // A different blob: the cached `stored_at` described the old one, and `GET` does
            // not say when this one was stored, so the refreshed copy carries none.
            *cache = refreshed;
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
    /// guidance when the store displaced an earlier escrow. Wrap rotation, not key
    /// rotation: no data re-encryption, no blob-hash changes.
    ///
    /// After this succeeds, the caller should
    /// [`rearm`](RecoveryCadence::rearm)`(now, RearmTrigger::SecretRotated)` its cadence
    /// when [`GuidedRewrap::stored`] says it [replaced](EscrowStored::replaced) an escrow; a
    /// store that replaced nothing is a first enrollment, which arms a fresh ladder with
    /// [`RecoveryCadence::armed_at_setup`].
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
        let stored = self.store_escrow(&new_blob).await?;
        tracing::info!(
            stored_at = %stored.stored_at,
            replaced = stored.replaced,
            "guided re-wrap: server escrow set to the new-secret wrap"
        );

        // Re-issue Shamir shares of the NEW seed when the account had them enrolled.
        let shamir = shamir_enrolled.then(|| ShamirReissue {
            shares: split_seed_2of3(secret.as_bytes()),
            old_shares_invalidated: true,
        });

        Ok(GuidedRewrap {
            secret,
            new_escrow: EscrowCache::with_stored_at(new_blob, stored.stored_at),
            stored,
            shamir,
            type_back_required: true,
            old_artifact_guidance: stored.old_artifact_guidance(),
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
    if let Some(refusal) = upgrade_required_of(&error) {
        return RecoveryError::UpgradeRequired(Box::new(refusal));
    }
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
            rest::FetchEscrowError::Status413(problem) => RecoveryError::Unexpected {
                status: 413,
                code: Some(problem.code.clone()),
            },
        },
        other => wire_error(&other),
    }
}

/// Map a `PUT /v1/auth/escrow` refusal onto its typed variant.
fn store_escrow_error(error: rest::Error<rest::StoreEscrowError>) -> RecoveryError {
    if let Some(refusal) = upgrade_required_of(&error) {
        return RecoveryError::UpgradeRequired(Box::new(refusal));
    }
    match error {
        rest::Error::Api(response) => match response.into_inner() {
            // `400` and `415` are the same answer to the caller: these bytes are not an
            // escrow, and sending them again will not help.
            rest::StoreEscrowError::Status400(problem)
            | rest::StoreEscrowError::Status415(problem) => RecoveryError::Malformed {
                code: Some(problem.code.clone()),
                detail: detail(&problem),
            },
            // Unreachable: every `426` is taken as `UpgradeRequired` above. Kept because the
            // generated enum is matched exhaustively.
            rest::StoreEscrowError::Status426(problem) => RecoveryError::Unexpected {
                status: 426,
                code: Some(problem.code.clone()),
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
            rest::StoreEscrowError::Status413(problem) => RecoveryError::Malformed {
                code: Some(problem.code.clone()),
                detail: "the escrow blob exceeds the server's request-body limit".to_owned(),
            },
        },
        other => wire_error(&other),
    }
}

/// Map a `GET /v1/auth/escrow/identity` refusal onto its typed variant: the escrow fetch's
/// table, except that the coded `404` is the sealed identity's own state.
fn fetch_sealed_identity_error(
    error: rest::Error<rest::FetchSealedIdentityError>,
) -> RecoveryError {
    if let Some(refusal) = upgrade_required_of(&error) {
        return RecoveryError::UpgradeRequired(Box::new(refusal));
    }
    match error {
        rest::Error::Api(response) => match response.into_inner() {
            rest::FetchSealedIdentityError::Status404(_) => RecoveryError::IdentityNotStored,
            rest::FetchSealedIdentityError::Status400(problem) => RecoveryError::Malformed {
                code: Some(problem.code.clone()),
                detail: detail(&problem),
            },
            rest::FetchSealedIdentityError::Status401(problem)
            | rest::FetchSealedIdentityError::Status403(problem) => refused(&problem),
            rest::FetchSealedIdentityError::Status500(problem) => unavailable(&problem),
            rest::FetchSealedIdentityError::Status413(problem) => RecoveryError::Unexpected {
                status: 413,
                code: Some(problem.code.clone()),
            },
        },
        other => wire_error(&other),
    }
}

/// Map a `PUT /v1/auth/escrow/identity` refusal onto its typed variant — the escrow store's
/// table, status for status.
fn store_sealed_identity_error(
    error: rest::Error<rest::StoreSealedIdentityError>,
) -> RecoveryError {
    if let Some(refusal) = upgrade_required_of(&error) {
        return RecoveryError::UpgradeRequired(Box::new(refusal));
    }
    match error {
        rest::Error::Api(response) => match response.into_inner() {
            rest::StoreSealedIdentityError::Status400(problem)
            | rest::StoreSealedIdentityError::Status415(problem) => RecoveryError::Malformed {
                code: Some(problem.code.clone()),
                detail: detail(&problem),
            },
            // Unreachable: every `426` is taken as `UpgradeRequired` above.
            rest::StoreSealedIdentityError::Status426(problem) => RecoveryError::Unexpected {
                status: 426,
                code: Some(problem.code.clone()),
            },
            rest::StoreSealedIdentityError::Status401(problem)
            | rest::StoreSealedIdentityError::Status403(problem) => refused(&problem),
            rest::StoreSealedIdentityError::Status500(problem) => unavailable(&problem),
            rest::StoreSealedIdentityError::Status413(problem) => RecoveryError::Malformed {
                code: Some(problem.code.clone()),
                detail: "the sealed identity exceeds the server's request-body limit".to_owned(),
            },
        },
        other => wire_error(&other),
    }
}

/// Read one entry of the album key listing: a UUID album id, a standard-base64 blob and an
/// RFC 3339 `stored_at`, or [`RecoveryError::InvalidResponse`] naming the one that is not.
fn read_escrowed_album_keys(
    entry: rest::types::EscrowedAlbumKeys,
) -> Result<EscrowedAlbumKeys, RecoveryError> {
    let album_id = Uuid::parse_str(&entry.album_id).map_err(|error| {
        RecoveryError::InvalidResponse(format!(
            "album id {:?} is not a UUID: {error}",
            entry.album_id
        ))
    })?;
    let blob = BASE64_STANDARD.decode(&entry.blob).map_err(|error| {
        RecoveryError::InvalidResponse(format!(
            "album {album_id}'s key escrow is not standard base64: {error}"
        ))
    })?;
    let stored_at = stored_instant(&entry.stored_at, "album key escrow")?;
    Ok(EscrowedAlbumKeys {
        album_id,
        blob,
        stored_at,
    })
}

/// Map a `PUT /v1/auth/escrow/albums/{album_id}` refusal onto its typed variant — the escrow
/// store's table, status for status.
fn store_album_keys_error(error: rest::Error<rest::StoreAlbumKeysError>) -> RecoveryError {
    if let Some(refusal) = upgrade_required_of(&error) {
        return RecoveryError::UpgradeRequired(Box::new(refusal));
    }
    match error {
        rest::Error::Api(response) => match response.into_inner() {
            rest::StoreAlbumKeysError::Status400(problem)
            | rest::StoreAlbumKeysError::Status415(problem) => RecoveryError::Malformed {
                code: Some(problem.code.clone()),
                detail: detail(&problem),
            },
            // Unreachable: every `426` is taken as `UpgradeRequired` above.
            rest::StoreAlbumKeysError::Status426(problem) => RecoveryError::Unexpected {
                status: 426,
                code: Some(problem.code.clone()),
            },
            rest::StoreAlbumKeysError::Status409(problem) => RecoveryError::AlbumKeysFull {
                code: Some(problem.code.clone()),
                detail: detail(&problem),
            },
            rest::StoreAlbumKeysError::Status401(problem)
            | rest::StoreAlbumKeysError::Status403(problem) => refused(&problem),
            rest::StoreAlbumKeysError::Status500(problem) => unavailable(&problem),
            rest::StoreAlbumKeysError::Status413(problem) => RecoveryError::Malformed {
                code: Some(problem.code.clone()),
                detail: "the album key escrow exceeds the server's request-body limit".to_owned(),
            },
        },
        other => wire_error(&other),
    }
}

/// Map a `GET /v1/auth/escrow/albums` refusal onto its typed variant: the escrow fetch's table,
/// with no `404` — an account with no album escrowed lists none.
fn list_album_keys_error(error: rest::Error<rest::ListAlbumKeysError>) -> RecoveryError {
    if let Some(refusal) = upgrade_required_of(&error) {
        return RecoveryError::UpgradeRequired(Box::new(refusal));
    }
    match error {
        rest::Error::Api(response) => match response.into_inner() {
            rest::ListAlbumKeysError::Status400(problem) => RecoveryError::Malformed {
                code: Some(problem.code.clone()),
                detail: detail(&problem),
            },
            rest::ListAlbumKeysError::Status401(problem)
            | rest::ListAlbumKeysError::Status403(problem) => refused(&problem),
            rest::ListAlbumKeysError::Status500(problem) => unavailable(&problem),
            rest::ListAlbumKeysError::Status413(problem) => RecoveryError::Unexpected {
                status: 413,
                code: Some(problem.code.clone()),
            },
        },
        other => wire_error(&other),
    }
}

/// Read a success answer's `stored_at`, which the server renders as RFC 3339.
///
/// The server *did* answer `200`, so the blob may well be stored; an unreadable instant is the
/// answer being unreadable, which is [`RecoveryError::InvalidResponse`] rather than a refusal.
fn stored_instant(stored_at: &str, what: &str) -> Result<Timestamp, RecoveryError> {
    stored_at.parse::<Timestamp>().map_err(|error| {
        tracing::warn!(
            %stored_at,
            %error,
            what,
            "stored, but the server's stored_at is not an RFC 3339 instant"
        );
        RecoveryError::InvalidResponse(format!(
            "stored_at {stored_at:?} is not an RFC 3339 instant: {error}"
        ))
    })
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
            code: None,
        },
        // Splitting `RequestConstruction` by class alone would report an unreachable server as
        // "sign in again", which on an offline device is the one remedy that cannot work; the
        // shared classifier reads the source instead.
        rest::Error::RequestConstruction(_) if is_token_failure(error) => {
            RecoveryError::Unauthorized {
                code: None,
                detail: describe_error(error),
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
        other => RecoveryError::Transport(describe_error(other)),
    }
}

#[cfg(test)]
mod tests;
