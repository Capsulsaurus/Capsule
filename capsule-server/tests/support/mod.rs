//! The doubles the auth suite drives the server with, and the fixture that assembles them.
//!
//! # Why the doubles live here and not in `src/`
//!
//! The in-memory [`AuthStateStore`] adapter lives in `src/`, because a shared conformance
//! suite has to be runnable against an adapter written in another crate. Nothing here has that
//! constraint, and one of these types is a **credential directory that accepts whatever password
//! it was told to accept**. That belongs in a test binary and nowhere a server could link it.
//!
//! The [`SessionTokens`] signer is *not* doubled: it is the real one over a generated key, so
//! every 401 in the suite is produced by a token that genuinely does not verify.
//!
//! # Why both collaborators can be broken on demand rather than replaced
//!
//! `assert_declared_responses_covered` walks the whole document against **one** client's
//! recording, so every response the description promises — the two 500s included — has to be
//! produced by one server. A second fixture with a permanently broken store could not
//! contribute to that walk. So the store and the directory each carry a switch, and the
//! coverage test breaks them for one request and repairs them.

#![allow(
    dead_code,
    reason = "each test binary uses a different part of the fixture"
)]

pub(crate) mod fault;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use base64::Engine as _;
use capsule_core::crypto::keys::HybridSigningKey;
use capsule_server::App;
use capsule_server::album::{
    AlbumContext, AlbumRecord, AlbumStore, InMemoryAlbums, ProvisionOutcome,
};
use capsule_server::app::Modules;
use capsule_server::attestation::{AttestationContext, InMemoryReceipts, LocalAttestationKey};
use capsule_server::auth::oidc::{
    AuthorizationRequest, DeviceAuthorization, DevicePoll, FederatedAccounts, FederatedLink,
    IdentityProvider, OidcCollaborators, OidcContext, ProviderError, ProviderFuture, Redemption,
    VerifiedIdentity,
};
use capsule_server::auth::{
    AccountDirectory, AccountProfiles, ActivateOutcome, AuthCollaborators, AuthContext,
    Authentication, BeginOutcome, ConsumeOutcome, DirectoryError, DirectoryFuture, EnrollmentState,
    PasswordChange, PasswordChanged, ProfileRecord, ProfileUpdate, SessionTokens, TotpCodes,
    TotpContext, TotpEnrollment, TotpSecret, TotpStore,
};
use capsule_server::blob::{
    BlobError, BlobFuture, BlobPage, BlobStat, BlobStore, ContentAddress, InMemoryBlobStore,
    Placement, QuarantineReason, QuarantinedBlob,
};
use capsule_server::counter::{CounterContext, InMemoryCounters};
use capsule_server::directory::{
    DeviceDirectoryContext, DeviceDirectoryStore, InMemoryDeviceDirectory, PublishOutcome,
    PublishedDirectory,
};
use capsule_server::discovery::revocation::{
    PublishedRevocations, RevocationList, RevokeFuture, RevokedToken,
};
use capsule_server::discovery::{DiscoveryContext, ProtocolWindow, ServerInfo};
use capsule_server::drop::{
    Admission, DropContext, DropStore, InMemoryDrops, InboxEntry, LinkProvisionOutcome,
    PendingDeposit, UploadLinkRecord,
};
use capsule_server::enrollment::EnrollmentContext;
use capsule_server::escrow::{
    AlbumKeyRecord, AlbumKeysStored, EscrowContext, EscrowRecord, EscrowStore, InMemoryEscrow,
    Replaced,
};
use capsule_server::federation::{
    CapabilityCodec, CapabilityFilter, CapabilityRecord, CapabilityStore, FederationCollaborators,
    FederationContext, FederationSetting, InMemoryCapabilities, InMemoryPeers, RefreshOutcome,
    ReportClaim, RevokeOutcome,
};
use capsule_server::gc::memory::InMemoryCollection;
use capsule_server::index::memory::InMemoryAssetIndex;
use capsule_server::index::{
    AssetIndex, AssetRow, BlobOutcome, BlobRecord, Custody, FeedEntry, HoldOutcome, IndexFuture,
    LifecycleOp, OpOutcome, PendingAsset, PurgeOutcome, Receipted, Reservation, ServingHold,
};
use capsule_server::ledger::{
    InMemoryLedger, LedgerEntry, LedgerError, LedgerEvent, LedgerFuture, LedgerHead, LedgerScan,
    OwnerLedger,
};
use capsule_server::membership::{
    InMemoryMembership, MemberRole, Membership, MembershipContext, MembershipStore, RosterOutcome,
    RosterRecord,
};
use capsule_server::moderation::{
    FederatedReport, InMemoryModeration, ModerationContext, ModerationEvent, ModerationStore,
    Standing,
};
use capsule_server::quota::{
    ChargeOutcome, InMemoryQuota, QuotaContext, QuotaLimits, QuotaStore, StoredUsage,
};
use capsule_server::serve::ServeContext;
use capsule_server::share::{InMemoryShares, IssueOutcome, ShareContext, ShareRecord, ShareStore};
use capsule_server::store::memory::{
    InMemoryAuthState, InMemoryChallenges, InMemoryChannels, InMemoryCohorts, InMemoryEnrollments,
    InMemoryOidcAuthorizations, InMemoryUploadSessions, ManualClock,
};
use capsule_server::store::{
    AcceptedChunk, AlbumId, AssetId, AuthStateStore, ChallengeStore, ChallengeToken, ChannelId,
    ChannelStore, Clock, CohortRecord, CohortStore, Direction, DrainOutcome, ENROLLMENT_CODE_TTL,
    EnrollmentCode, EnrollmentStore, FinalizeClaim, OidcAuthorizationStore, OidcState, OwnerId,
    PendingAuthorization, PendingEnrollment, RELAY_CHANNEL_TTL, RelayChannel, RelayOutcome,
    RelayPayload, RevokeAllChallenge, SessionId, SessionRecord, StoreError, StoreFuture, UploadId,
    UploadSessionRecord, UploadSessionStatus, UploadSessionStore, UserId,
};

pub(crate) mod idp;
use capsule_server::sync::{CURSOR_KEY_LEN, CursorCodec, SyncContext};
use capsule_server::upload::authority::{
    AlbumWriteAccess, AuthorityError, AuthorityFuture, WriteAuthority,
};
use capsule_server::upload::{ChunkReadDeadline, UploadContext, UploadPolicy, WriteRole};
use capsule_server::verify::VerifyContext;
use jiff::{SignedDuration, Timestamp};
use kynos::test::{TestClient, TestRequest};
use uuid::Uuid;

/// The session lifetime the fixture's store is built with.
///
/// Seven days, matching the refresh-token lifetime the Salvo deployment configured, so the suite
/// exercises a realistic "the refresh token dies with its record" window rather than a shorter
/// one that would hide the arrangement.
pub(crate) const SESSION_TTL: SignedDuration = SignedDuration::from_hours(24 * 7);

/// The asset the conformance suite's lifecycle ops act on.
pub(crate) const OPS_ASSET: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61";

/// A well-formed `POST /albums/{id}/ops` bundle for `action`, chaining onto `prior`.
///
/// The manifest bytes are deliberately not valid CBOR: this surface stores them verbatim and
/// never parses them, so feeding it something parseable would test less than the contract says.
pub(crate) fn op_bundle(
    clock: &ManualClock,
    action: &str,
    seed: &str,
    prior: Option<&str>,
) -> serde_json::Value {
    use base64::Engine as _;

    let manifest = format!("signed-lifecycle-manifest-{seed}").into_bytes();
    serde_json::json!({
        "manifest_envelope": {
            "crypto_suite_id": capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
            "protocol_version": PROTOCOL_VERSION,
            "album_id": album().as_str(),
            "file_id": OPS_ASSET,
            "amk_version": 1,
            "ciphertext_hash": checksum(b"the asset's original ciphertext"),
            "plaintext_size": 4096,
            "chunk_size": 65_536,
            "key_mode": "derived",
            "metadata_blob_hash": serde_json::Value::Null,
            "created_by_user": user().as_str(),
            "created_by_device": device().to_string(),
            "client_version": "capsule-cli/0.1.0",
            "timestamp": clock.now().to_string(),
            "action": action,
            "prior_provenance_hash": prior,
            "retention_until": serde_json::Value::Null,
        },
        "manifest_cbor": base64::engine::general_purpose::STANDARD.encode(&manifest),
        "metadata_blob": serde_json::Value::Null,
    })
}

/// An account's identity key, generated once per test and reused across its publishes.
///
/// A publish carries the key its document verifies under, and the server anchors the account to
/// the first one it sees. A helper that generated a fresh key per call would make
/// every second publish an `IdentityMismatch`, which is why the key is the caller's to hold.
pub(crate) fn identity_key() -> HybridSigningKey {
    HybridSigningKey::generate()
}

/// A device directory for the seeded account at `version`, signed by `ik`.
pub(crate) fn signed_directory_by(ik: &HybridSigningKey, version: u64) -> Vec<u8> {
    use capsule_core::crypto::keys::{DeviceDirectory, DirectoryCore};

    let directory: DeviceDirectory = DirectoryCore {
        user_id: Uuid::parse_str(user().as_str()).expect("the seeded account id is a uuid"),
        directory_version: version,
        updated_at: "2026-01-01T00:00:00Z".to_owned(),
        devices: Vec::new(),
        unknown: Default::default(),
    }
    .sign(ik);
    capsule_core::cbor::to_canonical_vec(&directory).expect("a directory serializes")
}

/// The base64 revoke-all proof `ik` makes over `challenge`.
///
/// Built through `capsule_core::crypto::revoke`, the same path a client takes: a test that
/// assembled the signed bytes itself would pass while the two ends disagreed.
pub(crate) fn revoke_proof(ik: &HybridSigningKey, challenge: &str) -> String {
    let signature = ik.sign(&capsule_core::crypto::revoke::revoke_all_signing_bytes(
        challenge,
    ));
    base64::engine::general_purpose::STANDARD
        .encode(capsule_core::cbor::to_canonical_vec(&signature).expect("a signature encodes"))
}

/// The `X-Capsule-Identity-Key` header value for `ik`.
pub(crate) fn identity_header(ik: &HybridSigningKey) -> String {
    base64::engine::general_purpose::STANDARD.encode(ik.verifying_key().to_bytes())
}

/// The origin the fixture's server calls itself.
///
/// One constant for the attestation key's `server_id` and the discovery record's, because a
/// receipt binds to the origin that signed it and a test asserting the two agree has to be
/// asserting about one fact rather than two matching literals.
pub(crate) const SERVER_ORIGIN: &str = "capsule.test";

/// The account [`Fixture::working`] seeds.
pub(crate) const EMAIL: &str = "somebody@example.test";

/// The password that account authenticates with.
pub(crate) const PASSWORD: &str = "correct horse battery staple";

/// What a broken collaborator says. Asserted against, so a 500 body that leaked it would fail.
const REFUSAL: &str = "the double refuses on purpose";

/// The sync-cursor MAC key the fixture builds every server with.
///
/// A constant, so a test can build a second codec over the same key and mint a cursor the
/// server accepts — and a *different* key is how the "cursor from another server" case is
/// written without reaching inside the codec.
pub(crate) const CURSOR_KEY: [u8; CURSOR_KEY_LEN] = [0x5C; CURSOR_KEY_LEN];

// ===========================================================================================
// Identity provider double
// ===========================================================================================

/// The authorization code the double redeems; every other code is `invalid_grant`.
pub(crate) const GOOD_CODE: &str = "good-code";

/// The binding secret the suite's OIDC client holds (issue #517): presented as the callback's
/// `binding_verifier`, and hashed into the authorize's `binding_challenge`.
pub(crate) const OIDC_BINDING: &str = "suite-client-binding-secret-0000000000000000";

/// The S256 hash of [`OIDC_BINDING`], the `binding_challenge` every suite authorize carries.
pub(crate) fn oidc_binding_challenge() -> String {
    capsule_server::auth::oidc::code_challenge(&capsule_server::store::PkceVerifier::new(
        OIDC_BINDING,
    ))
}

/// An authorize body for `redirect_uri`, bound to the suite's client.
pub(crate) fn oidc_authorize_body(redirect_uri: &str) -> serde_json::Value {
    serde_json::json!({
        "redirect_uri": redirect_uri,
        "binding_challenge": oidc_binding_challenge(),
    })
}

/// A callback body for `state` and `code`, presenting the suite client's binding.
pub(crate) fn oidc_callback_body(state: &serde_json::Value, code: &str) -> serde_json::Value {
    serde_json::json!({ "state": state, "code": code, "binding_verifier": OIDC_BINDING })
}

/// An identity provider that answers whatever the test told it to.
///
/// The routes are tested against this; the real [`HttpIdentityProvider`] is tested against the
/// in-process mock provider in [`idp`], which speaks the real wire. Every refusal the port can
/// make is a switch here, so the conformance walk can produce each declared response.
#[derive(Debug)]
pub(crate) struct SwitchableIdentityProvider {
    configured: AtomicBool,
    unavailable: AtomicBool,
    identity: Mutex<VerifiedIdentity>,
    token_rejected: AtomicBool,
    /// Every authorization request the server built, for a case that reads the ceremony back.
    requests: Mutex<Vec<RecordedAuthorization>>,
    /// How many codes the server asked this double to redeem, whatever it answered.
    redemptions: AtomicUsize,
    /// Whether the provider offers the device grant (issue #635).
    device_offered: AtomicBool,
    /// What every device poll answers until the test says otherwise.
    device_answer: Mutex<DeviceAnswer>,
    /// The `expires_in` every device authorization is issued with.
    device_expires_in: Mutex<jiff::SignedDuration>,
    /// Every device authorization issued, numbered.
    device_begins: AtomicUsize,
    /// Every device code the server polled with, in order.
    device_polls: Mutex<Vec<String>>,
}

/// What the double answers a device poll with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceAnswer {
    /// `authorization_pending`.
    Pending,
    /// `slow_down`.
    SlowDown,
    /// `access_denied`.
    Denied,
    /// `expired_token`.
    Expired,
    /// Approved: the double's identity.
    Approved,
    /// A refusal RFC 8628 does not name.
    ExchangeRefused,
    /// A token the relying party refuses.
    TokenRejected,
}

/// The user code every device authorization the double issues carries.
pub(crate) const DEVICE_USER_CODE: &str = "WDJB-MJHT";

/// A device-begin body, bound to the suite's client.
pub(crate) fn oidc_device_body() -> serde_json::Value {
    serde_json::json!({ "binding_challenge": oidc_binding_challenge() })
}

/// A device-poll body for `handle`, presenting the suite client's binding.
pub(crate) fn oidc_device_poll_body(handle: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "handle": handle, "binding_verifier": OIDC_BINDING })
}

/// One authorization request as the double saw it.
#[derive(Debug, Clone)]
pub(crate) struct RecordedAuthorization {
    pub(crate) redirect_uri: String,
    pub(crate) state: String,
    pub(crate) nonce: String,
    pub(crate) code_challenge: String,
}

impl SwitchableIdentityProvider {
    pub(crate) fn new() -> Self {
        Self {
            configured: AtomicBool::new(true),
            unavailable: AtomicBool::new(false),
            identity: Mutex::new(VerifiedIdentity {
                issuer: "https://idp.test".to_owned(),
                subject: "subject-1".to_owned(),
                email: Some("federated@example.test".to_owned()),
                email_verified: true,
            }),
            token_rejected: AtomicBool::new(false),
            requests: Mutex::new(Vec::new()),
            redemptions: AtomicUsize::new(0),
            device_offered: AtomicBool::new(true),
            device_answer: Mutex::new(DeviceAnswer::Pending),
            device_expires_in: Mutex::new(jiff::SignedDuration::from_mins(10)),
            device_begins: AtomicUsize::new(0),
            device_polls: Mutex::new(Vec::new()),
        }
    }

    /// Whether the provider offers the device grant.
    pub(crate) fn set_device_offered(&self, offered: bool) {
        self.device_offered.store(offered, Ordering::SeqCst);
    }

    /// What every device poll answers from now on.
    pub(crate) fn set_device_answer(&self, answer: DeviceAnswer) {
        *self
            .device_answer
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = answer;
    }

    /// The `expires_in` every device authorization is issued with from now on.
    pub(crate) fn set_device_expires_in(&self, expires_in: jiff::SignedDuration) {
        *self
            .device_expires_in
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = expires_in;
    }

    /// Every device code the server has polled with so far.
    pub(crate) fn device_polls(&self) -> Vec<String> {
        self.device_polls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// How many codes the server has asked to redeem so far.
    pub(crate) fn redemptions(&self) -> usize {
        self.redemptions.load(Ordering::SeqCst)
    }

    /// Whether the deployment has a provider at all.
    pub(crate) fn set_configured(&self, configured: bool) {
        self.configured.store(configured, Ordering::SeqCst);
    }

    /// Make every operation fail as an unreachable provider, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// Make the next redemptions fail as a refused ID token, or stop.
    pub(crate) fn set_token_rejected(&self, rejected: bool) {
        self.token_rejected.store(rejected, Ordering::SeqCst);
    }

    /// The identity every successful redemption yields.
    pub(crate) fn set_identity(&self, identity: VerifiedIdentity) {
        *self.identity.lock().unwrap_or_else(PoisonError::into_inner) = identity;
    }

    /// The authorization requests the server has built so far.
    pub(crate) fn requests(&self) -> Vec<RecordedAuthorization> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl SwitchableIdentityProvider {
    /// The real policy, so the double's redirect decision is the adapter's decision.
    ///
    /// One function for both `admits_redirect` and `authorization_url`: a double that answered
    /// the look-ahead and the enforcement differently would let the route's counter-key choice
    /// pass a test the shipped adapter fails.
    fn policy() -> capsule_server::auth::oidc::RedirectPolicy {
        capsule_server::auth::oidc::RedirectPolicy::new(
            Some("https://app.test/oidc/callback".to_owned()),
            true,
        )
    }
}

impl IdentityProvider for SwitchableIdentityProvider {
    fn admits_redirect(&self, redirect_uri: &str) -> bool {
        self.configured.load(Ordering::SeqCst) && Self::policy().admits(redirect_uri)
    }

    fn authorization_url<'a>(
        &'a self,
        request: &'a AuthorizationRequest<'a>,
    ) -> ProviderFuture<'a, String> {
        Box::pin(async move {
            if !self.configured.load(Ordering::SeqCst) {
                return Err(ProviderError::NotConfigured);
            }
            if !Self::policy().admits(request.redirect_uri) {
                return Err(ProviderError::RedirectRefused {
                    redirect_uri: request.redirect_uri.to_owned(),
                });
            }
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(ProviderError::Unavailable {
                    detail: REFUSAL.to_owned(),
                });
            }
            self.requests
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(RecordedAuthorization {
                    redirect_uri: request.redirect_uri.to_owned(),
                    state: request.state.as_str().to_owned(),
                    nonce: request.nonce.as_str().to_owned(),
                    code_challenge: request.code_challenge.to_owned(),
                });
            Ok(format!(
                "https://idp.test/authorize?state={}&nonce={}&code_challenge={}",
                request.state.as_str(),
                request.nonce.as_str(),
                request.code_challenge
            ))
        })
    }

    fn redeem<'a>(
        &'a self,
        redemption: &'a Redemption<'a>,
    ) -> ProviderFuture<'a, VerifiedIdentity> {
        Box::pin(async move {
            self.redemptions.fetch_add(1, Ordering::SeqCst);
            if !self.configured.load(Ordering::SeqCst) {
                return Err(ProviderError::NotConfigured);
            }
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(ProviderError::Unavailable {
                    detail: REFUSAL.to_owned(),
                });
            }
            if redemption.code.as_str() != GOOD_CODE {
                return Err(ProviderError::ExchangeRefused {
                    detail: "invalid_grant".to_owned(),
                });
            }
            if self.token_rejected.load(Ordering::SeqCst) {
                return Err(ProviderError::TokenRejected(
                    capsule_server::auth::oidc::ClaimRejection::Nonce,
                ));
            }
            Ok(self
                .identity
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone())
        })
    }

    fn begin_device(&self) -> ProviderFuture<'_, DeviceAuthorization> {
        Box::pin(async move {
            if !self.configured.load(Ordering::SeqCst) {
                return Err(ProviderError::NotConfigured);
            }
            if !self.device_offered.load(Ordering::SeqCst) {
                return Err(ProviderError::DeviceUnsupported {
                    detail: REFUSAL.to_owned(),
                });
            }
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(ProviderError::Unavailable {
                    detail: REFUSAL.to_owned(),
                });
            }
            let n = self.device_begins.fetch_add(1, Ordering::SeqCst);
            Ok(DeviceAuthorization {
                device_code: capsule_server::store::DeviceCode::new(format!("device-code-{n}")),
                user_code: DEVICE_USER_CODE.to_owned(),
                verification_uri: "https://idp.test/device".to_owned(),
                verification_uri_complete: Some(format!(
                    "https://idp.test/device?user_code={DEVICE_USER_CODE}"
                )),
                expires_in: *self
                    .device_expires_in
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner),
                interval: capsule_server::store::DEVICE_POLL_FLOOR,
            })
        })
    }

    fn poll_device<'a>(
        &'a self,
        device_code: &'a capsule_server::store::DeviceCode,
    ) -> ProviderFuture<'a, DevicePoll> {
        Box::pin(async move {
            self.device_polls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(device_code.as_str().to_owned());
            if !self.configured.load(Ordering::SeqCst) {
                return Err(ProviderError::NotConfigured);
            }
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(ProviderError::Unavailable {
                    detail: REFUSAL.to_owned(),
                });
            }
            let answer = *self
                .device_answer
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match answer {
                DeviceAnswer::Pending => Ok(DevicePoll::Pending),
                DeviceAnswer::SlowDown => Ok(DevicePoll::SlowDown),
                DeviceAnswer::Denied => Ok(DevicePoll::Denied),
                DeviceAnswer::Expired => Ok(DevicePoll::Expired),
                DeviceAnswer::Approved => Ok(DevicePoll::Verified(
                    self.identity
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone(),
                )),
                DeviceAnswer::ExchangeRefused => Err(ProviderError::ExchangeRefused {
                    detail: "invalid_grant".to_owned(),
                }),
                DeviceAnswer::TokenRejected => Err(ProviderError::TokenRejected(
                    capsule_server::auth::oidc::ClaimRejection::Nonce,
                )),
            }
        })
    }
}

/// The OIDC ceremony store, with two switches: unreachable, and full.
#[derive(Debug)]
pub(crate) struct SwitchableOidcAuthorizations {
    inner: InMemoryOidcAuthorizations,
    unavailable: AtomicBool,
    full: AtomicBool,
}

impl SwitchableOidcAuthorizations {
    pub(crate) fn new(clock: Arc<ManualClock>) -> Self {
        Self {
            inner: InMemoryOidcAuthorizations::with_default_ttl(clock),
            unavailable: AtomicBool::new(false),
            full: AtomicBool::new(false),
        }
    }

    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// Make every `begin` answer as a store at its ceiling, or stop.
    pub(crate) fn set_full(&self, full: bool) {
        self.full.store(full, Ordering::SeqCst);
    }

    fn refuse(&self) -> Result<(), StoreError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(StoreError::Unavailable {
                store: "oidc authorizations",
                detail: REFUSAL.to_owned(),
            });
        }
        Ok(())
    }
}

impl OidcAuthorizationStore for SwitchableOidcAuthorizations {
    fn ttl(&self) -> SignedDuration {
        self.inner.ttl()
    }

    fn begin<'a>(
        &'a self,
        state: &'a OidcState,
        record: PendingAuthorization,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            self.refuse()?;
            if self.full.load(Ordering::SeqCst) {
                return Err(StoreError::Rejected {
                    store: "oidc authorizations",
                    detail: REFUSAL.to_owned(),
                });
            }
            self.inner.begin(state, record).await
        })
    }

    fn consume<'a>(
        &'a self,
        state: &'a OidcState,
    ) -> StoreFuture<'a, Option<PendingAuthorization>> {
        Box::pin(async move {
            self.refuse()?;
            self.inner.consume(state).await
        })
    }
}

/// The OIDC device store (issue #635), with the same two switches: unreachable, and full.
#[derive(Debug)]
pub(crate) struct SwitchableOidcDevices {
    inner: capsule_server::store::memory::InMemoryOidcDevices,
    unavailable: AtomicBool,
    full: AtomicBool,
}

impl SwitchableOidcDevices {
    pub(crate) fn new(clock: Arc<ManualClock>) -> Self {
        Self {
            inner: capsule_server::store::memory::InMemoryOidcDevices::with_default_ttl(clock),
            unavailable: AtomicBool::new(false),
            full: AtomicBool::new(false),
        }
    }

    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// Make every `begin` answer as a store at its ceiling, or stop.
    pub(crate) fn set_full(&self, full: bool) {
        self.full.store(full, Ordering::SeqCst);
    }

    fn refuse(&self) -> Result<(), StoreError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(StoreError::Unavailable {
                store: "oidc devices",
                detail: REFUSAL.to_owned(),
            });
        }
        Ok(())
    }
}

impl capsule_server::store::OidcDeviceStore for SwitchableOidcDevices {
    fn ttl(&self) -> SignedDuration {
        self.inner.ttl()
    }

    fn begin<'a>(
        &'a self,
        handle: &'a capsule_server::store::OidcDeviceHandle,
        record: capsule_server::store::PendingDeviceAuthorization,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            self.refuse()?;
            if self.full.load(Ordering::SeqCst) {
                return Err(StoreError::Rejected {
                    store: "oidc devices",
                    detail: REFUSAL.to_owned(),
                });
            }
            self.inner.begin(handle, record).await
        })
    }

    fn claim<'a>(
        &'a self,
        handle: &'a capsule_server::store::OidcDeviceHandle,
    ) -> StoreFuture<'a, capsule_server::store::DevicePollSlot> {
        Box::pin(async move {
            self.refuse()?;
            self.inner.claim(handle).await
        })
    }

    fn slow_down<'a>(
        &'a self,
        handle: &'a capsule_server::store::OidcDeviceHandle,
    ) -> StoreFuture<'a, Option<SignedDuration>> {
        Box::pin(async move {
            self.refuse()?;
            self.inner.slow_down(handle).await
        })
    }

    fn finish<'a>(
        &'a self,
        handle: &'a capsule_server::store::OidcDeviceHandle,
    ) -> StoreFuture<'a, Option<capsule_server::store::PendingDeviceAuthorization>> {
        Box::pin(async move {
            self.refuse()?;
            self.inner.finish(handle).await
        })
    }
}

// ===========================================================================================
// Account directory double
// ===========================================================================================

/// An account directory holding whatever the test told it.
///
/// Passwords are compared verbatim: hashing them would be a test of `argon2`, which is the real
/// adapter's business rather than this port's contract. What *is* the contract — three outcomes,
/// and a credential that never rises above the port — is exercised exactly as it would be
/// against Postgres.
#[derive(Debug, Default)]
pub(crate) struct InMemoryAccounts {
    accounts: Mutex<BTreeMap<String, Account>>,
    /// `(issuer, subject)` → the account a federated sign-in resolved to.
    federated: Mutex<BTreeMap<(String, String), UserId>>,
    unavailable: AtomicBool,
    forget_after_authentication: AtomicBool,
}

#[derive(Debug, Clone)]
struct Account {
    user_id: UserId,
    password: String,
    locked: bool,
    display_name: Option<String>,
    created_at: Timestamp,
}

impl InMemoryAccounts {
    /// An empty directory.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record an account that will authenticate with `password`.
    pub(crate) fn insert(&self, email: &str, password: &str, user_id: &UserId) {
        self.accounts().insert(
            email.to_owned(),
            Account {
                user_id: user_id.clone(),
                password: password.to_owned(),
                locked: false,
                display_name: None,
                created_at: Timestamp::UNIX_EPOCH,
            },
        );
    }

    /// Arm the one interleaving a password change cannot rule out: the account is deleted
    /// *between* the verification that authorized the change and the write that applies it.
    ///
    /// Staged rather than raced, because a race would be a flaky test of a real hazard. The next
    /// successful `authenticate_user` removes the account it just authenticated, which is
    /// precisely the window the route's `NoSuchAccount` arm exists for.
    pub(crate) fn forget_after_next_authentication(&self) {
        self.forget_after_authentication
            .store(true, Ordering::SeqCst);
    }

    /// Forget an account entirely, leaving any session it opened live.
    ///
    /// The one way a suite reaches the `404` on the profile surface: a valid credential naming
    /// an account the directory no longer holds is not a hypothetical, it is what a deletion
    /// during a live session leaves behind.
    pub(crate) fn forget(&self, email: &str) {
        self.accounts().remove(email);
    }

    /// The password an account currently authenticates with, for a case that has to prove a
    /// change reached the store rather than only the response.
    pub(crate) fn password_of(&self, email: &str) -> Option<String> {
        self.accounts().get(email).map(|held| held.password.clone())
    }

    /// Put an existing account into the locked-out state.
    pub(crate) fn lock(&self, email: &str) {
        if let Some(account) = self.accounts().get_mut(email) {
            account.locked = true;
        }
    }

    /// Take it back out, so a case can produce a `423` without poisoning the rest of a walk.
    pub(crate) fn unlock(&self, email: &str) {
        if let Some(account) = self.accounts().get_mut(email) {
            account.locked = false;
        }
    }

    /// Make every subsequent lookup fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn accounts(&self) -> MutexGuard<'_, BTreeMap<String, Account>> {
        self.accounts.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl FederatedAccounts for InMemoryAccounts {
    /// Shares the password directory's rows, as both shipped adapters do since #460: an asserted
    /// address a password account holds is `AddressTaken`, and a created federated account is one
    /// nothing can sign into with a password, because no password row exists for it. Only a
    /// **verified** address is compared, as the shipped adapters do.
    ///
    /// The shipped adapters prove the contract against the shared suite in
    /// `capsule_server::auth::conformance`; this double only has to answer the routes alike.
    fn resolve_or_create<'a>(
        &'a self,
        identity: &'a VerifiedIdentity,
        user: &'a UserId,
        _at: Timestamp,
    ) -> DirectoryFuture<'a, FederatedLink> {
        Box::pin(async move {
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(DirectoryError::Unavailable {
                    detail: REFUSAL.to_owned(),
                });
            }
            let mut federated = self
                .federated
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let key = (identity.issuer.clone(), identity.subject.clone());
            if let Some(existing) = federated.get(&key) {
                return Ok(FederatedLink::Linked(existing.clone()));
            }
            if let Some(email) = &identity.email
                && identity.email_verified
                && self.accounts().contains_key(email)
            {
                return Ok(FederatedLink::AddressTaken);
            }
            federated.insert(key, user.clone());
            Ok(FederatedLink::Created(user.clone()))
        })
    }
}

impl AccountDirectory for InMemoryAccounts {
    fn authenticate<'a>(
        &'a self,
        email: &'a str,
        password: &'a str,
    ) -> DirectoryFuture<'a, Authentication> {
        Box::pin(async move {
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(DirectoryError::Unavailable {
                    detail: REFUSAL.to_owned(),
                });
            }

            let accounts = self.accounts();
            let Some(account) = accounts.get(email) else {
                return Ok(Authentication::Refused);
            };
            if account.locked {
                return Ok(Authentication::Locked);
            }
            if account.password == password {
                Ok(Authentication::Granted(account.user_id.clone()))
            } else {
                Ok(Authentication::Refused)
            }
        })
    }

    fn authenticate_user<'a>(
        &'a self,
        user: &'a UserId,
        password: &'a str,
    ) -> DirectoryFuture<'a, Authentication> {
        Box::pin(async move {
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(DirectoryError::Unavailable {
                    detail: REFUSAL.to_owned(),
                });
            }

            let mut accounts = self.accounts();
            let Some(account) = accounts
                .values()
                .find(|held| &held.user_id == user)
                .cloned()
            else {
                return Ok(Authentication::Refused);
            };
            if account.locked {
                return Ok(Authentication::Locked);
            }
            if account.password != password {
                return Ok(Authentication::Refused);
            }
            if self
                .forget_after_authentication
                .swap(false, Ordering::SeqCst)
            {
                accounts.retain(|_, held| held.user_id != account.user_id);
            }
            Ok(Authentication::Granted(account.user_id))
        })
    }
}

impl capsule_server::auth::AccountRegistry for InMemoryAccounts {
    fn create<'a>(
        &'a self,
        email: &'a str,
        password: &'a str,
        user: &'a UserId,
        at: Timestamp,
    ) -> capsule_server::auth::DirectoryFuture<'a, capsule_server::auth::Registration> {
        Box::pin(async move {
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(capsule_server::auth::DirectoryError::Unavailable {
                    detail: REFUSAL.to_owned(),
                });
            }
            // One critical section, as the port requires: a check and a write a caller could
            // interleave is two registrations that both believe they own the address.
            let mut accounts = self.accounts();
            if accounts.contains_key(email) {
                return Ok(capsule_server::auth::Registration::AlreadyExists);
            }
            accounts.insert(
                email.to_owned(),
                Account {
                    user_id: user.clone(),
                    password: password.to_owned(),
                    locked: false,
                    display_name: None,
                    created_at: at,
                },
            );
            Ok(capsule_server::auth::Registration::Created(user.clone()))
        })
    }
}

impl AccountProfiles for InMemoryAccounts {
    fn read<'a>(&'a self, user: &'a UserId) -> DirectoryFuture<'a, Option<ProfileRecord>> {
        Box::pin(async move {
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(DirectoryError::Unavailable {
                    detail: REFUSAL.to_owned(),
                });
            }
            Ok(self
                .accounts()
                .iter()
                .find(|(_, held)| &held.user_id == user)
                .map(|(email, held)| profile_of(email, held)))
        })
    }

    fn update<'a>(
        &'a self,
        user: &'a UserId,
        update: &'a ProfileUpdate,
    ) -> DirectoryFuture<'a, Option<ProfileRecord>> {
        Box::pin(async move {
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(DirectoryError::Unavailable {
                    detail: REFUSAL.to_owned(),
                });
            }
            // One critical section, as the port requires: a read-modify-write a caller could
            // interleave is an edit from another device silently clobbered.
            let mut accounts = self.accounts();
            let Some((email, held)) = accounts
                .iter_mut()
                .find(|(_, held)| &held.user_id == user)
                .map(|(email, held)| (email.clone(), held))
            else {
                return Ok(None);
            };
            if let Some(display_name) = update.display_name.clone() {
                held.display_name = display_name;
            }
            Ok(Some(profile_of(&email, held)))
        })
    }
}

impl PasswordChange for InMemoryAccounts {
    fn set_password<'a>(
        &'a self,
        user: &'a UserId,
        password: &'a str,
        _at: Timestamp,
    ) -> DirectoryFuture<'a, PasswordChanged> {
        Box::pin(async move {
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(DirectoryError::Unavailable {
                    detail: REFUSAL.to_owned(),
                });
            }
            let mut accounts = self.accounts();
            let Some(held) = accounts.values_mut().find(|held| &held.user_id == user) else {
                return Ok(PasswordChanged::NoSuchAccount);
            };
            password.clone_into(&mut held.password);
            // The port requires it: a change is a successful credential presentation, and
            // leaving the lockout behind would bar somebody from an account they just proved
            // they own.
            held.locked = false;
            Ok(PasswordChanged::Yes)
        })
    }
}

/// The profile view of a held account.
fn profile_of(email: &str, held: &Account) -> ProfileRecord {
    ProfileRecord {
        user_id: held.user_id.clone(),
        email: Some(email.to_owned()),
        display_name: held.display_name.clone(),
        created_at: held.created_at,
    }
}

// ===========================================================================================
// Second-factor double
// ===========================================================================================

/// The second-factor enrollments, with the switch every other double here carries.
///
/// A real implementation of the port's contract rather than a stub: the check-and-write in
/// `begin`, the pending-only `activate` and the compare-and-set in `consume` are the three
/// properties the port exists to promise, and a double that fudged them would let the routes
/// pass while a Postgres adapter written to the same contract failed.
#[derive(Debug, Default)]
pub(crate) struct InMemoryTotp {
    held: Mutex<BTreeMap<UserId, TotpEnrollment>>,
    unavailable: AtomicBool,
}

impl InMemoryTotp {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// The secret held for `user`, so a case can compute the code an app would show.
    pub(crate) fn secret_of(&self, user: &UserId) -> Option<TotpSecret> {
        self.enrollments().get(user).map(|held| held.secret.clone())
    }

    /// Whether `user` has a confirmed second factor.
    pub(crate) fn is_active(&self, user: &UserId) -> bool {
        self.enrollments()
            .get(user)
            .is_some_and(|held| held.state == EnrollmentState::Active)
    }

    fn enrollments(&self) -> MutexGuard<'_, BTreeMap<UserId, TotpEnrollment>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn refuse<T>(&self) -> Option<Result<T, DirectoryError>> {
        self.unavailable.load(Ordering::SeqCst).then(|| {
            Err(DirectoryError::Unavailable {
                detail: REFUSAL.to_owned(),
            })
        })
    }
}

impl TotpStore for InMemoryTotp {
    fn begin(&self, record: TotpEnrollment) -> DirectoryFuture<'_, BeginOutcome> {
        Box::pin(async move {
            if let Some(refusal) = self.refuse() {
                return refusal;
            }
            // One critical section, as the port requires: a caller that read, saw no active
            // enrollment and then wrote has a window in which a confirmation lands.
            let mut held = self.enrollments();
            if held
                .get(&record.user_id)
                .is_some_and(|existing| existing.state == EnrollmentState::Active)
            {
                return Ok(BeginOutcome::AlreadyActive);
            }
            held.insert(record.user_id.clone(), record);
            Ok(BeginOutcome::Started)
        })
    }

    fn read<'a>(&'a self, user: &'a UserId) -> DirectoryFuture<'a, Option<TotpEnrollment>> {
        Box::pin(async move {
            if let Some(refusal) = self.refuse() {
                return refusal;
            }
            Ok(self.enrollments().get(user).cloned())
        })
    }

    fn activate<'a>(
        &'a self,
        user: &'a UserId,
        step: u64,
        at: Timestamp,
    ) -> DirectoryFuture<'a, ActivateOutcome> {
        Box::pin(async move {
            if let Some(refusal) = self.refuse() {
                return refusal;
            }
            let mut held = self.enrollments();
            let Some(record) = held.get_mut(user) else {
                return Ok(ActivateOutcome::NotPending);
            };
            if record.state != EnrollmentState::Pending {
                return Ok(ActivateOutcome::NotPending);
            }
            record.state = EnrollmentState::Active;
            record.activated_at = Some(at);
            // The confirming code is spent, so it cannot also complete a sign-in.
            record.last_step = Some(step);
            Ok(ActivateOutcome::Activated)
        })
    }

    fn consume<'a>(&'a self, user: &'a UserId, step: u64) -> DirectoryFuture<'a, ConsumeOutcome> {
        Box::pin(async move {
            if let Some(refusal) = self.refuse() {
                return refusal;
            }
            let mut held = self.enrollments();
            let Some(record) = held.get_mut(user) else {
                return Ok(ConsumeOutcome::NotEnrolled);
            };
            if record.last_step.is_some_and(|last| step <= last) {
                return Ok(ConsumeOutcome::Replayed);
            }
            record.last_step = Some(step);
            Ok(ConsumeOutcome::Fresh)
        })
    }

    fn disable<'a>(&'a self, user: &'a UserId) -> DirectoryFuture<'a, bool> {
        Box::pin(async move {
            if let Some(refusal) = self.refuse() {
                return refusal;
            }
            Ok(self.enrollments().remove(user).is_some())
        })
    }
}

// ===========================================================================================
// Session store double
// ===========================================================================================

/// The in-memory session store, with a switch that makes it unreachable.
///
/// Delegation rather than reimplementation: when the switch is off this *is* the adapter the
/// shared conformance suite passes, so a test asserting session state is asserting against the
/// real thing. `ttl()` answers either way — a store that could not report its own configured
/// lifetime would be a different failure from a store that cannot be reached.
#[derive(Debug)]
pub(crate) struct SwitchableSessions {
    inner: InMemoryAuthState,
    unavailable: AtomicBool,
    unavailable_after_authentication: AtomicBool,
}

impl SwitchableSessions {
    /// A working store on `clock`, with [`SESSION_TTL`].
    pub(crate) fn new(clock: Arc<ManualClock>) -> Self {
        Self {
            inner: InMemoryAuthState::new(clock, SESSION_TTL),
            unavailable: AtomicBool::new(false),
            unavailable_after_authentication: AtomicBool::new(false),
        }
    }

    /// Make every subsequent operation fail, or stop.
    ///
    /// This includes `read_session`, which the bearer scheme calls, so a guarded
    /// operation under this switch is refused **at the door** and its handler is never entered.
    /// That is the truthful simulation of a total store outage; it is not the way to reach a
    /// handler's own store-unavailable answer — see
    /// [`Self::set_unavailable_after_authentication`].
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// Make every operation *except* `read_session` fail.
    ///
    /// This is the partial outage a handler's `500` is written for and the only shape that can
    /// still reach one: the bearer scheme's read succeeds, the request enters the handler, and
    /// the write the handler makes is the thing that fails. It is not a contrivance — a read
    /// served from a replica while the primary is unwritable is exactly this — and without it
    /// the ledger check on the bearer scheme would make several declared `500`s unreachable,
    /// which is the declared-but-unreachable-status defect wearing a new hat.
    pub(crate) fn set_unavailable_after_authentication(&self, unavailable: bool) {
        self.unavailable_after_authentication
            .store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "auth-state",
            detail: REFUSAL.to_owned(),
        })
    }

    /// Whether the store answers at all — the question `read_session` asks.
    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }

    /// Whether an operation a handler makes answers — every operation but `read_session`.
    fn is_down_past_the_door(&self) -> bool {
        self.is_down() || self.unavailable_after_authentication.load(Ordering::SeqCst)
    }
}

/// A drop store that can be made to fail on demand.
#[derive(Debug, Default)]
pub(crate) struct SwitchableDrops {
    inner: InMemoryDrops,
    unavailable: AtomicBool,
    file_fault: Mutex<Option<FileFault>>,
}

/// How the next [`DropStore::file`] goes wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileFault {
    /// The move commits and its answer is lost: the caller sees an error.
    AnswerLost,
    /// The move never happens and the caller sees an error.
    Refused,
}

impl SwitchableDrops {
    /// A working store.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Make the next `file` go wrong as `fault` says; every later one works.
    pub(crate) fn fault_next_file(&self, fault: FileFault) {
        *self
            .file_fault
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(fault);
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "drops",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl DropStore for SwitchableDrops {
    fn provision(&self, record: UploadLinkRecord) -> StoreFuture<'_, LinkProvisionOutcome> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.provision(record)
    }

    fn resolve<'a>(&'a self, opaque_id: &'a str) -> StoreFuture<'a, Option<UploadLinkRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.resolve(opaque_id)
    }

    fn revoke<'a>(
        &'a self,
        owner: &'a UserId,
        opaque_id: &'a str,
        at: Timestamp,
    ) -> StoreFuture<'a, bool> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.revoke(owner, opaque_id, at)
    }

    fn charge<'a>(
        &'a self,
        opaque_id: &'a str,
        size: u64,
        at: Timestamp,
    ) -> StoreFuture<'a, Admission> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.charge(opaque_id, size, at)
    }

    fn reserve(&self, pending: PendingDeposit, upload: &UploadId) -> StoreFuture<'_, ()> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.reserve(pending, upload)
    }

    fn take_reservation<'a>(
        &'a self,
        upload: &'a UploadId,
    ) -> StoreFuture<'a, Option<PendingDeposit>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.take_reservation(upload)
    }

    fn refund<'a>(&'a self, opaque_id: &'a str, size: u64) -> StoreFuture<'a, ()> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.refund(opaque_id, size)
    }

    fn deposit(&self, entry: InboxEntry) -> StoreFuture<'_, ()> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.deposit(entry)
    }

    fn file<'a>(
        &'a self,
        upload: &'a UploadId,
        address: &'a ContentAddress,
        received_at: Timestamp,
    ) -> StoreFuture<'a, Option<InboxEntry>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        let fault = self
            .file_fault
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match fault {
            None => self.inner.file(upload, address, received_at),
            Some(FileFault::Refused) => Box::pin(async { Self::refuse() }),
            Some(FileFault::AnswerLost) => Box::pin(async move {
                self.inner.file(upload, address, received_at).await?;
                Self::refuse()
            }),
        }
    }

    fn inbox<'a>(&'a self, owner: &'a UserId) -> StoreFuture<'a, Vec<InboxEntry>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.inbox(owner)
    }

    fn claim<'a>(
        &'a self,
        owner: &'a UserId,
        drop_id: &'a str,
    ) -> StoreFuture<'a, Option<InboxEntry>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.claim(owner, drop_id)
    }

    fn settle<'a>(&'a self, drop_id: &'a str) -> StoreFuture<'a, ()> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.settle(drop_id)
    }

    fn release<'a>(&'a self, drop_id: &'a str) -> StoreFuture<'a, ()> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.release(drop_id)
    }

    fn discard<'a>(&'a self, owner: &'a UserId, drop_id: &'a str) -> StoreFuture<'a, bool> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.discard(owner, drop_id)
    }
}

/// A share store that can be made to fail on demand.
///
/// The switch matters more here than elsewhere: the public serve path's `500` is the one answer
/// it gives that is *not* the indistinguishable `404`, and it exists because fail-closed means
/// a process that cannot confirm a link is live must refuse without claiming it was revoked.
#[derive(Debug, Default)]
pub(crate) struct SwitchableShares {
    inner: InMemoryShares,
    unavailable: AtomicBool,
}

impl SwitchableShares {
    /// A working store.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "shares",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl ShareStore for SwitchableShares {
    fn issue(&self, record: ShareRecord) -> StoreFuture<'_, IssueOutcome> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.issue(record)
    }

    fn resolve<'a>(&'a self, opaque_id: &'a str) -> StoreFuture<'a, Option<ShareRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.resolve(opaque_id)
    }

    fn revoke<'a>(
        &'a self,
        owner: &'a UserId,
        opaque_id: &'a str,
        at: Timestamp,
    ) -> StoreFuture<'a, bool> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.revoke(owner, opaque_id, at)
    }

    fn names_live<'a>(
        &'a self,
        address: &'a ContentAddress,
        now: Timestamp,
    ) -> StoreFuture<'a, bool> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.names_live(address, now)
    }
}

/// Whether the fixture's server federates (`FEDERATION_ENABLED`, #509), switchable per request.
///
/// A switch for the reason the stores carry one: `assert_declared_responses_covered` walks one
/// client's recording, so the `404` every federation operation declares for a deployment that
/// does not federate has to come from the same server that produces their `200`s. Production
/// hands in a fixed `bool`.
#[derive(Debug)]
pub(crate) struct SwitchableFederation {
    enabled: AtomicBool,
}

impl SwitchableFederation {
    /// A switch starting in `enabled`.
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
        }
    }

    /// Turn federation on or off for every subsequent request.
    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
    }
}

impl FederationSetting for SwitchableFederation {
    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }
}

/// The fixture server's chunk-body read deadline (#717), switchable per request.
///
/// A switch for the reason [`SwitchableFederation`] is one: the `408` both chunk routes declare
/// has to come from the same server that answers their `204`s, and an in-process body is always
/// whole, so the walk produces it by switching to a deadline that has already passed. Production
/// hands in a fixed `Duration`.
#[derive(Debug)]
pub(crate) struct SwitchableChunkDeadline {
    millis: AtomicU64,
}

impl SwitchableChunkDeadline {
    /// A switch starting at `timeout`.
    pub(crate) fn new(timeout: std::time::Duration) -> Self {
        let switch = Self {
            millis: AtomicU64::new(0),
        };
        switch.set(timeout);
        switch
    }

    /// Move the deadline for every subsequent chunk; [`Duration::ZERO`](std::time::Duration::ZERO)
    /// refuses every one.
    pub(crate) fn set(&self, timeout: std::time::Duration) {
        let millis = u64::try_from(timeout.as_millis()).expect("a test deadline fits in u64 ms");
        self.millis.store(millis, Ordering::SeqCst);
    }
}

impl ChunkReadDeadline for SwitchableChunkDeadline {
    fn chunk_read_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.millis.load(Ordering::SeqCst))
    }
}

/// A moderation store that can be made to fail on demand.
#[derive(Debug, Default)]
pub(crate) struct SwitchableModeration {
    inner: InMemoryModeration,
    unavailable: AtomicBool,
}

impl SwitchableModeration {
    /// A working store: every account active, nothing on record.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "moderation",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl ModerationStore for SwitchableModeration {
    fn apply(&self, event: ModerationEvent, standing: Option<Standing>) -> StoreFuture<'_, ()> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.apply(event, standing)
    }

    fn standing<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Standing> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.standing(user)
    }

    fn events_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<ModerationEvent>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.events_for_user(user)
    }

    fn file_report(&self, report: FederatedReport) -> StoreFuture<'_, ()> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.file_report(report)
    }

    fn pending_reports(&self) -> StoreFuture<'_, Vec<FederatedReport>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.pending_reports()
    }
}

/// An enrollment-code store that can be made to fail on demand.
#[derive(Debug)]
pub(crate) struct SwitchableEnrollments {
    inner: InMemoryEnrollments,
    unavailable: AtomicBool,
}

impl SwitchableEnrollments {
    /// A working store on `clock`, with the ceremony's own TTL.
    pub(crate) fn new(clock: Arc<ManualClock>) -> Self {
        Self {
            inner: InMemoryEnrollments::new(clock, ENROLLMENT_CODE_TTL),
            unavailable: AtomicBool::new(false),
        }
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "enrollment",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl EnrollmentStore for SwitchableEnrollments {
    fn ttl(&self) -> SignedDuration {
        self.inner.ttl()
    }

    fn issue(&self, record: PendingEnrollment) -> StoreFuture<'_, ()> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.issue(record)
    }

    fn is_taken<'a>(&'a self, code: &'a EnrollmentCode) -> StoreFuture<'a, bool> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.is_taken(code)
    }

    fn redeem<'a>(
        &'a self,
        code: &'a EnrollmentCode,
    ) -> StoreFuture<'a, Option<PendingEnrollment>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.redeem(code)
    }
}

/// A relay-channel store that can be made to fail on demand.
#[derive(Debug)]
pub(crate) struct SwitchableChannels {
    inner: InMemoryChannels,
    unavailable: AtomicBool,
}

impl SwitchableChannels {
    /// A working store on `clock`, with the ceremony's own TTL.
    pub(crate) fn new(clock: Arc<ManualClock>) -> Self {
        Self {
            inner: InMemoryChannels::new(clock, RELAY_CHANNEL_TTL),
            unavailable: AtomicBool::new(false),
        }
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "relay-channel",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl ChannelStore for SwitchableChannels {
    fn ttl(&self) -> SignedDuration {
        self.inner.ttl()
    }

    fn open<'a>(&'a self, channel: &'a ChannelId, record: RelayChannel) -> StoreFuture<'a, ()> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.open(channel, record)
    }

    fn lookup<'a>(&'a self, channel: &'a ChannelId) -> StoreFuture<'a, Option<RelayChannel>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.lookup(channel)
    }

    fn enqueue<'a>(
        &'a self,
        channel: &'a ChannelId,
        direction: Direction,
        payload: RelayPayload,
    ) -> StoreFuture<'a, RelayOutcome> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.enqueue(channel, direction, payload)
    }

    fn drain<'a>(
        &'a self,
        channel: &'a ChannelId,
        direction: Direction,
    ) -> StoreFuture<'a, DrainOutcome> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.drain(channel, direction)
    }

    fn close<'a>(&'a self, channel: &'a ChannelId) -> StoreFuture<'a, bool> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.close(channel)
    }
}

/// A cohort map that can be made to fail on demand.
///
/// Delegating to the real in-memory one. The switch exists because the listing's `500` is
/// otherwise reachable only through the *session* store, and a status that only one of two
/// collaborators can produce is a status half-tested.
#[derive(Debug, Default)]
pub(crate) struct SwitchableCohorts {
    inner: InMemoryCohorts,
    unavailable: AtomicBool,
}

impl SwitchableCohorts {
    /// A working map.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "device-cohorts",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl CohortStore for SwitchableCohorts {
    fn observe<'a>(
        &'a self,
        user: &'a UserId,
        cohort_hash: &'a str,
        at: Timestamp,
    ) -> StoreFuture<'a, CohortRecord> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.observe(user, cohort_hash, at)
    }

    fn cohorts_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<CohortRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.cohorts_for_user(user)
    }
}

/// An escrow store that can be made to fail on demand.
///
/// Delegating to the real in-memory one. The switch exists because the `500` on both escrow
/// operations is otherwise unreachable, and a declared status nothing can reach is the
/// defect this rebuild exists to make impossible.
#[derive(Debug, Default)]
pub(crate) struct SwitchableEscrow {
    inner: InMemoryEscrow,
    unavailable: AtomicBool,
}

impl SwitchableEscrow {
    /// A working store.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "escrow",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl EscrowStore for SwitchableEscrow {
    fn store(&self, record: EscrowRecord) -> StoreFuture<'_, Replaced> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.store(record)
    }

    fn fetch<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Option<EscrowRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.fetch(user)
    }

    fn store_identity(&self, record: EscrowRecord) -> StoreFuture<'_, Replaced> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.store_identity(record)
    }

    fn fetch_identity<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Option<EscrowRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.fetch_identity(user)
    }

    fn store_album_keys(&self, record: AlbumKeyRecord) -> StoreFuture<'_, AlbumKeysStored> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.store_album_keys(record)
    }

    fn list_album_keys<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<AlbumKeyRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.list_album_keys(user)
    }
}

/// A revoke-all challenge store that can be made to fail on demand.
///
/// Delegating to the real in-memory one, so a case that walks the ceremony is walking the
/// adapter the conformance suite covers. The switch exists because the `500` on both revoke-all
/// operations is otherwise unreachable, and a declared status nothing can reach is the
/// defect this rebuild exists to make impossible.
#[derive(Debug)]
pub(crate) struct SwitchableChallenges {
    inner: InMemoryChallenges,
    unavailable: AtomicBool,
}

impl SwitchableChallenges {
    /// A working store on `clock`, with the ceremony's own TTL.
    pub(crate) fn new(clock: Arc<ManualClock>) -> Self {
        Self {
            inner: InMemoryChallenges::with_default_ttl(clock),
            unavailable: AtomicBool::new(false),
        }
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "revoke-all-challenge",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl ChallengeStore for SwitchableChallenges {
    fn ttl(&self) -> SignedDuration {
        self.inner.ttl()
    }

    fn issue<'a>(
        &'a self,
        token: &'a ChallengeToken,
        record: RevokeAllChallenge,
    ) -> StoreFuture<'a, ()> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.issue(token, record)
    }

    fn consume<'a>(
        &'a self,
        token: &'a ChallengeToken,
    ) -> StoreFuture<'a, Option<RevokeAllChallenge>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.consume(token)
    }
}

impl AuthStateStore for SwitchableSessions {
    fn ttl(&self) -> SignedDuration {
        self.inner.ttl()
    }

    fn open_session(&self, record: SessionRecord) -> StoreFuture<'_, ()> {
        if self.is_down_past_the_door() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.open_session(record)
    }

    fn read_session<'a>(
        &'a self,
        session: &'a SessionId,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.read_session(session)
    }

    fn touch_session<'a>(
        &'a self,
        session: &'a SessionId,
        last_active_at: Timestamp,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        if self.is_down_past_the_door() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.touch_session(session, last_active_at)
    }

    fn mark_authenticated<'a>(
        &'a self,
        session: &'a SessionId,
        at: Timestamp,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        if self.is_down_past_the_door() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.mark_authenticated(session, at)
    }

    fn close_session<'a>(
        &'a self,
        session: &'a SessionId,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        if self.is_down_past_the_door() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.close_session(session)
    }

    fn sessions_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<SessionRecord>> {
        if self.is_down_past_the_door() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.sessions_for_user(user)
    }

    fn close_all_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<SessionRecord>> {
        if self.is_down_past_the_door() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.close_all_for_user(user)
    }
}

// ===========================================================================================
// Upload-session store double
// ===========================================================================================

/// The in-memory upload store, with the same switch [`SwitchableSessions`] carries.
///
/// Delegation, not reimplementation: with the switch off this *is* the adapter the shared
/// conformance suite passes, so an assertion about a session's status or its accepted chunks is
/// an assertion about the real thing.
#[derive(Debug)]
pub(crate) struct SwitchableUploads {
    inner: InMemoryUploadSessions,
    unavailable: AtomicBool,
    claim_after_progress: AtomicBool,
    status_writes_refused: AtomicBool,
}

impl SwitchableUploads {
    /// A working store on `clock`, with the port's own 24-hour lifetime cap.
    pub(crate) fn new(clock: Arc<ManualClock>) -> Self {
        Self {
            inner: InMemoryUploadSessions::with_default_ttl(clock),
            unavailable: AtomicBool::new(false),
            claim_after_progress: AtomicBool::new(false),
            status_writes_refused: AtomicBool::new(false),
        }
    }

    /// Refuse every subsequent [`UploadSessionStore::set_status`], or stop.
    ///
    /// Only the status write: a finalizer that loses it after its claim can neither complete nor
    /// fail its session, which is the session a process that died mid-finalize leaves behind —
    /// claimed, with every step before the terminal write done.
    pub(crate) fn refuse_status_writes(&self, refused: bool) {
        self.status_writes_refused.store(refused, Ordering::SeqCst);
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "upload-session",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }

    /// The record `id` names, for an assertion about what the server wrote.
    ///
    /// A test asserts against the store the server just wrote to, never against a second
    /// reading of the response body — a response can say anything.
    pub(crate) async fn read_for_test(&self, id: &str) -> Option<UploadSessionRecord> {
        self.inner
            .read(&UploadId::new(id))
            .await
            .expect("the in-memory store answers")
    }

    /// Let a concurrent finalizer win the claim in the window the racing request cannot see:
    /// between its chunk being recorded and its own `claim_finalize`.
    ///
    /// The window is real and is exactly one `await` wide, so it cannot be driven from outside
    /// the server. Arming it here is the only way to make the losing side of the race a case
    /// rather than a comment.
    pub(crate) fn claim_after_next_progress(&self) {
        self.claim_after_progress.store(true, Ordering::SeqCst);
    }

    /// Claim `id`'s finalization out from under the server, as a concurrent finalizer would.
    pub(crate) async fn claim_for_test(&self, id: &str) {
        let claim = self
            .inner
            .claim_finalize(&UploadId::new(id))
            .await
            .expect("the in-memory store answers");
        assert!(
            matches!(claim, FinalizeClaim::Won(_)),
            "the test must be the one that wins the claim it is simulating"
        );
    }

    /// Open `count` live sessions for `uploader`, each for a different blob, as a client that
    /// leaks sessions would leave them.
    pub(crate) async fn fill_live_sessions_for_test(
        &self,
        uploader: &UserId,
        count: usize,
        now: jiff::Timestamp,
    ) {
        for index in 0..count {
            self.inner
                .open(UploadSessionRecord {
                    upload_id: UploadId::new(format!("leaked-{index:06}")),
                    asset_id: AssetId::new(format!("leaked-asset-{index:06}")),
                    owner_id: OwnerId::new(uploader.as_str()),
                    upload_user_id: uploader.clone(),
                    album_id: None,
                    content_type: None,
                    expected_hash: format!("{index:064x}"),
                    crypto_suite_id: 1,
                    protocol_version: PROTOCOL_VERSION.to_owned(),
                    blob_role: capsule_server::store::BlobRole::Original,
                    intent_id: None,
                    manifest_envelope: "{}".to_owned(),
                    received_bytes: 0,
                    total_size: 4096,
                    status: UploadSessionStatus::Pending,
                    created_at: now,
                    last_progress_at: now,
                })
                .await
                .expect("the in-memory store opens");
        }
    }
}

impl UploadSessionStore for SwitchableUploads {
    fn ttl(&self) -> SignedDuration {
        self.inner.ttl()
    }

    fn open(&self, record: UploadSessionRecord) -> StoreFuture<'_, ()> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.open(record)
    }

    fn read<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.read(upload)
    }

    fn sessions_for_uploader<'a>(
        &'a self,
        uploader: &'a UserId,
    ) -> StoreFuture<'a, Vec<UploadSessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.sessions_for_uploader(uploader)
    }

    fn record_progress<'a>(
        &'a self,
        upload: &'a UploadId,
        chunk: AcceptedChunk,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        Box::pin(async move {
            let updated = self.inner.record_progress(upload, chunk).await?;
            if self.claim_after_progress.swap(false, Ordering::SeqCst) {
                self.inner.claim_finalize(upload).await?;
            }
            Ok(updated)
        })
    }

    fn chunk_at<'a>(
        &'a self,
        upload: &'a UploadId,
        offset: u64,
    ) -> StoreFuture<'a, Option<AcceptedChunk>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.chunk_at(upload, offset)
    }

    fn reconcile_received_bytes<'a>(
        &'a self,
        upload: &'a UploadId,
        on_disk: u64,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.reconcile_received_bytes(upload, on_disk)
    }

    fn set_status<'a>(
        &'a self,
        upload: &'a UploadId,
        status: UploadSessionStatus,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        if self.is_down() || self.status_writes_refused.load(Ordering::SeqCst) {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.set_status(upload, status)
    }

    fn claim_finalize<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, FinalizeClaim> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.claim_finalize(upload)
    }

    fn claimed(&self) -> StoreFuture<'_, Vec<UploadSessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.claimed()
    }

    fn discard<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.discard(upload)
    }

    fn pending_for_address<'a>(
        &'a self,
        uploader: &'a UserId,
        expected_hash: &'a str,
    ) -> StoreFuture<'a, Option<UploadId>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.pending_for_address(uploader, expected_hash)
    }

    fn in_flight_for_album<'a>(&'a self, album: &'a AlbumId) -> StoreFuture<'a, u64> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.in_flight_for_album(album)
    }

    fn least_recently_progressed(
        &self,
        not_progressed_since: Timestamp,
        limit: usize,
    ) -> StoreFuture<'_, Vec<UploadId>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner
            .least_recently_progressed(not_progressed_since, limit)
    }
}

// ===========================================================================================
// Blob store double
// ===========================================================================================

/// The in-memory blob store, with one seam: it can **swallow** an append.
///
/// The seam simulates the one failure the upload protocol's validation section calls out and
/// that no in-process test could otherwise reach — a crash between a durable append and the
/// counter that records it. With `swallow_next_append` armed, the store answers as though it
/// wrote the bytes and writes nothing, so the session's counter runs ahead of the stage. The
/// next append then meets the port's own offset cross-check and the divergence surfaces as
/// `500 error.upload.storage_inconsistent` — which is exactly what it is: the server's
/// inconsistency, never the client's fault.
#[derive(Debug)]
pub(crate) struct SwallowingBlobs {
    inner: InMemoryBlobStore,
    swallow: AtomicBool,
    /// Answer the next append as a full disk would, writing nothing — an adapter that honours
    /// the port's "a failed append leaves the stage at its offset" rule.
    full_next_append: AtomicBool,
    /// Answer the next `begin` as a full disk would, staging nothing.
    full_next_begin: AtomicBool,
    /// Park every append until the test lets it go, the way [`Self::hold_puts`] parks a `put`.
    held_appends: Mutex<Option<PutHold>>,
    /// Park every commit, *after* it has landed, until the test lets it go.
    held_commits: Mutex<Option<PutHold>>,
    /// Refuse every [`BlobStore::put`], as a full or read-only disk would.
    puts_unavailable: AtomicBool,
    /// Park every [`BlobStore::put`] until the test lets it go.
    held_puts: Mutex<Option<PutHold>>,
}

/// A `put` parked by [`SwallowingBlobs::hold_puts`].
///
/// `entered` fires when a `put` reaches the store; it then waits on `release` before it either
/// refuses or writes, so a test can act on the caller in between.
#[derive(Debug, Clone)]
pub(crate) struct PutHold {
    /// Notified once per `put` that has reached the store.
    pub(crate) entered: Arc<tokio::sync::Notify>,
    /// One permit lets one parked `put` go on.
    pub(crate) release: Arc<tokio::sync::Semaphore>,
}

impl SwallowingBlobs {
    /// A working store.
    pub(crate) fn new() -> Self {
        Self {
            inner: InMemoryBlobStore::new(),
            swallow: AtomicBool::new(false),
            full_next_append: AtomicBool::new(false),
            full_next_begin: AtomicBool::new(false),
            held_appends: Mutex::new(None),
            held_commits: Mutex::new(None),
            puts_unavailable: AtomicBool::new(false),
            held_puts: Mutex::new(None),
        }
    }

    /// Park every subsequent `put` until the returned hold releases it.
    pub(crate) fn hold_puts(&self) -> PutHold {
        let hold = PutHold {
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Semaphore::new(0)),
        };
        *self
            .held_puts
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(hold.clone());
        hold
    }

    /// Drop the next append on the floor while telling the caller it landed.
    pub(crate) fn swallow_next_append(&self) {
        self.swallow.store(true, Ordering::SeqCst);
    }

    /// Fail the next append with `ENOSPC`, as a disk that filled mid-write would.
    pub(crate) fn fill_disk_on_next_append(&self) {
        self.full_next_append.store(true, Ordering::SeqCst);
    }

    /// Fail the next `begin` with `ENOSPC`, as a disk with no room for a new stage would.
    pub(crate) fn fill_disk_on_next_begin(&self) {
        self.full_next_begin.store(true, Ordering::SeqCst);
    }

    /// Park every subsequent append until the returned hold releases it.
    pub(crate) fn hold_appends(&self) -> PutHold {
        let hold = PutHold {
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Semaphore::new(0)),
        };
        *self
            .held_appends
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(hold.clone());
        hold
    }

    /// Park every subsequent commit until the returned hold releases it.
    ///
    /// The commit has **already landed** when `entered` fires: the bytes are at their content
    /// address and the stage is gone. That is the window a finalizer lost between its commit and
    /// its index write leaves behind, which a case reaches by acting on the caller while the
    /// commit is parked.
    pub(crate) fn hold_commits(&self) -> PutHold {
        let hold = PutHold {
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Semaphore::new(0)),
        };
        *self
            .held_commits
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(hold.clone());
        hold
    }

    /// Make every subsequent `put` fail, or stop.
    pub(crate) fn set_puts_unavailable(&self, unavailable: bool) {
        self.puts_unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// The staged length for `id`, or `None` when nothing is staged.
    pub(crate) async fn staged_len_for_test(&self, id: &str) -> Option<u64> {
        self.inner
            .staged_len(&UploadId::new(id))
            .await
            .expect("the in-memory store answers")
    }

    /// Rot the bytes under `address`, exactly as a failing disk would.
    ///
    /// Built from the port's own `remove` + `put` rather than from a test-only hook, so what it
    /// produces is a state the real store can genuinely be in: a committed address whose bytes
    /// are not its digest. That is the state a deep verify exists to find and a structural one
    /// cannot.
    pub(crate) async fn corrupt(&self, address: &ContentAddress) {
        self.inner
            .remove(address)
            .await
            .expect("the in-memory store removes");
        self.inner
            .put(
                address,
                b"these are not the bytes that hash to this address",
            )
            .await
            .expect("the in-memory store puts");
    }

    /// The committed blob at `hash`, whole.
    pub(crate) async fn blob_for_test(&self, hash: &str) -> Option<Vec<u8>> {
        let address = ContentAddress::parse(hash).expect("a content address");
        let stat = self
            .inner
            .stat(&address)
            .await
            .expect("the in-memory store answers")?;
        self.inner
            .read_at(&address, 0, stat.size as usize)
            .await
            .expect("the in-memory store answers")
    }

    /// How many blobs the store holds — the assertion deduplication is made with.
    pub(crate) async fn blob_count_for_test(&self) -> usize {
        self.inner
            .enumerate(None, 100)
            .await
            .expect("the in-memory store answers")
            .entries
            .len()
    }
}

impl BlobStore for SwallowingBlobs {
    fn begin<'a>(&'a self, upload: &'a UploadId) -> BlobFuture<'a, ()> {
        if self.full_next_begin.swap(false, Ordering::SeqCst) {
            return Box::pin(async move {
                Err(BlobError::StorageFull {
                    operation: "create a staging file",
                })
            });
        }
        self.inner.begin(upload)
    }

    fn append<'a>(
        &'a self,
        upload: &'a UploadId,
        offset: u64,
        bytes: &'a [u8],
    ) -> BlobFuture<'a, u64> {
        if self.swallow.swap(false, Ordering::SeqCst) {
            return Box::pin(async move { Ok(offset + bytes.len() as u64) });
        }
        if self.full_next_append.swap(false, Ordering::SeqCst) {
            return Box::pin(async move {
                Err(BlobError::StorageFull {
                    operation: "append to a staging file",
                })
            });
        }
        let hold = self
            .held_appends
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        Box::pin(async move {
            if let Some(hold) = hold {
                hold.entered.notify_one();
                let _permit = hold
                    .release
                    .acquire()
                    .await
                    .expect("the hold is never closed");
            }
            self.inner.append(upload, offset, bytes).await
        })
    }

    fn truncate_staged<'a>(
        &'a self,
        upload: &'a UploadId,
        len: u64,
    ) -> BlobFuture<'a, Option<u64>> {
        self.inner.truncate_staged(upload, len)
    }

    fn staged_len<'a>(&'a self, upload: &'a UploadId) -> BlobFuture<'a, Option<u64>> {
        self.inner.staged_len(upload)
    }

    fn read_staged_at<'a>(
        &'a self,
        upload: &'a UploadId,
        offset: u64,
        len: usize,
    ) -> BlobFuture<'a, Option<Vec<u8>>> {
        self.inner.read_staged_at(upload, offset, len)
    }

    fn abandon<'a>(&'a self, upload: &'a UploadId) -> BlobFuture<'a, bool> {
        self.inner.abandon(upload)
    }

    fn staged(&self) -> BlobFuture<'_, Vec<UploadId>> {
        self.inner.staged()
    }

    fn commit<'a>(
        &'a self,
        upload: &'a UploadId,
        address: &'a ContentAddress,
    ) -> BlobFuture<'a, Placement> {
        let hold = self
            .held_commits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        Box::pin(async move {
            let placement = self.inner.commit(upload, address).await?;
            if let Some(hold) = hold {
                hold.entered.notify_one();
                let _permit = hold
                    .release
                    .acquire()
                    .await
                    .expect("the hold is never closed");
            }
            Ok(placement)
        })
    }

    fn put<'a>(
        &'a self,
        address: &'a ContentAddress,
        bytes: &'a [u8],
    ) -> BlobFuture<'a, Placement> {
        let hold = self
            .held_puts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        Box::pin(async move {
            if let Some(hold) = hold {
                hold.entered.notify_one();
                let _permit = hold
                    .release
                    .acquire()
                    .await
                    .expect("the hold is never closed");
            }
            // Read after the hold, so a test can decide how a parked `put` ends.
            if self.puts_unavailable.load(Ordering::SeqCst) {
                return Err(BlobError::Backend {
                    operation: "put",
                    detail: REFUSAL.to_owned(),
                });
            }
            self.inner.put(address, bytes).await
        })
    }

    fn stat<'a>(&'a self, address: &'a ContentAddress) -> BlobFuture<'a, Option<BlobStat>> {
        self.inner.stat(address)
    }

    fn read_at<'a>(
        &'a self,
        address: &'a ContentAddress,
        offset: u64,
        len: usize,
    ) -> BlobFuture<'a, Option<Vec<u8>>> {
        self.inner.read_at(address, offset, len)
    }

    fn enumerate<'a>(
        &'a self,
        after: Option<&'a ContentAddress>,
        limit: usize,
    ) -> BlobFuture<'a, BlobPage> {
        self.inner.enumerate(after, limit)
    }

    fn remove<'a>(&'a self, address: &'a ContentAddress) -> BlobFuture<'a, bool> {
        self.inner.remove(address)
    }

    fn quarantine<'a>(
        &'a self,
        address: &'a ContentAddress,
        reason: QuarantineReason,
    ) -> BlobFuture<'a, bool> {
        self.inner.quarantine(address, reason)
    }

    fn quarantined(&self) -> BlobFuture<'_, Vec<QuarantinedBlob>> {
        self.inner.quarantined()
    }
}

// ===========================================================================================
// Write-authority double
// ===========================================================================================

/// The album and device facts invariants 6 and 7 are decided against.
///
/// There is no adapter for [`WriteAuthority`] in `src/` — the real one is Postgres — so this is
/// the only implementation the suite has, and it is deliberately a *directory*, not a
/// permission bit: a test revokes a device by removing its row, exactly as the real directory
/// would, rather than by flipping a flag the port does not have.
#[derive(Debug, Default)]
pub(crate) struct TestAuthority {
    /// Each album's owner and protocol pin.
    albums: Mutex<BTreeMap<String, (String, String)>>,
    /// Each `(album, member)`'s role on the roster.
    shares: Mutex<BTreeMap<(String, String), MemberRole>>,
    upgrades: Mutex<BTreeMap<(String, String), Uuid>>,
    devices: Mutex<BTreeMap<(String, Uuid), Timestamp>>,
    /// A device's published signing key where a case set one; [`device_key`] otherwise.
    keys: Mutex<BTreeMap<(String, Uuid), capsule_core::crypto::keys::HybridVerifyingKey>>,
    unavailable: AtomicBool,
}

impl TestAuthority {
    /// An authority that knows nothing.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record `album` as writable by `owner`, pinned to `protocol_pin`.
    pub(crate) fn allow_album(&self, owner: &OwnerId, album: &AlbumId, protocol_pin: &str) {
        self.albums().insert(
            album.as_str().to_owned(),
            (owner.as_str().to_owned(), protocol_pin.to_owned()),
        );
    }

    /// Put `member` on `album`'s roster with `role`.
    pub(crate) fn share(&self, album: &AlbumId, member: &UserId, role: MemberRole) {
        self.shares().insert(
            (album.as_str().to_owned(), member.as_str().to_owned()),
            role,
        );
    }

    /// Take `member` off `album`'s roster.
    pub(crate) fn unshare(&self, album: &AlbumId, member: &UserId) {
        self.shares()
            .remove(&(album.as_str().to_owned(), member.as_str().to_owned()));
    }

    /// Put an album into upgrade quiescence under `intent`.
    ///
    /// The double carries the fact the production authority reads off the album record, so a
    /// case can assert what a quiescing album does to an upload without standing up the ceremony
    /// that got it there.
    pub(crate) fn quiesce_album(&self, owner: &OwnerId, album: &AlbumId, intent: Uuid) {
        self.upgrades
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                (owner.as_str().to_owned(), album.as_str().to_owned()),
                intent,
            );
    }

    /// The ceremony an album is quiescing under, if any.
    fn quiescing_under(&self, owner: &OwnerId, album: &AlbumId) -> Option<Uuid> {
        self.upgrades
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(owner.as_str().to_owned(), album.as_str().to_owned()))
            .copied()
    }

    /// Forget an album, as a closed one would be.
    ///
    /// `owner` is asserted rather than looked up: the map is album-keyed, and a case that closes
    /// the wrong owner's album would otherwise pass vacuously.
    pub(crate) fn close_album(&self, owner: &OwnerId, album: &AlbumId) {
        let mut albums = self.albums();
        assert!(
            albums
                .get(album.as_str())
                .is_some_and(|(held, _)| held == owner.as_str()),
            "close_album: {album} is not {owner}'s"
        );
        {
            albums.remove(album.as_str());
        }
    }

    /// Record `device` as entering `user`'s directory at `added_at`.
    pub(crate) fn add_device(&self, user: &UserId, device: Uuid, added_at: Timestamp) {
        self.devices()
            .insert((user.as_str().to_owned(), device), added_at);
    }

    /// Publish `key` as `device`'s signing key, for a case whose manifests a real library signs.
    pub(crate) fn set_device_key(
        &self,
        user: &UserId,
        device: Uuid,
        key: capsule_core::crypto::keys::HybridVerifyingKey,
    ) {
        self.keys
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((user.as_str().to_owned(), device), key);
    }

    /// Remove a device from the directory, as a revocation would.
    pub(crate) fn revoke_device(&self, user: &UserId, device: Uuid) {
        self.devices().remove(&(user.as_str().to_owned(), device));
    }

    /// Make every subsequent lookup fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn albums(&self) -> MutexGuard<'_, BTreeMap<String, (String, String)>> {
        self.albums.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn shares(&self) -> MutexGuard<'_, BTreeMap<(String, String), MemberRole>> {
        self.shares.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn devices(&self) -> MutexGuard<'_, BTreeMap<(String, Uuid), Timestamp>> {
        self.devices.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl WriteAuthority for TestAuthority {
    fn album_write_access<'a>(
        &'a self,
        caller: &'a UserId,
        album: &'a AlbumId,
    ) -> AuthorityFuture<'a, AlbumWriteAccess> {
        Box::pin(async move {
            if self.is_down() {
                return Err(AuthorityError::unavailable(REFUSAL));
            }
            let Some((owner, pin)) = self.albums().get(album.as_str()).cloned() else {
                return Ok(AlbumWriteAccess::Denied);
            };
            let role = if owner == caller.as_str() {
                WriteRole::Owner
            } else {
                match self
                    .shares()
                    .get(&(album.as_str().to_owned(), caller.as_str().to_owned()))
                {
                    Some(MemberRole::Writer) => WriteRole::Member,
                    _ => return Ok(AlbumWriteAccess::Denied),
                }
            };
            let owner_id = OwnerId::new(owner);
            Ok(AlbumWriteAccess::Writable {
                protocol_pin: pin,
                quiescing_under: self.quiescing_under(&owner_id, album),
                owner_id,
                role,
            })
        })
    }

    fn device_added_at<'a>(
        &'a self,
        user: &'a UserId,
        device: Uuid,
    ) -> AuthorityFuture<'a, Option<Timestamp>> {
        Box::pin(async move {
            if self.is_down() {
                return Err(AuthorityError::unavailable(REFUSAL));
            }
            Ok(self
                .devices()
                .get(&(user.as_str().to_owned(), device))
                .copied())
        })
    }

    fn device_directory<'a>(
        &'a self,
        user: &'a UserId,
    ) -> AuthorityFuture<'a, Option<capsule_core::crypto::keys::DeviceDirectory>> {
        Box::pin(async move {
            use capsule_core::crypto::keys::{DeviceEntry, DirectoryCore};

            if self.is_down() {
                return Err(AuthorityError::unavailable(REFUSAL));
            }
            let Ok(user_id) = Uuid::parse_str(user.as_str()) else {
                return Ok(None);
            };
            let keys = self
                .keys
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let devices: Vec<DeviceEntry> = self
                .devices()
                .iter()
                .filter(|((holder, _), _)| holder == user.as_str())
                .map(|((holder, device), added_at)| DeviceEntry {
                    device_id: *device,
                    dsk_public: keys
                        .get(&(holder.clone(), *device))
                        .cloned()
                        .unwrap_or_else(|| device_key(*device).verifying_key()),
                    dek_public: None,
                    added_at: added_at.to_string(),
                    revoked_at: None,
                    unknown: Default::default(),
                })
                .collect();
            if devices.is_empty() {
                return Ok(None);
            }
            // Signed under a fixed identity key: the signer check reads entries, and the
            // directory's own signature was the publish route's to verify, not this double's.
            Ok(Some(
                DirectoryCore {
                    user_id,
                    directory_version: 1,
                    updated_at: "2026-01-01T00:00:00Z".to_owned(),
                    devices,
                    unknown: Default::default(),
                }
                .sign(&HybridSigningKey::from_seed_bytes(&[0x1D; 32], &[0x1D; 32])),
            ))
        })
    }
}

/// The signing key [`TestAuthority`] publishes for `device`: derived from its id, so a case signs
/// a manifest as a device just by naming it ([`signed_provenance`]).
pub(crate) fn device_key(device: Uuid) -> HybridSigningKey {
    let mut seed = [0u8; 32];
    seed[..16].copy_from_slice(device.as_bytes());
    seed[16..].copy_from_slice(device.as_bytes());
    HybridSigningKey::from_seed_bytes(&seed, &seed)
}

/// A provenance blob for the envelope `request` declares: the canonical CBOR of a provenance
/// record whose manifest says exactly what the envelope says, signed by the device it names under
/// [`device_key`] — what a real client uploads as the `provenance` member of a bundle.
///
/// `request` is a `POST /v1/upload` body ([`create_request`] and friends); only its
/// `manifest_envelope` is read. The manifest's own `ciphertext_hash` is the envelope's
/// `original_blob_hash` when it carries one, and otherwise a fixed value no blob in a case has.
pub(crate) fn signed_provenance(request: &serde_json::Value) -> Vec<u8> {
    signed_provenance_by(request, device_key)
}

/// A provenance member's `POST /v1/upload` body and the signed bytes it declares: a
/// [`create_request`] for the `provenance` role that commits to a metadata blob, as every create
/// manifest does, adjusted by `adjust`, then signed ([`signed_provenance`]) and declared at the
/// signed bytes' own size and address.
pub(crate) fn provenance_upload(
    clock: &ManualClock,
    adjust: impl FnOnce(&mut serde_json::Value),
) -> (serde_json::Value, Vec<u8>) {
    let mut request = create_request(clock, b"a signed manifest", "provenance");
    request["manifest_envelope"]["metadata_blob_hash"] = "22".repeat(32).into();
    adjust(&mut request);
    let bytes = signed_provenance(&request);
    declare_bytes(&mut request, &bytes);
    (request, bytes)
}

/// Sign the manifest `request`'s envelope describes ([`signed_provenance`]), declare `request`
/// at the signed bytes, and return them: a provenance body built any other way, made the
/// signed manifest a real client would send under it.
pub(crate) fn sign_provenance_body(request: &mut serde_json::Value) -> Vec<u8> {
    let bytes = signed_provenance(request);
    declare_bytes(request, &bytes);
    bytes
}

/// Point `request`'s declared size and address at `bytes`.
pub(crate) fn declare_bytes(request: &mut serde_json::Value, bytes: &[u8]) {
    let hash = checksum(bytes);
    request["size"] = bytes.len().into();
    request["hash"] = hash.clone().into();
    request["manifest_envelope"]["ciphertext_hash"] = hash.into();
}

/// `bytes` as the two chunks a case sends: the first 4 KiB, then the rest.
pub(crate) fn two_chunks(bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    assert!(
        bytes.len() > 4096,
        "a signed manifest is larger than one 4 KiB chunk"
    );
    (bytes[..4096].to_vec(), bytes[4096..].to_vec())
}

/// [`signed_provenance`], signed by whatever key `key_for` returns for the named device — a case
/// signs with the wrong key to watch the signer check refuse.
pub(crate) fn signed_provenance_by(
    request: &serde_json::Value,
    key_for: impl Fn(Uuid) -> HybridSigningKey,
) -> Vec<u8> {
    use capsule_core::crypto::hash::Hash32;
    use capsule_core::crypto::keys::AmkVersion;
    use capsule_core::crypto::provenance::{
        ASSET_MANIFEST_VERSION, Action, KeyMode, ManifestCore, ProvenanceRecord,
    };

    let envelope = &request["manifest_envelope"];
    let text = |field: &str| envelope[field].as_str().map(str::to_owned);
    let uuid = |field: &str| {
        Uuid::parse_str(envelope[field].as_str().expect("the envelope names it"))
            .expect("the envelope's id is a uuid")
    };
    let hash = |field: &str| text(field).map(|hex| Hash32::from_hex(&hex).expect("a hex hash"));
    let wire = |field: &str| format!("\"{}\"", envelope[field].as_str().expect("a wire enum"));
    let action: Action = serde_json::from_str(&wire("action")).expect("a known action");
    let key_mode: KeyMode = serde_json::from_str(&wire("key_mode")).expect("a known key mode");
    let device = uuid("created_by_device");
    let prior = hash("prior_provenance_hash");
    let core = ManifestCore {
        version: ASSET_MANIFEST_VERSION.into(),
        crypto_suite_id: u16::try_from(envelope["crypto_suite_id"].as_u64().expect("a suite"))
            .expect("a suite fits"),
        protocol_version: text("protocol_version").expect("a protocol version"),
        file_id: uuid("file_id"),
        album_id: uuid("album_id"),
        amk_version: AmkVersion(
            u32::try_from(envelope["amk_version"].as_u64().expect("an epoch"))
                .expect("an epoch fits"),
        ),
        ciphertext_hash: hash("original_blob_hash").unwrap_or(Hash32([0xC1; 32])),
        plaintext_size: envelope["plaintext_size"].as_u64().expect("a size"),
        chunk_size: u32::try_from(envelope["chunk_size"].as_u64().expect("a chunk size"))
            .expect("a chunk size fits"),
        nonce_prefix: [0u8; 7],
        key_mode,
        wrapped_file_key: None,
        metadata_blob_hash: hash("metadata_blob_hash"),
        created_by_user: uuid("created_by_user"),
        created_by_device: device,
        client_version: text("client_version").expect("a client version"),
        timestamp: text("timestamp").expect("a timestamp"),
        action,
        prior_provenance_hash: prior,
        upgraded_from: None,
        unknown: Default::default(),
        retention_until: text("retention_until"),
    };
    let key = key_for(device);
    let manifest = core.sign(&key, &key).expect("a software key signs");
    let record = ProvenanceRecord::new(manifest.core.file_id, manifest, prior);
    capsule_core::cbor::to_canonical_vec(&record).expect("a provenance record encodes")
}

// ===========================================================================================
// The fixture
// ===========================================================================================

/// The asset index, with a switch that makes it refuse.
///
/// Delegation, not reimplementation: with the switch off this *is* the adapter the shared
/// conformance suite passes, so an assertion about a published asset is an assertion about the
/// real thing.
#[derive(Debug, Default)]
pub(crate) struct SwitchableIndex {
    inner: InMemoryAssetIndex,
    unavailable: AtomicBool,
}

/// The owner ledger, with a switch that makes every append fail.
///
/// Delegation, as [`SwitchableIndex`] is: with the switch off this is the in-memory adapter the
/// ledger conformance suite passes.
#[derive(Debug, Default)]
pub(crate) struct SwitchableLedger {
    inner: InMemoryLedger,
    unavailable: AtomicBool,
    /// Fail [`OwnerLedger::append`] alone, so a caller that attaches first gets past the attach.
    appends_unavailable: AtomicBool,
}

impl SwitchableLedger {
    /// Make every subsequent append fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// Make every subsequent append fail while attachments still land, or stop.
    pub(crate) fn set_appends_unavailable(&self, unavailable: bool) {
        self.appends_unavailable
            .store(unavailable, Ordering::SeqCst);
    }
}

impl OwnerLedger for SwitchableLedger {
    fn append<'a>(
        &'a self,
        owner: &'a OwnerId,
        event: LedgerEvent,
        recorded_at: Timestamp,
        signer: &'a dyn capsule_server::attestation::ReceiptSigner,
    ) -> LedgerFuture<'a, LedgerEntry> {
        if self.unavailable.load(Ordering::SeqCst)
            || self.appends_unavailable.load(Ordering::SeqCst)
        {
            return Box::pin(async {
                Err(LedgerError::Backend {
                    operation: "append",
                    detail: REFUSAL.to_owned(),
                })
            });
        }
        self.inner.append(owner, event, recorded_at, signer)
    }

    fn scan<'a>(&'a self, owner: &'a OwnerId) -> LedgerFuture<'a, LedgerScan> {
        self.inner.scan(owner)
    }

    fn owners(&self) -> LedgerFuture<'_, Vec<OwnerId>> {
        self.inner.owners()
    }

    /// Refused while the switch is on, as an append is: a ledger that cannot write cannot keep
    /// an attachment either.
    fn attach<'a>(
        &'a self,
        owner: &'a OwnerId,
        key: capsule_core::crypto::hash::Hash32,
        bytes: Vec<u8>,
    ) -> LedgerFuture<'a, ()> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Box::pin(async {
                Err(LedgerError::Backend {
                    operation: "attach",
                    detail: REFUSAL.to_owned(),
                })
            });
        }
        self.inner.attach(owner, key, bytes)
    }

    fn attachment<'a>(
        &'a self,
        owner: &'a OwnerId,
        key: &'a capsule_core::crypto::hash::Hash32,
    ) -> LedgerFuture<'a, Option<Vec<u8>>> {
        self.inner.attachment(owner, key)
    }
}

/// A quota ledger that can be made to fail on demand.
#[derive(Debug, Default)]
pub(crate) struct SwitchableQuota {
    inner: InMemoryQuota,
    unavailable: AtomicBool,
    /// Fail [`QuotaStore::charge`] alone, so a caller that reads usage first gets past the read.
    charges_unavailable: AtomicBool,
}

impl SwitchableQuota {
    /// A working ledger.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// Make every subsequent charge fail while reads keep answering, or stop.
    pub(crate) fn set_charges_unavailable(&self, unavailable: bool) {
        self.charges_unavailable
            .store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "quota",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl QuotaStore for SwitchableQuota {
    fn usage<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, StoredUsage> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.usage(user)
    }

    fn charge<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
        size: u64,
        at: Timestamp,
        limits: QuotaLimits,
    ) -> StoreFuture<'a, ChargeOutcome> {
        if self.is_down() || self.charges_unavailable.load(Ordering::SeqCst) {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.charge(user, address, size, at, limits)
    }

    fn release<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
    ) -> StoreFuture<'a, bool> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.release(user, address)
    }

    fn release_attribution<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> StoreFuture<'a, Option<(UserId, u64)>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.release_attribution(address)
    }

    fn suspend<'a>(&'a self, user: &'a UserId, at: Timestamp) -> StoreFuture<'a, bool> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.suspend(user, at)
    }

    fn unsuspend<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, bool> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.unsuspend(user)
    }
}

/// A membership store that can be made to fail on demand.
#[derive(Debug, Default)]
pub(crate) struct SwitchableMembership {
    inner: InMemoryMembership,
    unavailable: AtomicBool,
}

impl SwitchableMembership {
    /// A working store.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "membership",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl MembershipStore for SwitchableMembership {
    fn apply_roster(
        &self,
        roster: RosterRecord,
        members: Vec<(UserId, MemberRole)>,
    ) -> StoreFuture<'_, RosterOutcome> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.apply_roster(roster, members)
    }

    fn membership<'a>(
        &'a self,
        album: &'a AlbumId,
        user: &'a UserId,
    ) -> StoreFuture<'a, Membership> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.membership(album, user)
    }

    fn current_roster<'a>(&'a self, album: &'a AlbumId) -> StoreFuture<'a, Option<RosterRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.current_roster(album)
    }
}

/// An album store that can be made to fail on demand.
#[derive(Debug, Default)]
pub(crate) struct SwitchableAlbums {
    inner: InMemoryAlbums,
    unavailable: AtomicBool,
}

impl SwitchableAlbums {
    /// A working store.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "albums",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl AlbumStore for SwitchableAlbums {
    fn provision(&self, record: AlbumRecord) -> StoreFuture<'_, ProvisionOutcome> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.provision(record)
    }

    fn read<'a>(&'a self, album: &'a AlbumId) -> StoreFuture<'a, Option<AlbumRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.read(album)
    }

    fn begin_upgrade<'a>(
        &'a self,
        album: &'a AlbumId,
        owner: &'a OwnerId,
        quiescence: capsule_server::album::UpgradeQuiescence,
        now: Timestamp,
    ) -> StoreFuture<'a, capsule_server::album::UpgradeOutcome> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.begin_upgrade(album, owner, quiescence, now)
    }

    fn end_upgrade<'a>(
        &'a self,
        album: &'a AlbumId,
        owner: &'a OwnerId,
        intent_id: Uuid,
        now: Timestamp,
    ) -> StoreFuture<'a, capsule_server::album::UpgradeOutcome> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.end_upgrade(album, owner, intent_id, now)
    }
}

/// A capability store — and therefore a revocation list — that can be made to fail on demand.
///
/// Delegates to the real in-memory store, so the failing case and the working case differ in
/// exactly one thing. It exists because `503` on the published record is a *claim*: the
/// endpoint refuses to serve an empty list on a storage failure, since an empty list is the
/// strongest statement the record can make and serving it during an outage would silently
/// un-revoke every token a peer holds. A status nothing can reach is a status nothing proves.
///
/// One object behind two ports, exactly as `boot` wires it: discovery reads it as the list and
/// federation writes it as the store, so a revocation the federation layer records is the one
/// `revoked-jti` publishes.
#[derive(Debug)]
pub(crate) struct SwitchableRevocations {
    inner: InMemoryCapabilities,
    unavailable: AtomicBool,
    writes_unavailable: AtomicBool,
}

impl SwitchableRevocations {
    /// A working store reading `clock` for pruning.
    pub(crate) fn new(clock: Arc<ManualClock>) -> Self {
        Self {
            inner: InMemoryCapabilities::new(clock),
            unavailable: AtomicBool::new(false),
            writes_unavailable: AtomicBool::new(false),
        }
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// Make every subsequent *write* fail while reads keep answering, or stop.
    ///
    /// The one seam a route-level `500` can be reached through: a store that cannot be **read**
    /// refuses the credential in the authenticator, which can render only `401`, so a whole
    /// outage never reaches a handler. A store that answers `find` and refuses `refresh` is the
    /// partial failure the coded `500` exists for.
    pub(crate) fn set_writes_unavailable(&self, unavailable: bool) {
        self.writes_unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "revocations",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }

    fn writes_down(&self) -> bool {
        self.is_down() || self.writes_unavailable.load(Ordering::SeqCst)
    }
}

impl RevocationList for SwitchableRevocations {
    fn revoke(&self, token: RevokedToken) -> RevokeFuture<'_> {
        if self.writes_down() {
            return Box::pin(async { Self::refuse().map_err(Into::into) });
        }
        self.inner.revoke(token)
    }

    fn published(&self) -> StoreFuture<'_, PublishedRevocations> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.published()
    }
}

impl CapabilityStore for SwitchableRevocations {
    fn issue(&self, record: CapabilityRecord) -> StoreFuture<'_, ()> {
        if self.writes_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.issue(record)
    }

    fn find<'a>(&'a self, jti: &'a str) -> StoreFuture<'a, Option<CapabilityRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.find(jti)
    }

    fn live<'a>(
        &'a self,
        filter: &'a CapabilityFilter,
        now: Timestamp,
    ) -> StoreFuture<'a, Vec<CapabilityRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.live(filter, now)
    }

    fn revoke_issued<'a>(&'a self, jti: &'a str, at: Timestamp) -> StoreFuture<'a, RevokeOutcome> {
        if self.writes_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.revoke_issued(jti, at)
    }

    fn refresh<'a>(
        &'a self,
        predecessor: &'a str,
        successor: CapabilityRecord,
        at: Timestamp,
    ) -> StoreFuture<'a, RefreshOutcome> {
        if self.writes_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.refresh(predecessor, successor, at)
    }
}

/// A device-directory store that can be made to fail on demand.
///
/// Delegates to a real in-memory store, so the failing case and the working case differ in
/// exactly one thing.
#[derive(Debug, Default)]
pub(crate) struct SwitchableDirectories {
    inner: InMemoryDeviceDirectory,
    unavailable: AtomicBool,
}

impl SwitchableDirectories {
    /// A working store.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "device-directory",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl DeviceDirectoryStore for SwitchableDirectories {
    fn publish(&self, record: PublishedDirectory) -> StoreFuture<'_, PublishOutcome> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.publish(record)
    }

    fn fetch<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Option<PublishedDirectory>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.fetch(user)
    }
}

impl SwitchableIndex {
    /// A working index.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The receipt log the index appends finalization's custody receipts to, which the
    /// attestation context must serve from.
    pub(crate) fn receipts(&self) -> Arc<InMemoryReceipts> {
        self.inner.receipts().clone()
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "asset-index",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl AssetIndex for SwitchableIndex {
    fn reserve(&self, asset: PendingAsset) -> IndexFuture<'_, Reservation> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.reserve(asset)
    }

    fn read<'a>(&'a self, asset: &'a AssetId) -> IndexFuture<'a, Option<AssetRow>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.read(asset)
    }

    fn record_blob<'a>(
        &'a self,
        asset: &'a AssetId,
        blob: BlobRecord,
    ) -> IndexFuture<'a, BlobOutcome> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.record_blob(asset, blob)
    }

    fn record_blob_with_receipt<'a>(
        &'a self,
        asset: &'a AssetId,
        blob: BlobRecord,
        custody: Custody<'a>,
    ) -> IndexFuture<'a, Receipted<BlobOutcome>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.record_blob_with_receipt(asset, blob, custody)
    }

    fn tombstone<'a>(
        &'a self,
        asset: &'a AssetId,
        at: Timestamp,
    ) -> IndexFuture<'a, Option<AssetRow>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.tombstone(asset, at)
    }

    fn apply_op(&self, op: LifecycleOp) -> IndexFuture<'_, OpOutcome> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.apply_op(op)
    }

    fn apply_op_with_receipt<'a>(
        &'a self,
        op: LifecycleOp,
        custody: Custody<'a>,
    ) -> IndexFuture<'a, Receipted<OpOutcome>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.apply_op_with_receipt(op, custody)
    }

    fn rows<'a>(
        &'a self,
        after: Option<&'a AssetId>,
        limit: usize,
    ) -> IndexFuture<'a, Vec<AssetRow>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.rows(after, limit)
    }

    fn set_hold<'a>(
        &'a self,
        asset: &'a AssetId,
        hold: Option<ServingHold>,
    ) -> IndexFuture<'a, HoldOutcome> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.set_hold(asset, hold)
    }

    fn reference_count<'a>(&'a self, address: &'a ContentAddress) -> IndexFuture<'a, u64> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.reference_count(address)
    }

    fn tombstoned(&self, limit: usize) -> IndexFuture<'_, Vec<AssetRow>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.tombstoned(limit)
    }

    fn purge<'a>(&'a self, asset: &'a AssetId, now: Timestamp) -> IndexFuture<'a, PurgeOutcome> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.purge(asset, now)
    }

    fn find_references<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> IndexFuture<'a, Vec<capsule_server::index::BlobReference>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.find_references(address)
    }

    fn find_by_address<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
        address: &'a ContentAddress,
    ) -> IndexFuture<'a, Option<AssetId>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.find_by_address(owner, album, address)
    }

    fn feed_page<'a>(
        &'a self,
        owner: &'a OwnerId,
        after: u64,
        limit: usize,
    ) -> IndexFuture<'a, Vec<FeedEntry>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.feed_page(owner, after, limit)
    }

    fn head_seq<'a>(&'a self, owner: &'a OwnerId) -> IndexFuture<'a, u64> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.head_seq(owner)
    }

    fn album_feed_page<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
        after: u64,
        limit: usize,
    ) -> IndexFuture<'a, Vec<FeedEntry>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.album_feed_page(owner, album, after, limit)
    }

    fn album_head_seq<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
    ) -> IndexFuture<'a, u64> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.album_head_seq(owner, album)
    }

    fn sync_epoch<'a>(&'a self, owner: &'a OwnerId) -> IndexFuture<'a, u64> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.sync_epoch(owner)
    }

    fn bump_sync_epoch<'a>(&'a self, owner: &'a OwnerId) -> IndexFuture<'a, u64> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.bump_sync_epoch(owner)
    }

    fn ledger_head<'a>(&'a self, owner: &'a OwnerId) -> IndexFuture<'a, Option<LedgerHead>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.ledger_head(owner)
    }

    fn anchored_owners(&self) -> IndexFuture<'_, Vec<OwnerId>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.anchored_owners()
    }

    fn record_ledger_head<'a>(
        &'a self,
        owner: &'a OwnerId,
        head: LedgerHead,
    ) -> IndexFuture<'a, ()> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.record_ledger_head(owner, head)
    }

    fn reset_ledger_head<'a>(
        &'a self,
        owner: &'a OwnerId,
        head: Option<LedgerHead>,
    ) -> IndexFuture<'a, ()> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.reset_ledger_head(owner, head)
    }

    fn restore(
        &self,
        row: capsule_server::index::AssetRow,
    ) -> IndexFuture<'_, capsule_server::index::RestoreOutcome> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.restore(row)
    }
}

/// The fixture's client: Kynos's in-process `TestClient`, sending the protocol handshake.
///
/// Every request a real client makes carries `X-Capsule-Protocol` — the SDK sets it as a
/// default header on its transport — so the fixture does the same, once, here, rather than at
/// every one of the suite's several hundred request sites. A case about the handshake itself
/// overrides the header (a later `header` call replaces an earlier one) or reaches for
/// [`Client::raw`] to send none at all; the two are the only ways a request leaves without it,
/// which is what keeps "the gate refused this" a deliberate assertion rather than a fixture
/// accident.
///
/// Deliberately not `Deref` to the inner client: a function taking `&TestClient<App>` would
/// then accept this and silently drive the router without the handshake.
pub(crate) struct Client {
    inner: TestClient<App>,
}

impl Client {
    pub(crate) fn new(inner: TestClient<App>) -> Self {
        Self { inner }
    }

    /// The bare client, for a request that must **not** carry the handshake.
    pub(crate) fn raw(&self) -> &TestClient<App> {
        &self.inner
    }

    fn handshake(request: TestRequest<'_, App>) -> TestRequest<'_, App> {
        request.header("x-capsule-protocol", PROTOCOL_VERSION)
    }

    pub(crate) fn get(&self, path: &str) -> TestRequest<'_, App> {
        Self::handshake(self.inner.get(path))
    }

    pub(crate) fn post(&self, path: &str) -> TestRequest<'_, App> {
        Self::handshake(self.inner.post(path))
    }

    pub(crate) fn put(&self, path: &str) -> TestRequest<'_, App> {
        Self::handshake(self.inner.put(path))
    }

    pub(crate) fn patch(&self, path: &str) -> TestRequest<'_, App> {
        Self::handshake(self.inner.patch(path))
    }

    pub(crate) fn delete(&self, path: &str) -> TestRequest<'_, App> {
        Self::handshake(self.inner.delete(path))
    }

    pub(crate) fn head(&self, path: &str) -> TestRequest<'_, App> {
        Self::handshake(self.inner.head(path))
    }

    /// A request with any method, for a walk driven by the document rather than by a verb.
    pub(crate) fn method(&self, method: kynos::http::Method, path: &str) -> TestRequest<'_, App> {
        Self::handshake(self.inner.method(method, path))
    }

    /// Every response this client observed was one the description predicts.
    pub(crate) fn assert_conformance(&self) {
        self.inner.assert_conformance();
    }

    /// Every response the description predicts was produced through this client.
    pub(crate) fn assert_declared_responses_covered(&self) {
        self.inner.assert_declared_responses_covered();
    }
}

/// A built server, plus handles on everything behind it.
///
/// The handles matter: an assertion about a session is made against the store the server just
/// wrote to, not against a second reading of the response body.
pub(crate) struct Fixture {
    /// The in-process client, sending the handshake on every request. No socket, no port.
    pub(crate) client: Client,
    /// The context the client drives, for the one case that has to serve it on a socket.
    app: App,
    /// The store the server opened its sessions in.
    pub(crate) sessions: Arc<SwitchableSessions>,
    /// The directory the server authenticated against.
    pub(crate) accounts: Arc<InMemoryAccounts>,
    /// The signer the server minted with — the *same* one, so a test can mint a token the
    /// server will accept, or one it must not.
    pub(crate) tokens: Arc<SessionTokens>,
    /// The clock every record and every deadline is stamped from.
    pub(crate) clock: Arc<ManualClock>,
    /// The store the server opened its upload sessions in.
    pub(crate) uploads: Arc<SwitchableUploads>,
    /// The store the server staged and committed bytes through.
    pub(crate) blobs: Arc<SwallowingBlobs>,
    /// The album and device facts invariants 6 and 7 were decided against.
    pub(crate) authority: Arc<TestAuthority>,
    /// The durable asset index the feed reads from.
    pub(crate) index: Arc<SwitchableIndex>,
    /// The crash seam wrapped around it, disarmed unless a case arms it (E2E case 11).
    ///
    /// Always in the chain rather than swapped in by a second constructor: a decorator that only
    /// some fixtures carried would be a second wiring for the tests that carry it, and this one
    /// delegates every call it is not armed for.
    pub(crate) index_fault: Arc<fault::CrashBeforeCommit>,
    /// The cursor codec the server mints with — the *same* one, so a test can mint a cursor
    /// the server will accept, or one it must not.
    pub(crate) cursors: Arc<CursorCodec>,
    /// The published device directories the server reads and writes.
    pub(crate) directories: Arc<SwitchableDirectories>,
    /// The albums the server has provisioned.
    pub(crate) albums: Arc<SwitchableAlbums>,
    /// The album rosters the server holds, and who they make a member.
    pub(crate) members: Arc<SwitchableMembership>,
    /// The quota ledger the server charges against.
    pub(crate) quotas: Arc<SwitchableQuota>,
    /// The collector's marks, which is where `retrievable` diverges from `stored`.
    pub(crate) marks: Arc<InMemoryCollection>,
    /// The append-only custody-receipt log.
    pub(crate) receipts: Arc<InMemoryReceipts>,
    /// The attestation key the server signs receipts with — the *same* one, so a test can
    /// verify a fetched receipt the way a client would.
    pub(crate) attestation_key: Arc<LocalAttestationKey>,
    /// The owner ledger finalization appends to — the *same* one, so a test can read and verify
    /// what was ledgered.
    pub(crate) ledger: Arc<SwitchableLedger>,
    /// The federation capabilities this server issued, and the revocations it publishes.
    pub(crate) revocations: Arc<SwitchableRevocations>,
    /// The peers this server has pinned or blocked.
    pub(crate) peers: Arc<InMemoryPeers>,
    /// Whether the server federates — the *same* switch it reads, so a case can turn the whole
    /// federation surface off for the requests it is about.
    pub(crate) federation: Arc<SwitchableFederation>,
    /// The capability codec the server mints with — the *same* one, over the *same* key as
    /// `tokens`, so a test can mint a capability the server will accept, or one it must not.
    pub(crate) codec: Arc<CapabilityCodec>,
    /// The single-use revoke-all challenges.
    pub(crate) challenges: Arc<SwitchableChallenges>,
    /// The account's wrapped master key.
    pub(crate) escrows: Arc<SwitchableEscrow>,
    /// The durable device-cohort map.
    pub(crate) cohorts: Arc<SwitchableCohorts>,
    /// The pending cross-device enrollment codes.
    pub(crate) enrollments: Arc<SwitchableEnrollments>,
    /// The enrollment relay channels.
    pub(crate) channels: Arc<SwitchableChannels>,
    /// The account's standing and moderation record.
    pub(crate) moderation: Arc<SwitchableModeration>,
    /// The public share links.
    pub(crate) shares: Arc<SwitchableShares>,
    /// The upload links and the inbox behind them.
    pub(crate) dropstore: Arc<SwitchableDrops>,
    /// The rate-limit counters.
    pub(crate) counters: Arc<InMemoryCounters>,
    /// The second-factor enrollments.
    pub(crate) totp: Arc<InMemoryTotp>,
    /// The code generator the server verifies with — the *same* one, so a case can compute the
    /// code an authenticator app would be showing rather than guessing at one.
    pub(crate) codes: Arc<TotpCodes>,
    /// The identity provider double, when the fixture was built with one.
    pub(crate) idp: Arc<SwitchableIdentityProvider>,
    /// The pending OIDC ceremonies.
    pub(crate) oidc_authorizations: Arc<SwitchableOidcAuthorizations>,
    /// The pending OIDC device authorizations (issue #635).
    pub(crate) oidc_devices: Arc<SwitchableOidcDevices>,
    /// The chunk-body read deadline both chunk routes read (#717).
    pub(crate) chunk_deadline: Arc<SwitchableChunkDeadline>,
}

impl Fixture {
    /// A server whose collaborators all work, with one account, one writable album and one
    /// directory device seeded.
    pub(crate) fn working() -> Self {
        // Unlimited, which is what a self-hosted deployment runs and what every case that is
        // not about quota wants.
        Self::with_quota(QuotaLimits::unlimited())
    }

    /// The same server, with a deployment's quota thresholds.
    pub(crate) fn with_quota(quota_limits: QuotaLimits) -> Self {
        Self::build(quota_limits, None, None, true)
    }

    /// The working server over a **real** identity provider adapter, for the cases that drive
    /// the wire against the mock provider in [`idp`]. `fixture.idp` is present but unused.
    pub(crate) fn with_identity_provider(provider: Arc<dyn IdentityProvider>) -> Self {
        Self::build(QuotaLimits::unlimited(), Some(provider), None, true)
    }

    /// The working server whose rate-limit counters hold at most `ceiling` keys **per
    /// partition**.
    ///
    /// The shipped ceilings are twenty thousand keys wide, which is the right number for a
    /// deployment and the wrong number for a wire test: saturating one over HTTP would be twenty
    /// thousand requests. The property a saturated partition has to have — `429` with the
    /// at-capacity code rather than a `500` that impersonates an outage — does not depend on how
    /// wide it is, so the tests that assert it shrink the partitions and spend two requests.
    pub(crate) fn with_counter_ceiling(ceiling: usize) -> Self {
        Self::build(QuotaLimits::unlimited(), None, Some(ceiling), true)
    }

    /// The same server on a deployment that does **not** federate: `FEDERATION_ENABLED` off,
    /// which is the default (#509).
    ///
    /// Built off rather than switched off, so a case about the default deployment never
    /// observes the server federating. [`Fixture::federation`] still turns it on, which is how
    /// a case mints a grant "while federation was on" and then asks what off does to it.
    pub(crate) fn without_federation() -> Self {
        Self::build(QuotaLimits::unlimited(), None, None, false)
    }

    fn build(
        quota_limits: QuotaLimits,
        provider: Option<Arc<dyn IdentityProvider>>,
        counter_ceiling: Option<usize>,
        federating: bool,
    ) -> Self {
        let clock = Arc::new(ManualClock::default());
        let sessions = Arc::new(SwitchableSessions::new(clock.clone()));
        let accounts = Arc::new(InMemoryAccounts::new());
        accounts.insert(EMAIL, PASSWORD, &user());
        // One key pair for both token types, as `boot` wires it: the capability a peer verifies
        // against `server-info`'s key is signed by the key that signs sessions.
        let der = signing_key_der();
        let tokens = Arc::new(signer_from(&der, clock.clone()));
        let codec = Arc::new(
            CapabilityCodec::from_pkcs8(&der, SERVER_ORIGIN, clock.clone())
                .expect("a key just generated parses"),
        );

        let uploads = Arc::new(SwitchableUploads::new(clock.clone()));
        let blobs = Arc::new(SwallowingBlobs::new());
        let authority = Arc::new(TestAuthority::new());
        // The album the suite uploads into, pinned to the protocol version core speaks — the
        // pin the request is compared against, never the request's own value.
        authority.allow_album(&owner(), &album(), PROTOCOL_VERSION);
        // The device the suite's manifests are written by, admitted at the clock's own zero so
        // every manifest timestamp the suite writes is at or after it.
        authority.add_device(&user(), device(), clock.now());

        let index = Arc::new(SwitchableIndex::new());
        let index_fault = Arc::new(fault::CrashBeforeCommit::new(index.clone()));
        let cursors = Arc::new(CursorCodec::new(&CURSOR_KEY));
        let directories = Arc::new(SwitchableDirectories::new());
        let albums = Arc::new(SwitchableAlbums::new());
        let members = Arc::new(SwitchableMembership::new());
        let quotas = Arc::new(SwitchableQuota::new());
        let marks = Arc::new(InMemoryCollection::new());
        // The log the index appends custody receipts to in finalization's one critical section
        // (#693): the attestation context serves from the same one.
        let receipts = index.receipts();
        // The attestation key is generated per fixture and *distinct from* the token signer, as
        // the design requires: a receipt that verified under the operational key would let
        // anything holding that key manufacture custody evidence.
        let attestation_key = Arc::new(LocalAttestationKey::new(
            SERVER_ORIGIN,
            capsule_core::crypto::keys::HybridSigningKey::generate(),
        ));
        let ledger = Arc::new(SwitchableLedger::default());
        let revocations = Arc::new(SwitchableRevocations::new(clock.clone()));
        let peers = Arc::new(InMemoryPeers::new());
        let federation = Arc::new(SwitchableFederation::new(federating));
        let chunk_deadline = Arc::new(SwitchableChunkDeadline::new(
            capsule_server::upload::policy::DEFAULT_CHUNK_READ_TIMEOUT,
        ));
        let challenges = Arc::new(SwitchableChallenges::new(clock.clone()));
        let escrows = Arc::new(SwitchableEscrow::new());
        let cohorts = Arc::new(SwitchableCohorts::new());
        let enrollments = Arc::new(SwitchableEnrollments::new(clock.clone()));
        let channels = Arc::new(SwitchableChannels::new(clock.clone()));
        let moderation = Arc::new(SwitchableModeration::new());
        let shares = Arc::new(SwitchableShares::new());
        let dropstore = Arc::new(SwitchableDrops::new());
        let counters = Arc::new(match counter_ceiling {
            Some(ceiling) => InMemoryCounters::new().with_ceiling(ceiling),
            None => InMemoryCounters::new(),
        });
        let totp = Arc::new(InMemoryTotp::new());
        let codes = Arc::new(TotpCodes::new("Capsule"));
        let idp = Arc::new(SwitchableIdentityProvider::new());
        let oidc_authorizations = Arc::new(SwitchableOidcAuthorizations::new(clock.clone()));
        let oidc_devices = Arc::new(SwitchableOidcDevices::new(clock.clone()));
        let provider: Arc<dyn IdentityProvider> = provider.unwrap_or_else(|| idp.clone());

        // One index behind both modules, which is what makes "upload it, then read it back off
        // the feed" a test of the server rather than of two disconnected doubles.
        let app = App::new(Modules {
            auth: AuthContext::new(AuthCollaborators {
                sessions: sessions.clone(),
                accounts: accounts.clone(),
                registry: accounts.clone(),
                profiles: accounts.clone(),
                passwords: accounts.clone(),
                challenges: challenges.clone(),
                cohorts: cohorts.clone(),
                tokens: tokens.clone(),
                clock: clock.clone(),
            }),
            // Every module reads the index **through** the crash seam, so an armed fault is a
            // property of the server rather than of one module's wiring — and a disarmed one is
            // a delegating pass-through, which is what every other case sees.
            upload: UploadContext::new(
                uploads.clone(),
                blobs.clone(),
                index_fault.clone(),
                authority.clone(),
                clock.clone(),
                UploadPolicy::default().with_chunk_read_deadline(chunk_deadline.clone()),
            ),
            sync: SyncContext::new(
                index_fault.clone(),
                blobs.clone(),
                cursors.clone(),
                albums.clone(),
                members.clone(),
            ),
            serve: ServeContext::new(
                index_fault.clone(),
                blobs.clone(),
                marks.clone(),
                uploads.clone(),
                capsule_server::serve::membership_reads(members.clone()),
            ),
            verify: VerifyContext::new(
                index_fault.clone(),
                blobs.clone(),
                marks.clone(),
                clock.clone(),
            ),
            directories: DeviceDirectoryContext::new(directories.clone(), clock.clone()),
            albums: AlbumContext::new(albums.clone(), clock.clone()),
            membership: MembershipContext::new(members.clone(), clock.clone()),
            quota: QuotaContext::new(quotas.clone(), clock.clone(), quota_limits),
            attestation: AttestationContext::new(
                receipts.clone(),
                attestation_key.clone(),
                Timestamp::UNIX_EPOCH,
            )
            // Anchored over the index, as `boot` wires it, so every producer's head is recorded.
            .with_ledger(Arc::new(capsule_server::ledger::AnchoredLedger::new(
                ledger.clone(),
                index.clone(),
            ))),
            discovery: DiscoveryContext::new(Arc::new(server_info(&tokens)), revocations.clone()),
            escrow: EscrowContext::new(escrows.clone(), clock.clone()),
            // On unless the case asked otherwise ([`Fixture::without_federation`]).
            federation: FederationContext::new(FederationCollaborators {
                codec: codec.clone(),
                capabilities: revocations.clone(),
                peers: peers.clone(),
                clock: clock.clone(),
                enabled: federation.clone(),
            }),
            enrollment: EnrollmentContext::new(
                enrollments.clone(),
                channels.clone(),
                clock.clone(),
            ),
            moderation: ModerationContext::new(moderation.clone()),
            share: ShareContext::new(shares.clone(), blobs.clone(), clock.clone()),
            drops: DropContext::new(
                dropstore.clone(),
                uploads.clone(),
                blobs.clone(),
                clock.clone(),
            ),
            counters: CounterContext::new(counters.clone(), clock.clone()),
            totp: TotpContext::new(totp.clone(), codes.clone()),
            oidc: OidcContext::new(OidcCollaborators {
                provider,
                authorizations: oidc_authorizations.clone(),
                devices: oidc_devices.clone(),
                accounts: accounts.clone(),
                clock: clock.clone(),
            }),
        });

        Self {
            client: Client::new(TestClient::new(
                capsule_server::service(app.clone()).expect("the router builds"),
            )),
            app,
            sessions,
            accounts,
            tokens,
            clock,
            uploads,
            blobs,
            authority,
            index,
            index_fault,
            cursors,
            directories,
            albums,
            members,
            quotas,
            marks,
            receipts,
            attestation_key,
            ledger,
            revocations,
            peers,
            federation,
            codec,
            challenges,
            escrows,
            cohorts,
            enrollments,
            channels,
            moderation,
            shares,
            dropstore,
            counters,
            totp,
            codes,
            idp,
            oidc_authorizations,
            oidc_devices,
            chunk_deadline,
        }
    }

    /// Just the application context, for a test that needs no handles on what is behind it.
    pub(crate) fn working_app() -> App {
        Self::working_context().0
    }

    /// The application context of *this* fixture, for a test that has to serve it on a socket.
    ///
    /// The fixture's own `TestClient` drives the router in-process, which is what every other
    /// case wants and is exactly what a client-library test must not do: the property there is
    /// that the **generated** client, over reqwest, over TCP, round-trips the real thing. This
    /// hands the same context to a listener, so the stores a case seeds through `fixture.index`
    /// are the stores that listener serves.
    pub(crate) fn app(&self) -> App {
        self.app.clone()
    }

    /// The application context, plus the clock it was built on.
    fn working_context() -> (App, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::default());
        let accounts = Arc::new(InMemoryAccounts::new());
        accounts.insert(EMAIL, PASSWORD, &user());
        let authority = Arc::new(TestAuthority::new());
        authority.allow_album(&owner(), &album(), PROTOCOL_VERSION);
        authority.add_device(&user(), device(), clock.now());

        let blobs = Arc::new(SwallowingBlobs::new());
        let index = Arc::new(SwitchableIndex::new());
        let receipts = index.receipts();
        let members = Arc::new(InMemoryMembership::new());
        let albums = Arc::new(SwitchableAlbums::new());
        let der = signing_key_der();
        let tokens = Arc::new(signer_from(&der, clock.clone()));
        let issued = Arc::new(SwitchableRevocations::new(clock.clone()));
        let app = App::new(Modules {
            auth: AuthContext::new(AuthCollaborators {
                sessions: Arc::new(SwitchableSessions::new(clock.clone())),
                accounts: accounts.clone(),
                registry: accounts.clone(),
                profiles: accounts.clone(),
                passwords: accounts,
                challenges: Arc::new(InMemoryChallenges::with_default_ttl(clock.clone())),
                cohorts: Arc::new(InMemoryCohorts::new()),
                tokens: tokens.clone(),
                clock: clock.clone(),
            }),
            upload: UploadContext::new(
                Arc::new(SwitchableUploads::new(clock.clone())),
                blobs.clone(),
                index.clone(),
                authority,
                clock.clone(),
                UploadPolicy::default(),
            ),
            sync: SyncContext::new(
                index.clone(),
                blobs.clone(),
                Arc::new(CursorCodec::new(&CURSOR_KEY)),
                albums.clone(),
                members.clone(),
            ),
            serve: ServeContext::new(
                index.clone(),
                blobs.clone(),
                Arc::new(InMemoryCollection::new()),
                Arc::new(SwitchableUploads::new(clock.clone())),
                capsule_server::serve::membership_reads(members.clone()),
            ),
            verify: VerifyContext::new(
                index,
                blobs.clone(),
                Arc::new(InMemoryCollection::new()),
                clock.clone(),
            ),
            directories: DeviceDirectoryContext::new(
                Arc::new(SwitchableDirectories::new()),
                clock.clone(),
            ),
            albums: AlbumContext::new(albums.clone(), clock.clone()),
            membership: MembershipContext::new(members, clock.clone()),
            quota: QuotaContext::new(
                Arc::new(SwitchableQuota::new()),
                clock.clone(),
                QuotaLimits::unlimited(),
            ),
            attestation: AttestationContext::new(
                receipts,
                Arc::new(LocalAttestationKey::new(
                    SERVER_ORIGIN,
                    capsule_core::crypto::keys::HybridSigningKey::generate(),
                )),
                Timestamp::UNIX_EPOCH,
            ),
            discovery: DiscoveryContext::new(Arc::new(server_info(&tokens)), issued.clone()),
            escrow: EscrowContext::new(Arc::new(SwitchableEscrow::new()), clock.clone()),
            federation: FederationContext::new(FederationCollaborators {
                codec: Arc::new(
                    CapabilityCodec::from_pkcs8(&der, SERVER_ORIGIN, clock.clone())
                        .expect("a key just generated parses"),
                ),
                capabilities: issued,
                peers: Arc::new(InMemoryPeers::new()),
                clock: clock.clone(),
                enabled: Arc::new(true),
            }),
            enrollment: EnrollmentContext::new(
                Arc::new(InMemoryEnrollments::new(clock.clone(), ENROLLMENT_CODE_TTL)),
                Arc::new(InMemoryChannels::new(clock.clone(), RELAY_CHANNEL_TTL)),
                clock.clone(),
            ),
            moderation: ModerationContext::new(Arc::new(InMemoryModeration::new())),
            share: ShareContext::new(
                Arc::new(InMemoryShares::new()),
                blobs.clone(),
                clock.clone(),
            ),
            drops: DropContext::new(
                Arc::new(InMemoryDrops::new()),
                Arc::new(SwitchableUploads::new(clock.clone())),
                blobs,
                clock.clone(),
            ),
            counters: CounterContext::new(Arc::new(InMemoryCounters::new()), clock.clone()),
            totp: TotpContext::new(
                Arc::new(InMemoryTotp::new()),
                Arc::new(TotpCodes::new("Capsule")),
            ),
            oidc: OidcContext::disabled(clock.clone()),
        });
        (app, clock)
    }

    /// Sign in with the seeded account and return the pair.
    pub(crate) async fn login(&self) -> capsule_server::routes::auth::TokenResponse {
        self.client
            .post("/v1/auth/login")
            .header("accept", "application/json")
            .json(&serde_json::json!({ "email": EMAIL, "password": PASSWORD }))
            .send()
            .await
            .assert_status(kynos::http::StatusCode::OK)
            .json()
    }

    /// The server's clock reading, for an assertion about an instant the server computed.
    pub(crate) fn now(&self) -> Timestamp {
        self.clock.now()
    }

    /// Spend every hit `budget` allows against `key`, so the next request charging it is refused.
    ///
    /// Through the store the server charges, at the server's own clock, so the window it opens
    /// is the one the next request meets.
    pub(crate) async fn spend_budget(
        &self,
        key: &capsule_server::counter::CounterKey,
        budget: capsule_server::counter::Budget,
    ) {
        use capsule_server::counter::CounterStore as _;
        for _ in 0..budget.limit {
            self.counters
                .hit(key, budget, self.clock.now())
                .await
                .expect("the in-memory counters answer");
        }
    }

    /// An access token for the seeded account, as the `Authorization` header carries it.
    pub(crate) async fn bearer(&self) -> String {
        format!("Bearer {}", self.login().await.access_token)
    }

    /// An access token for an account that is not the seeded one.
    ///
    /// Minted with the server's own signer rather than by signing in, because the account it
    /// names has no directory row — which is exactly the point: the credential is valid, and
    /// the session it asks about is somebody else's.
    ///
    /// **It opens a session record too**, and that is not incidental. The ledger is on the
    /// bearer scheme's path, so a token naming a session nothing holds is now refused at the
    /// door with a `401` — which would silently turn every cross-account isolation case into a
    /// test of the authenticator rather than of the handler it was written for. A second
    /// account has to be as real as the first for "another account cannot see this" to be the
    /// thing being asserted.
    pub(crate) async fn other_bearer(&self, user_id: &str) -> String {
        let session = SessionId::new("01937b7c-0000-7000-8000-00000000000f");
        let now = self.clock.now();
        self.sessions
            .open_session(SessionRecord {
                session_id: session.clone(),
                user_id: UserId::new(user_id),
                created_at: now,
                authenticated_at: now,
                last_active_at: now,
                user_agent: None,
                ip_address: None,
                cohort_hash: None,
                device_id: None,
            })
            .await
            .expect("the double opens a session");
        let issued = self
            .tokens
            .issue(&UserId::new(user_id), &session, SESSION_TTL)
            .expect("the signer mints");
        format!("Bearer {}", issued.access_token)
    }

    /// Open a session for `bytes` and return its identifier.
    ///
    /// Panics rather than returning a `Result`: a fixture that cannot open a session has
    /// nothing to test, and the failure a test wants to see is the one it is about to cause.
    pub(crate) async fn open_session(&self, bytes: &[u8], role: &str, bearer: &str) -> String {
        self.open_session_with(&create_request(&self.clock, bytes, role), bearer)
            .await
    }

    /// The same, over a body the caller has already mutated — the shape a case wants when it
    /// is varying a field the default body fixes, such as the album.
    pub(crate) async fn open_session_with(
        &self,
        request: &serde_json::Value,
        bearer: &str,
    ) -> String {
        let body: serde_json::Value = self
            .client
            .post("/v1/upload")
            .header("authorization", bearer)
            .json(request)
            .send()
            .await
            .assert_status(kynos::http::StatusCode::CREATED)
            .json();
        body["id"].as_str().expect("a session id").to_owned()
    }

    /// A well-formed `PATCH` of `payload` at `offset`.
    ///
    /// Every header the protocol requires is set — the handshake by the client, the rest here —
    /// so a test that wants one wrong overrides it: the later `header` call wins.
    pub(crate) fn chunk<'a>(
        &'a self,
        id: &str,
        offset: u64,
        payload: &[u8],
        bearer: &str,
    ) -> TestRequest<'a, App> {
        self.client
            .patch(&format!("/v1/upload/{id}"))
            .header("authorization", bearer)
            .header("x-capsule-offset", &offset.to_string())
            .header("x-capsule-checksum", &checksum(payload))
            .body("application/octet-stream", payload.to_vec())
    }

    /// Every entry of `owner`'s ledger, asserting the chain verifies to its end under the
    /// fixture's attestation key.
    pub(crate) async fn verified_ledger(&self, owner: &OwnerId) -> Vec<LedgerEntry> {
        use capsule_server::attestation::ReceiptSigner as _;

        let scan = self.ledger.scan(owner).await.expect("the ledger reads");
        let key = self.attestation_key.clone();
        let report = capsule_server::ledger::verify_chain(
            owner,
            &scan,
            &|id| (*id == key.key_id()).then(|| key.verifying_key()),
            None,
        );
        assert_eq!(report.fault, None, "the owner's ledger verifies to its end");
        assert_eq!(report.verified, scan.entries.len());
        scan.entries
    }
}

/// A peer server's operational key pair: the signer, and the raw thirty-two public bytes an
/// operator pins with `PeerStore::pin`.
///
/// Generated per call rather than fixed, so a case that means "a *different* peer's key" gets
/// one by asking again.
pub(crate) fn peer_keypair() -> (ring::signature::Ed25519KeyPair, [u8; 32]) {
    use ring::signature::KeyPair as _;

    let der = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
        .expect("a key generates");
    let pair = ring::signature::Ed25519KeyPair::from_pkcs8(der.as_ref()).expect("it parses");
    let public = pair
        .public_key()
        .as_ref()
        .try_into()
        .expect("an Ed25519 public key is thirty-two bytes");
    (pair, public)
}

/// A federated moderation report body, signed by `pair` exactly as a peer signs one.
///
/// Built through [`ReportClaim::signing_bytes`] rather than by re-encoding the JSON, so the
/// suite signs the same bytes the server verifies and a change to the signing contract fails as
/// a verification failure rather than as a silently-different test.
pub(crate) fn signed_report(
    pair: &ring::signature::Ed25519KeyPair,
    reporting_server: &str,
    reported_user: &str,
    asset_hash: &str,
    album: &AlbumId,
    reason: Option<&str>,
    reported_at: &str,
) -> serde_json::Value {
    sign_report(
        pair,
        &ReportClaim {
            reporting_server: reporting_server.to_owned(),
            recipient_server: SERVER_ORIGIN.to_owned(),
            // A fresh nonce per report, as a peer mints one: the server accepts each once.
            nonce: uuid::Uuid::now_v7().to_string(),
            reported_user: reported_user.to_owned(),
            asset_hash: asset_hash.to_owned(),
            album_id: album.as_str().to_owned(),
            reason: reason.map(str::to_owned),
            reported_at: reported_at.to_owned(),
        },
    )
}

/// The instant the suite's reports are signed at: the fixture clock's own start, so a report is
/// inside the server's freshness window until a case moves the clock.
pub(crate) const REPORTED_AT: &str = "1970-01-01T00:00:00Z";

/// `claim` as the body a peer sends, signed by `pair`.
pub(crate) fn sign_report(
    pair: &ring::signature::Ed25519KeyPair,
    claim: &ReportClaim,
) -> serde_json::Value {
    let signature = pair.sign(&claim.signing_bytes().expect("a claim encodes"));
    serde_json::json!({
        "reporting_server": claim.reporting_server,
        "recipient_server": claim.recipient_server,
        "nonce": claim.nonce,
        "reported_user": claim.reported_user,
        "asset_hash": claim.asset_hash,
        "album_id": claim.album_id,
        "reason": claim.reason,
        "reported_at": claim.reported_at,
        "signature": base64::engine::general_purpose::STANDARD.encode(signature.as_ref()),
    })
}

/// The key the suite's peers sign their capability presentations with.
///
/// One fixed pair for every peer the suite names: what a case under test is the binding of a
/// presentation to *a* pinned key, and a case that means "some other key" asks
/// [`peer_keypair`] for one.
pub(crate) fn request_keypair() -> (ring::signature::Ed25519KeyPair, [u8; 32]) {
    use ring::signature::KeyPair as _;

    let pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[0x5e; 32]).expect("a seed");
    let public = pair
        .public_key()
        .as_ref()
        .try_into()
        .expect("an Ed25519 public key is thirty-two bytes");
    (pair, public)
}

/// Pin [`request_keypair`] as `peer`'s operational key, as an operator would.
///
/// Pinned one probation period ago, so the peer is held to the established budgets the suite's
/// budget cases spend: a case about probation pins a peer now with [`pin_request_key_now`].
pub(crate) async fn pin_request_key(fixture: &Fixture, peer: &str) {
    let established = fixture
        .clock
        .now()
        .checked_sub(capsule_server::counter::budgets::PEER_PROBATION)
        .expect("the fixture clock is a probation period past the minimum instant");
    pin_request_key_at(fixture, peer, established).await;
}

/// Pin [`request_keypair`] as `peer`'s operational key now, so the peer is on probation.
pub(crate) async fn pin_request_key_now(fixture: &Fixture, peer: &str) {
    pin_request_key_at(fixture, peer, fixture.clock.now()).await;
}

async fn pin_request_key_at(fixture: &Fixture, peer: &str, at: jiff::Timestamp) {
    use capsule_server::federation::PeerStore as _;

    fixture
        .peers
        .pin(
            &capsule_server::federation::PeerId::new(peer),
            request_keypair().1,
            at,
        )
        .await
        .expect("the peer store pins");
}

/// The `X-Capsule-Request-Signature` a peer holding [`request_keypair`] sends with `bearer` on
/// `method target`, signed now on the fixture's clock with a fresh nonce.
///
/// Built through the server's own `RequestClaim`, so the suite signs the bytes the server
/// verifies and a change to the format fails as a refusal rather than as a different test.
pub(crate) fn request_signature(
    fixture: &Fixture,
    method: &str,
    target: &str,
    bearer: &str,
) -> String {
    request_signature_by(
        &request_keypair().0,
        fixture.clock.now(),
        method,
        target,
        bearer,
    )
}

/// As [`request_signature`], by `pair` at `at`.
pub(crate) fn request_signature_by(
    pair: &ring::signature::Ed25519KeyPair,
    at: jiff::Timestamp,
    method: &str,
    target: &str,
    bearer: &str,
) -> String {
    use capsule_server::federation::scheme::{RequestClaim, RequestSignature};

    let path = target.split_once('?').map_or(target, |(path, _)| path);
    let token = bearer.strip_prefix("Bearer ").unwrap_or(bearer);
    let nonce =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(uuid::Uuid::now_v7().as_bytes());
    let claim = RequestClaim::new(method, path, token, at.as_second(), &nonce);
    let signature = pair.sign(&claim.signing_bytes().expect("a claim encodes"));
    RequestSignature {
        signed_at: claim.signed_at,
        nonce,
        signature: signature
            .as_ref()
            .try_into()
            .expect("an Ed25519 signature is sixty-four bytes"),
    }
    .to_header_value()
}

/// `hours` from the fixture's own clock, as an RFC 3339 instant.
///
/// The suite's clock starts at the Unix epoch, so a wall-clock literal in a request body is
/// decades out and refused; every deadline a case names is relative to this.
pub(crate) fn deadline(fixture: &Fixture, hours: i64) -> jiff::Timestamp {
    fixture.clock.now() + jiff::SignedDuration::from_hours(hours)
}

/// The protocol version the suite's manifests and sessions are written under.
pub(crate) const PROTOCOL_VERSION: &str = capsule_core::crypto::primitives::PROTOCOL_VERSION;

/// The album [`Fixture::working`] seeds as writable.
pub(crate) fn album() -> AlbumId {
    AlbumId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e60")
}

/// A second album under the same owner, for cases about album scoping.
///
/// Not seeded by [`Fixture::working`]: a case that wants it writable says so, so the album
/// gate stays the thing the case is asserting rather than fixture background.
pub(crate) fn second_album() -> AlbumId {
    AlbumId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e70")
}

/// The owner the seeded album belongs to — the seeded account, filing under itself.
pub(crate) fn owner() -> OwnerId {
    OwnerId::new(user().as_str())
}

/// The device [`Fixture::working`] seeds in the directory.
pub(crate) fn device() -> Uuid {
    Uuid::parse_str("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f").expect("the literal is a uuid")
}

/// The code an authenticator app would be showing for `user` at the fixture's current time.
///
/// Built from the constants `capsule_server::auth::totp` publishes rather than through a helper
/// on `TotpCodes`, which is deliberate twice over: a server has no business generating codes, and
/// reconstructing SHA-1 / six digits / thirty seconds here checks the interop contract every
/// authenticator app assumes instead of trusting one function to agree with itself.
pub(crate) fn totp_code(fixture: &Fixture, user: &UserId) -> String {
    let secret = fixture
        .totp
        .secret_of(user)
        .expect("the account has an enrollment");
    let bytes = totp_rs::Secret::Encoded(secret.expose_base32().to_owned())
        .to_bytes()
        .expect("the fixture's secret is base32");
    totp_rs::TOTP::new(
        totp_rs::Algorithm::SHA1,
        capsule_server::auth::totp::DIGITS,
        0,
        capsule_server::auth::totp::STEP_SECONDS,
        bytes,
        Some("Capsule".to_owned()),
        String::new(),
    )
    .expect("a usable secret")
    .generate(u64::try_from(fixture.clock.now().as_second()).expect("a post-epoch instant"))
}

// ===========================================================================================
// Upload helpers
// ===========================================================================================

/// A blob of exactly `len` bytes, filled with `marker`.
///
/// Opaque bytes as far as the server is concerned — it holds ciphertext it cannot read, so a
/// repeating ASCII fill is as representative as random noise and makes a failure legible.
pub(crate) fn payload(marker: u8, len: usize) -> Vec<u8> {
    vec![marker; len]
}

/// The SHA-256 of `bytes`, spelled as the wire spells it.
pub(crate) fn checksum(bytes: &[u8]) -> String {
    capsule_core::crypto::hash::hash_bytes(bytes).to_hex()
}

// ===========================================================================================
// Request ids
// ===========================================================================================

/// The fixed string [`masked_request_id`] writes in place of a problem's request id.
const MASKED_REQUEST_ID: &str = "00000000-0000-7000-8000-000000000000";

/// A problem body with its `request_id` member removed, after asserting it carried one.
///
/// Every problem names the request that produced it, so two refusals that must be one answer
/// differ in that member by construction — it is minted per request and says nothing about the
/// account, the album or the address being probed. An indistinguishability assertion compares
/// everything else.
pub(crate) fn without_request_id(mut problem: serde_json::Value) -> serde_json::Value {
    let removed = problem
        .as_object_mut()
        .expect("a problem is a JSON object")
        .remove(capsule_server::problem::REQUEST_ID_MEMBER);
    assert!(
        removed.as_ref().is_some_and(serde_json::Value::is_string),
        "every problem names its request: {problem}"
    );
    problem
}

/// A problem body's bytes with its request id overwritten by a fixed one of the same length.
///
/// The byte-for-byte form of [`without_request_id`]: everything around the id — member order,
/// spacing, every other value — is still compared exactly. An empty body (a `HEAD`'s) has no
/// member to mask and is returned as it is.
pub(crate) fn masked_request_id(bytes: &[u8]) -> Vec<u8> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let marker = format!("\"{}\":\"", capsule_server::problem::REQUEST_ID_MEMBER);
    let marker = marker.as_bytes();
    let start = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .map_or_else(
            || {
                panic!(
                    "every problem names its request: {}",
                    String::from_utf8_lossy(bytes)
                )
            },
            |at| at + marker.len(),
        );
    let end = start + MASKED_REQUEST_ID.len();
    assert!(
        bytes.get(end) == Some(&b'"'),
        "a request id is a hyphenated UUID: {}",
        String::from_utf8_lossy(bytes)
    );
    let mut masked = bytes.to_vec();
    masked[start..end].copy_from_slice(MASKED_REQUEST_ID.as_bytes());
    masked
}

/// A `POST /v1/upload` body for `bytes`, internally consistent in every field the gate compares.
///
/// Tests mutate the returned value to make exactly one thing wrong, which is what keeps a
/// rejection test about the invariant it names rather than about the fixture.
pub(crate) fn create_request(clock: &ManualClock, bytes: &[u8], role: &str) -> serde_json::Value {
    let hash = checksum(bytes);
    let timestamp = clock.now().to_string();
    // The metadata role is the one whose manifest must commit to this blob's own address
    // (invariant 25); every other role's commitment is to a different object.
    let metadata_blob_hash = if role == "metadata" {
        serde_json::Value::String(hash.clone())
    } else {
        serde_json::Value::Null
    };
    // A derivative declares its `(role, format)` pair, which the gate checks against the closed
    // still set; the format is the one this build produces, and rides `content_type` too.
    let content_type = match role {
        "original" => "image/jpeg",
        "derivative" => DERIVATIVE_FORMAT,
        _ => "application/octet-stream",
    };

    let mut request = serde_json::json!({
        "size": bytes.len(),
        "hash": hash,
        "content_type": content_type,
        "crypto_suite_id": capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
        "protocol_version": PROTOCOL_VERSION,
        "blob_role": role,
        "album_id": album().as_str(),
        "manifest_envelope": {
            "crypto_suite_id": capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
            "protocol_version": PROTOCOL_VERSION,
            "album_id": album().as_str(),
            "file_id": "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61",
            "amk_version": 1,
            "ciphertext_hash": hash,
            "plaintext_size": bytes.len(),
            "chunk_size": 65_536,
            "key_mode": "derived",
            "metadata_blob_hash": metadata_blob_hash,
            "created_by_user": user().as_str(),
            "created_by_device": device().to_string(),
            "client_version": "capsule-cli/0.1.0",
            "timestamp": timestamp,
            "action": "create",
            "prior_provenance_hash": serde_json::Value::Null,
            "retention_until": serde_json::Value::Null,
        },
        "owner_id": serde_json::Value::Null,
        "intent_id": serde_json::Value::Null,
    });
    if role == "derivative" {
        request["manifest_envelope"]["derivative_role"] = "thumbnail".into();
        request["manifest_envelope"]["derivative_format"] = DERIVATIVE_FORMAT.into();
    }
    request
}

/// A really signed `create` for `asset` in the seeded album, by the seeded account's seeded
/// device, committing to `original` and `metadata`, in the provenance record a client stores it
/// in: what a client re-announces (#781).
///
/// The timestamp is the clock's, so the device the fixture published at construction precedes
/// it, as invariant 7 requires.
pub(crate) fn signed_create(
    clock: &ManualClock,
    asset: Uuid,
    original: &[u8],
    metadata: &[u8],
) -> Vec<u8> {
    let user = Uuid::parse_str(user().as_str()).expect("the user is a uuid");
    signed_create_by(clock, user, asset, original, metadata)
}

/// [`signed_create`], signed as `created_by` rather than the seeded account.
pub(crate) fn signed_create_by(
    clock: &ManualClock,
    created_by: Uuid,
    asset: Uuid,
    original: &[u8],
    metadata: &[u8],
) -> Vec<u8> {
    use capsule_core::crypto::hash::hash_bytes;
    use capsule_core::crypto::keys::{AmkVersion, HybridSigningKey};
    use capsule_core::crypto::provenance::{
        ASSET_MANIFEST_VERSION, Action, KeyMode, ManifestCore, ProvenanceRecord,
    };

    let core = ManifestCore {
        version: ASSET_MANIFEST_VERSION.to_owned(),
        crypto_suite_id: capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
        protocol_version: PROTOCOL_VERSION.to_owned(),
        file_id: asset,
        album_id: Uuid::parse_str(album().as_str()).expect("the album is a uuid"),
        amk_version: AmkVersion(0),
        ciphertext_hash: hash_bytes(original),
        plaintext_size: original.len() as u64,
        chunk_size: 65_536,
        nonce_prefix: [0; 7],
        key_mode: KeyMode::Derived,
        wrapped_file_key: None,
        metadata_blob_hash: Some(hash_bytes(metadata)),
        created_by_user: created_by,
        created_by_device: device(),
        client_version: "capsule-cli/0.1.0".to_owned(),
        timestamp: clock.now().to_string(),
        action: Action::Create,
        prior_provenance_hash: None,
        upgraded_from: None,
        unknown: BTreeMap::new(),
        retention_until: None,
    };
    let signed = core
        .sign(
            &HybridSigningKey::from_seed64(&[1; 64]),
            &HybridSigningKey::from_seed64(&[2; 64]),
        )
        .expect("a manifest signs");
    capsule_core::cbor::to_canonical_vec(&ProvenanceRecord::new(asset, signed, None))
        .expect("a record encodes")
}

/// The still format a derivative [`create_request`] declares: the one this build encodes.
const DERIVATIVE_FORMAT: &str = "image/jxl";

/// The account [`Fixture::working`] seeds.
pub(crate) fn user() -> UserId {
    UserId::new("01937b7c-0000-7000-8000-000000000001")
}

/// A signer over a freshly generated Ed25519 key pair.
///
/// Generated rather than read from a checked-in PEM: a private key in the repository is a
/// private key somebody eventually reuses.
/// The public record this server serves, over the key its own tokens verify under.
///
/// The signing key is read out of the signer rather than passed alongside it, which is the
/// invariant `ServerInfo` exists to hold: a published key that is only *usually* the signing
/// key fails silently here and totally on the peer checking a capability token.
pub(crate) fn server_info(tokens: &SessionTokens) -> ServerInfo {
    ServerInfo::new(
        SERVER_ORIGIN,
        "https://capsule.test/v1",
        ProtocolWindow {
            min: PROTOCOL_VERSION.to_owned(),
            max: PROTOCOL_VERSION.to_owned(),
        },
        tokens.public_key().to_vec(),
    )
    // The fixture's provider double is configured, so the record says so — and a case can post
    // to the *published* OIDC endpoints as it does to the published login.
    .with_oidc()
}

pub(crate) fn signer(clock: Arc<ManualClock>) -> SessionTokens {
    signer_from(&signing_key_der(), clock)
}

/// A freshly generated PKCS#8 Ed25519 private key.
///
/// One of these backs both the session signer and the capability codec of a fixture, because
/// that is the one-key invariant `boot` holds: the key `server-info` publishes signs both.
pub(crate) fn signing_key_der() -> Vec<u8> {
    ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
        .expect("the platform can generate an Ed25519 key")
        .as_ref()
        .to_vec()
}

/// The session signer over `der`.
pub(crate) fn signer_from(der: &[u8], clock: Arc<ManualClock>) -> SessionTokens {
    SessionTokens::from_pkcs8(der, clock).expect("a key just generated parses")
}

/// A `POST /v1/upload` body for one member of a **replace** bundle.
///
/// `prior` is the chain head the manifest supersedes. `bundle` is `Some((original, metadata))`
/// on the manifest's own session and `None` on the others — which is the shape of the rule: a
/// replace mutates an asset that is already visible, so it is applied as one act when its
/// manifest lands, and only the manifest has to be able to name what the asset will hold.
pub(crate) fn replace_request(
    clock: &ManualClock,
    bytes: &[u8],
    role: &str,
    prior: &str,
    bundle: Option<(&str, &str)>,
) -> serde_json::Value {
    let mut body = create_request(clock, bytes, role);
    body["manifest_envelope"]["action"] = "replace".into();
    body["manifest_envelope"]["prior_provenance_hash"] = prior.into();
    if let Some((original, metadata)) = bundle {
        body["manifest_envelope"]["original_blob_hash"] = original.into();
        body["manifest_envelope"]["metadata_blob_hash"] = metadata.into();
    }
    body
}

/// A signed device directory holding one device, whose DSK is `dsk`.
///
/// The empty directory `signed_directory_by` produces is enough for the identity-anchor check and
/// not enough for an upgrade proposal, which is verified against the *proposing device's* DSK.
pub(crate) fn signed_directory_with_device(
    ik: &HybridSigningKey,
    version: u64,
    device_id: Uuid,
    dsk: &HybridSigningKey,
    added_at: &str,
) -> Vec<u8> {
    use capsule_core::crypto::keys::{DeviceDirectory, DeviceEntry, DirectoryCore};

    let directory: DeviceDirectory = DirectoryCore {
        user_id: Uuid::parse_str(user().as_str()).expect("the seeded account id is a uuid"),
        directory_version: version,
        updated_at: "2026-01-01T00:00:00Z".to_owned(),
        devices: vec![DeviceEntry {
            device_id,
            dsk_public: dsk.verifying_key(),
            dek_public: None,
            added_at: added_at.to_owned(),
            revoked_at: None,
            unknown: Default::default(),
        }],
        unknown: Default::default(),
    }
    .sign(ik);
    capsule_core::cbor::to_canonical_vec(&directory).expect("a directory serializes")
}

/// A signed album roster, base64-encoded as `PUT /v1/albums/{album_id}/roster` carries it.
///
/// Attested by the seeded account through `capsule_core::crypto::membership` — the same types
/// the server verifies with — so a fixture cannot pass while the two ends disagree about what
/// was signed.
pub(crate) fn signed_roster(
    dsk: &HybridSigningKey,
    device_id: Uuid,
    album: &AlbumId,
    roster_version: u64,
    amk_epoch: u32,
    members: &[(&str, MemberRole)],
) -> String {
    use capsule_core::crypto::keys::AmkVersion;
    use capsule_core::crypto::membership::{AlbumRoster, RosterMember, SignedAlbumRoster};

    let roster = AlbumRoster {
        album_id: Uuid::parse_str(album.as_str()).expect("an album id is a uuid"),
        roster_version,
        amk_epoch: AmkVersion(amk_epoch),
        attested_by_user: Uuid::parse_str(user().as_str())
            .expect("the seeded account id is a uuid"),
        attested_by_device: device_id,
        attested_at: "2026-09-02T00:00:00Z".to_owned(),
        members: members
            .iter()
            .map(|(user_id, role)| RosterMember {
                user_id: Uuid::parse_str(user_id).expect("a member id is a uuid"),
                role: *role,
            })
            .collect(),
    };
    let signed = SignedAlbumRoster::sign(roster, dsk).expect("a roster signs");
    base64::engine::general_purpose::STANDARD
        .encode(capsule_core::cbor::to_canonical_vec(&signed).expect("a signed roster serializes"))
}

/// A signed upgrade intent, as the proposing admin device's client would produce it.
///
/// Built through `capsule_core::crypto::upgrade` — the *same* types the server verifies with —
/// because a fixture that assembled the signed bytes itself would pass while the two ends
/// disagreed about what was signed.
pub(crate) fn signed_upgrade_intent(
    dsk: &HybridSigningKey,
    device_id: Uuid,
    intent_id: Uuid,
    to_protocol_version: &str,
    deadline_secs: u64,
) -> Vec<u8> {
    use capsule_core::crypto::upgrade::{SignedUpgradeIntent, UpgradeIntent};

    let intent = UpgradeIntent {
        intent_id,
        from_protocol_version: PROTOCOL_VERSION.to_owned(),
        to_protocol_version: to_protocol_version.to_owned(),
        from_suite_id: capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
        to_suite_id: capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
        proposer_user: Uuid::parse_str(user().as_str()).expect("the seeded account id is a uuid"),
        proposer_device: device_id,
        deadline_secs,
    };
    let proposer_sig = dsk.sign(&intent.signing_bytes().expect("an intent encodes"));
    capsule_core::cbor::to_canonical_vec(&SignedUpgradeIntent {
        intent,
        proposer_sig,
    })
    .expect("a signed intent serializes")
}

// ===========================================================================================
// Socket helpers (#717)
// ===========================================================================================

/// Serve `app` on an ephemeral loopback port with the HTTP/1 settings `serve` gives its
/// listener, and return the address.
///
/// The connection bounds are properties of the listener and the body read on a real socket — an
/// in-process `TestClient` hands the router a whole body and has no connection to time out — so
/// the cases about them speak raw HTTP/1 over TCP to this.
pub(crate) async fn serve_on_socket(
    app: App,
    header_read_timeout: std::time::Duration,
) -> std::net::SocketAddr {
    let service = capsule_server::service(app).expect("the router builds");
    let bound = kynos::server::Server::new(service)
        .bind(("127.0.0.1", 0))
        .http1(capsule_server::limits::http1(header_read_timeout))
        .prepare()
        .await
        .expect("an ephemeral port binds");
    let address = *bound
        .local_addrs()
        .first()
        .expect("a bound server has an address");
    tokio::spawn(async move {
        // The task lives as long as the test; a serve error after the test has finished is not
        // this test's to report.
        let _ = bound.serve().await;
    });
    address
}

/// The outer bound on any one socket read a case makes: far above every deadline a case sets,
/// so reaching it means the server held a connection it should have answered or closed.
pub(crate) const SOCKET_PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

/// Read whatever the server sends until it closes the connection.
///
/// A reset counts as closed: a server that stops reading a body and closes with bytes still
/// unread in its buffer resets the connection rather than ending it, and either way it is gone.
///
/// # Panics
///
/// When the server neither answers nor closes within [`SOCKET_PATIENCE`].
pub(crate) async fn read_until_closed(stream: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt as _;

    let mut received = Vec::new();
    let mut buffer = [0_u8; 4096];
    tokio::time::timeout(SOCKET_PATIENCE, async {
        loop {
            match stream.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => received.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => break,
                Err(error) => panic!("the socket reads: {error}"),
            }
        }
    })
    .await
    .expect("the server closed the connection within the patience bound");
    String::from_utf8_lossy(&received).into_owned()
}

/// Read one response head and its `Content-Length` body, leaving the connection open.
///
/// # Panics
///
/// When no whole response arrives within [`SOCKET_PATIENCE`].
pub(crate) async fn read_response(stream: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt as _;

    let mut received = Vec::new();
    let mut buffer = [0_u8; 4096];
    tokio::time::timeout(SOCKET_PATIENCE, async {
        loop {
            let read = stream.read(&mut buffer).await.expect("the socket reads");
            assert!(read > 0, "the server closed before a whole response");
            received.extend_from_slice(&buffer[..read]);
            let text = String::from_utf8_lossy(&received);
            if let Some(head_end) = text.find("\r\n\r\n") {
                let length = text[..head_end]
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if received.len() >= head_end + 4 + length {
                    break;
                }
            }
        }
    })
    .await
    .expect("a whole response arrived within the patience bound");
    String::from_utf8_lossy(&received).into_owned()
}
