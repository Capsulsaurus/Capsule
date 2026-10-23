//! Upload delegation: a device hands sealed, signed upload bundles to another device of the same
//! account, which uploads them later as an opaque courier (SSoT: [Peering § Delegation]).
//!
//! ```text
//! origin: seal + sign bundle ──hand-off──▶ delegate: verify, fsync, ack, upload ──▶ server
//!    ▲                                                                                │
//!    └──────── origin verifies custody of the hashes it computed itself ◀────────────┘
//! ```
//!
//! # What moves
//!
//! A [`HandOff`] is one asset's upload bundle exactly as the origin would upload it: every
//! blob's `POST /v1/upload` body ([`CreateUploadRequest`], built by [`crate::push`]) and the
//! blob's bytes, which are ciphertext or signed CBOR. The delegate never holds an album key or
//! plaintext for it, never re-signs, re-encrypts or re-derives anything, and uploads the requests
//! byte for byte under its **own** session. The server admits the manifest on the origin's
//! signature, never on who uploads it (`capsule-server::upload::signer`).
//!
//! A hand-off is per asset and all or nothing: the delegate verifies every member against the
//! signed manifest before it persists any of it, so it never uploads an incomplete bundle.
//!
//! # The origin ([`OriginLedger`])
//!
//! [`seal`] builds the hand-off and records the hashes the origin computed itself. The
//! [`OriginLedger`] holds each [`Delegation`] with a **monotonic** deadline: on expiry, when the
//! delegate is revoked, or when the clock the deadline was set on is gone (a reboot), the asset
//! is re-queued for the origin to upload itself or delegate again ([`OriginLedger::due`]). It is
//! released only through the verify-before-destroy gate over custody facts the origin fetched
//! with its own session for its own hashes, plus a read-back of its own copy
//! ([`OriginLedger::release`]); an acknowledgement is never custody, and nothing the delegate
//! relays is consulted. A signed delete of a handed-off asset is held until the create's custody
//! is confirmed ([`OriginLedger::hold_delete`]), so the server never sees a delete for an asset
//! it has not indexed.
//!
//! # The delegate
//!
//! [`accept_hand_off`] verifies a hand-off ([`verify_hand_off`]), persists it durably through a
//! [`DelegateInbox`] and only then returns the [`HandOffAck`]. [`upload_hand_off`] drives the
//! requests in ladder order under the delegate's own [`StagedScheduler`] (its own battery and
//! metering policy), and [`settle`] deletes the delegate's copy once custody of every blob it
//! sent is confirmed. A delegate copy is never a library asset. A held hand-off that can never
//! succeed — its origin revoked after acceptance, its bytes no longer verifying, or a server
//! refusal the delegate cannot act on — is discarded by [`dispose_failure`]; a transient failure
//! keeps it for the next run ([`DelegationError::is_permanent`]).
//!
//! # Transport
//!
//! The hand-off is a self-verifying byte string with a content address ([`HandOff::address`]),
//! so it rides any channel: the peering channel's ranged pull ([`super::pull_artifact`]), or a
//! file a CLI writes and another reads. Integrity never depends on the channel: the delegate
//! re-verifies every hash and the manifest's signature, the server verifies the signature again,
//! and the origin trusts only custody it fetched itself.
//!
//! [Peering § Delegation]: https://docs/design/peering/#delegation

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::{fmt, io};

use capsule_core::cbor;
use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::provenance::{DerivativeManifest, ProvenanceRecord};
use capsule_core::crypto::verify_asset::verify_device_signature;
use capsule_library::import::UploadTier;
use capsule_library::library::{
    BlobRole as CoreBlobRole, ReceiptExpectations, ReleaseDecision, ReleaseGate, RetainReason,
    StorageVerifier,
};
use capsule_library::lifecycle::UploadBundle;
use serde::{Deserialize, Serialize};
use tracing::instrument;
use uuid::Uuid;

use super::PeeringError;
use super::channel::{PinnedTrust, admit_device, admit_directory};
use super::session::DirectoryVersions;
use crate::push::{bundle_blobs, create_request};
use crate::staged::{StagedAsset, StagedScheduler, TierBlob};
use crate::upload::{BlobRole, CreateUploadRequest, UploadClient, UploadError, UploadOutcome};
use crate::verify::{HeldRelease, ReleaseClock, ReleaseCoordinator};

/// The hand-off container format this build writes, and the newest it reads.
pub const HAND_OFF_FORMAT: u16 = 1;

/// The bytes every hand-off starts with.
const MAGIC: &[u8; 8] = b"CAPSHOF1";

/// The largest header a hand-off may declare. The header is a few kilobytes of JSON (one upload
/// request per blob); the ceiling bounds a malicious length prefix.
pub const MAX_HAND_OFF_HEADER: usize = 1024 * 1024;

/// How long an origin waits, by default, for a delegated asset's custody before it re-queues
/// the upload itself.
pub const DEFAULT_HAND_OFF_DEADLINE_MS: u64 = 24 * 60 * 60 * 1000;

/// Why a hand-off could not be sealed, accepted, uploaded or settled.
#[derive(Debug, thiserror::Error)]
pub enum DelegationError {
    /// The device directory or a device in it was refused (unknown, revoked, rolled back,
    /// foreign).
    #[error(transparent)]
    Peering(#[from] PeeringError),
    /// The origin named itself as the delegate.
    #[error("a device cannot delegate to itself")]
    SelfDelegation,
    /// The bytes are not a hand-off this build reads.
    #[error("malformed hand-off: {0}")]
    Malformed(String),
    /// The hand-off is in a container format newer than this build reads.
    #[error("hand-off format {found} is newer than this build reads ({HAND_OFF_FORMAT})")]
    UnsupportedFormat {
        /// The format the container declares.
        found: u16,
    },
    /// The hand-off was sealed for another delegate.
    #[error("the hand-off is addressed to device {addressed}, not to this device")]
    WrongDelegate {
        /// The delegate the origin named.
        addressed: Uuid,
    },
    /// The bundle's protocol version is newer than this delegate speaks. The delegate refuses
    /// the hand-off and the origin keeps the upload.
    #[error("the bundle speaks protocol {bundle}; this device speaks up to {ours}")]
    ProtocolUnsupported {
        /// The bundle's `protocol_version`.
        bundle: String,
        /// The newest protocol this delegate speaks.
        ours: String,
    },
    /// A member of the bundle does not match what the signed manifest commits to, or its bytes
    /// do not hash to what its request declares. Carries what failed.
    #[error("the hand-off does not verify: {0}")]
    Tampered(String),
    /// Persisting, reading or removing the delegate's copy failed.
    #[error("delegate inbox: {0}")]
    Inbox(String),
    /// The upload itself failed.
    #[error(transparent)]
    Upload(#[from] UploadError),
    /// The origin ledger knows no delegation for the asset.
    #[error("no delegation is recorded for asset {0}")]
    UnknownDelegation(Uuid),
    /// An acknowledgement names another hand-off or another delegate than the one recorded.
    #[error("the acknowledgement does not match the recorded hand-off")]
    AckMismatch,
}

/// The server's `error.*` codes that refuse a hand-off's requests for good. The delegate sends
/// the origin's signed requests verbatim and can change none of them, so no later run of the
/// same hand-off can succeed:
///
/// - `error.upload.envelope_rejected`: finalization re-validated the manifest and refused it —
///   a create sealed under an album epoch the album has moved past (invariant 18), a signer
///   revoked or not the account's, a signature that does not verify, or a role conflict.
/// - `error.upload.envelope_mismatch`: a request contradicts its own manifest envelope.
/// - `error.upload.stale_revival`: the asset's chain has moved past the one the manifest
///   extends — a conflicting provenance only the origin can rebase.
pub const PERMANENT_UPLOAD_REFUSALS: [&str; 3] = [
    capsule_i18n::error_codes::UPLOAD_ENVELOPE_REJECTED,
    capsule_i18n::error_codes::UPLOAD_ENVELOPE_MISMATCH,
    capsule_i18n::error_codes::UPLOAD_STALE_REVIVAL,
];

impl DelegationError {
    /// The stable `error.*` code the server supplied, when there was one.
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::Upload(UploadError::Rejected { code, .. }) => code.as_deref(),
            Self::Upload(UploadError::UpgradeRequired { .. }) => {
                Some(capsule_i18n::error_codes::PROTOCOL_VERSION_UNSUPPORTED)
            }
            _ => None,
        }
    }

    /// Whether a hand-off the delegate holds, failing with this error, can never succeed, so
    /// [`dispose_failure`] discards the delegate's copy.
    ///
    /// Permanent: the origin is no longer an unrevoked device of the pinned directory (revoked
    /// or removed after the hand-off was accepted); the held bytes no longer verify (malformed,
    /// addressed to another device, tampered with); or the server refused the requests with one
    /// of [`PERMANENT_UPLOAD_REFUSALS`]. Everything else is transient and keeps the copy for
    /// the next run: this device's directory or trust state, a container or protocol newer
    /// than this build (an update reads it), the inbox, the network, an outage, quota, rate
    /// limits, and any other refusal.
    #[must_use]
    pub fn is_permanent(&self) -> bool {
        match self {
            Self::Peering(PeeringError::UnknownDevice(_) | PeeringError::RevokedDevice(_))
            | Self::Malformed(_)
            | Self::WrongDelegate { .. }
            | Self::Tampered(_) => true,
            Self::Upload(UploadError::Rejected {
                code: Some(code), ..
            }) => PERMANENT_UPLOAD_REFUSALS.contains(&code.as_str()),
            _ => false,
        }
    }
}

/// The ladder tier a blob role belongs to (transfer upload doc, §Upload Tiering).
#[must_use]
pub fn tier_of(role: BlobRole) -> UploadTier {
    match role {
        BlobRole::Provenance | BlobRole::Metadata | BlobRole::Backup => UploadTier::Index,
        BlobRole::DerivativeManifest | BlobRole::Derivative => UploadTier::Preview,
        BlobRole::Original => UploadTier::Original,
    }
}

// ─── The hand-off container ───────────────────────────────────────────────────

/// The hand-off's header: who handed what to whom, and each member's upload request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandOffHeader {
    /// The container format ([`HAND_OFF_FORMAT`]).
    pub format: u16,
    /// The asset the bundle uploads.
    pub asset_id: Uuid,
    /// The device that sealed and signed the bundle.
    pub origin_device: Uuid,
    /// The device the origin handed it to.
    pub delegate_device: Uuid,
    /// The bundle's protocol version — the one every request declares.
    pub protocol_version: String,
    /// Each member's `POST /v1/upload` body, in ladder order. The member's bytes follow the
    /// header in the same order, each exactly `request.size` long.
    pub members: Vec<CreateUploadRequest>,
}

/// One asset's sealed upload bundle, as it moves from the origin to the delegate.
#[derive(Clone)]
pub struct HandOff {
    /// The header.
    pub header: HandOffHeader,
    /// Each member's bytes, in header order.
    pub blobs: Vec<Vec<u8>>,
}

impl fmt::Debug for HandOff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HandOff")
            .field("header", &self.header)
            .field("bytes", &self.blobs.iter().map(Vec::len).sum::<usize>())
            .finish()
    }
}

impl HandOff {
    /// The container's bytes: the magic `CAPSHOF1`, the header's length (`u32`, big-endian), the
    /// header as JSON, then every member's bytes in header order.
    ///
    /// # Errors
    ///
    /// [`DelegationError::Malformed`] when the header does not serialize or exceeds
    /// [`MAX_HAND_OFF_HEADER`].
    pub fn encode(&self) -> Result<Vec<u8>, DelegationError> {
        let header = serde_json::to_vec(&self.header)
            .map_err(|error| DelegationError::Malformed(error.to_string()))?;
        if header.len() > MAX_HAND_OFF_HEADER {
            return Err(DelegationError::Malformed(
                "the header exceeds its ceiling".into(),
            ));
        }
        let length = u32::try_from(header.len())
            .map_err(|_| DelegationError::Malformed("the header exceeds u32".into()))?;
        let body: usize = self.blobs.iter().map(Vec::len).sum();
        let mut out = Vec::with_capacity(MAGIC.len() + 4 + header.len() + body);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(&header);
        for blob in &self.blobs {
            out.extend_from_slice(blob);
        }
        Ok(out)
    }

    /// Read a container back. Structure only: [`verify_hand_off`] decides whether to trust it.
    ///
    /// # Errors
    ///
    /// [`DelegationError::Malformed`] for anything that is not a well-formed container, and
    /// [`DelegationError::UnsupportedFormat`] for one a newer build wrote.
    pub fn decode(bytes: &[u8]) -> Result<Self, DelegationError> {
        let malformed = |what: &str| DelegationError::Malformed(what.to_owned());
        let rest = bytes
            .strip_prefix(MAGIC.as_slice())
            .ok_or_else(|| malformed("not a hand-off"))?;
        let (length, rest) = rest
            .split_first_chunk::<4>()
            .ok_or_else(|| malformed("truncated header length"))?;
        let length = u32::from_be_bytes(*length) as usize;
        if length > MAX_HAND_OFF_HEADER || length > rest.len() {
            return Err(malformed("header length out of bounds"));
        }
        let (header, mut body) = rest.split_at(length);
        let format = serde_json::from_slice::<serde_json::Value>(header)
            .ok()
            .and_then(|value| value.get("format").and_then(serde_json::Value::as_u64))
            .ok_or_else(|| malformed("the header names no format"))?;
        if format > u64::from(HAND_OFF_FORMAT) {
            return Err(DelegationError::UnsupportedFormat {
                found: u16::try_from(format).unwrap_or(u16::MAX),
            });
        }
        let header: HandOffHeader = serde_json::from_slice(header)
            .map_err(|error| DelegationError::Malformed(error.to_string()))?;
        let mut blobs = Vec::with_capacity(header.members.len());
        for member in &header.members {
            let size = usize::try_from(member.size).map_err(|_| malformed("member too large"))?;
            if size > body.len() {
                return Err(malformed("truncated member"));
            }
            let (blob, remaining) = body.split_at(size);
            blobs.push(blob.to_vec());
            body = remaining;
        }
        if !body.is_empty() {
            return Err(malformed("trailing bytes after the last member"));
        }
        Ok(Self { header, blobs })
    }

    /// The content address of an encoded container (SHA-256, lowercase hex): what an
    /// acknowledgement names and a ranged pull verifies.
    #[must_use]
    pub fn address(encoded: &[u8]) -> String {
        hash_bytes(encoded).to_hex()
    }
}

// ─── The origin ───────────────────────────────────────────────────────────────

/// One blob the origin handed off, as the origin itself computed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnBlob {
    /// The blob's role.
    pub role: BlobRole,
    /// The content address the origin computed.
    pub hash: Hash32,
    /// Its size.
    pub size: u64,
}

/// A sealed hand-off: the container to send and what the origin records about it.
#[derive(Debug, Clone)]
pub struct SealedHandOff {
    /// The encoded container.
    pub bytes: Vec<u8>,
    /// Its content address.
    pub address: String,
    /// The asset.
    pub asset_id: Uuid,
    /// The delegate it is addressed to.
    pub delegate: Uuid,
    /// Every blob, as the origin computed it.
    pub own: Vec<OwnBlob>,
    /// What releasing the asset's original waits on: the origin's own original and metadata
    /// hashes ([`HeldRelease::for_original`]).
    pub release: Vec<Hash32>,
}

/// Seal `bundle` for `delegate`: the full upload bundle exactly as the origin would upload it,
/// and the hashes the origin computed itself.
///
/// The delegate must be a device of `trust`'s directory that is not revoked, and not the device
/// that signed the bundle.
///
/// # Errors
///
/// [`DelegationError::SelfDelegation`], a [`PeeringError`] for a delegate the directory does not
/// admit, [`DelegationError::Inbox`] when the original cannot be read, or
/// [`DelegationError::Malformed`] when the container cannot be encoded.
#[instrument(skip_all, fields(asset_id = %bundle.asset_id, %delegate))]
pub fn seal(
    bundle: &UploadBundle,
    delegate: Uuid,
    trust: &PinnedTrust,
) -> Result<SealedHandOff, DelegationError> {
    if delegate == bundle.created_by_device {
        return Err(DelegationError::SelfDelegation);
    }
    admit_device(trust, delegate)?;
    let mut members = Vec::new();
    let mut blobs = Vec::new();
    let mut own = Vec::new();
    for (blob, hash) in bundle_blobs(bundle) {
        let bytes = blob
            .bytes
            .to_vec()
            .map_err(|error| DelegationError::Inbox(format!("read the bundle: {error}")))?;
        let computed = hash_bytes(&bytes);
        if computed.to_hex() != hash {
            // The bundle's own address disagrees with its bytes: the library is damaged, and
            // handing it off would only move the damage.
            return Err(DelegationError::Tampered(format!(
                "the origin's {:?} blob does not hash to its address",
                blob.role
            )));
        }
        own.push(OwnBlob {
            role: blob.role,
            hash: computed,
            size: bytes.len() as u64,
        });
        members.push(create_request(bundle, &blob, &hash));
        blobs.push(bytes);
    }
    let hand_off = HandOff {
        header: HandOffHeader {
            format: HAND_OFF_FORMAT,
            asset_id: bundle.asset_id,
            origin_device: bundle.created_by_device,
            delegate_device: delegate,
            protocol_version: bundle.protocol_version.clone(),
            members,
        },
        blobs,
    };
    let bytes = hand_off.encode()?;
    let address = HandOff::address(&bytes);
    let release = HeldRelease::for_original(bundle)
        .blobs
        .iter()
        .map(|expected| expected.ciphertext_hash)
        .collect();
    tracing::info!(
        %address,
        members = hand_off.blobs.len(),
        bytes = bytes.len(),
        "delegation: hand-off sealed"
    );
    Ok(SealedHandOff {
        bytes,
        address,
        asset_id: bundle.asset_id,
        delegate,
        own,
        release,
    })
}

/// Where a delegation stands on the origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationState {
    /// Sealed and handed over; the delegate has not acknowledged it yet.
    HandedOff,
    /// The delegate verified and persisted it. Not custody.
    Acknowledged,
    /// The origin takes the upload back: it uploads the asset itself or delegates again.
    Requeued,
    /// The server holds the asset, by the origin's own check: the original may be released.
    Released,
}

/// Why a delegation was re-queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequeueReason {
    /// The monotonic deadline passed without custody.
    DeadlineExpired,
    /// The delegate is no longer an unrevoked device of the directory.
    DelegateRevoked,
    /// The deadline was set on a monotonic clock that is gone (the device rebooted), so it can
    /// no longer be measured; the upload is taken back rather than trusted to a stale clock.
    ClockReset,
}

/// One delegated asset, as the origin records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delegation {
    /// The asset.
    pub asset_id: Uuid,
    /// The device it was handed to.
    pub delegate: Uuid,
    /// The hand-off's content address.
    pub hand_off: String,
    /// Every blob, as the origin computed it.
    pub own: Vec<OwnBlob>,
    /// The hashes releasing the original waits on (original and metadata), the origin's own.
    pub release: Vec<Hash32>,
    /// The monotonic clock the deadline is on (a boot identity).
    pub clock_epoch: String,
    /// The deadline, in that clock's milliseconds.
    pub deadline_ms: u64,
    /// Where it stands.
    pub state: DelegationState,
    /// Why it was re-queued, once it was.
    pub requeued_because: Option<RequeueReason>,
    /// Signed deletes held until the create's custody exists, in the order they were made.
    pub held_deletes: Vec<Vec<u8>>,
}

impl Delegation {
    /// The line an origin shows for the asset while it waits: the delegate it was handed to,
    /// awaiting the server.
    #[must_use]
    pub fn status_line(&self, delegate_name: &str) -> String {
        match self.state {
            DelegationState::HandedOff | DelegationState::Acknowledged => {
                format!("handed to {delegate_name}, awaiting server")
            }
            DelegationState::Requeued => "re-queued to upload from this device".to_owned(),
            DelegationState::Released => "held by the server".to_owned(),
        }
    }

    fn is_open(&self) -> bool {
        matches!(
            self.state,
            DelegationState::HandedOff | DelegationState::Acknowledged
        )
    }
}

/// A delegate's acknowledgement: it verified the hand-off and persisted it durably. Not custody.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandOffAck {
    /// The asset.
    pub asset_id: Uuid,
    /// The hand-off's content address, as the delegate computed it over what it received.
    pub hand_off: String,
    /// The acknowledging device.
    pub delegate_device: Uuid,
}

/// What [`OriginLedger::hold_delete`] did with a signed delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteDisposition {
    /// The asset's create has no confirmed custody yet: the delete is held, and
    /// [`OriginLedger::release`] hands it back once custody is confirmed.
    Held,
    /// No open delegation: send it now.
    SendNow(Vec<u8>),
}

/// What [`OriginLedger::release`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseOutcome {
    /// Custody is confirmed: the original may be released, and these held deletes may now be
    /// sent, in order.
    Released {
        /// The deletes held for the asset.
        deletes: Vec<Vec<u8>>,
    },
    /// Not yet: the original stays pinned.
    Retained(RetainReason),
}

/// The origin's record of every asset it handed off: pinned until its own custody check
/// passes, re-queued on a deadline or a revocation.
///
/// Serializable whole, so a client persists it beside its own state and reloads it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginLedger {
    delegations: BTreeMap<Uuid, Delegation>,
}

impl OriginLedger {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every recorded delegation, by asset.
    pub fn delegations(&self) -> impl Iterator<Item = &Delegation> {
        self.delegations.values()
    }

    /// The delegation recorded for `asset_id`.
    #[must_use]
    pub fn get(&self, asset_id: &Uuid) -> Option<&Delegation> {
        self.delegations.get(asset_id)
    }

    /// Record `sealed` as handed over now: pinned, with a deadline `deadline_ms` from `now_ms`
    /// on the monotonic clock `clock_epoch` names.
    ///
    /// Recording an asset that already has an open delegation replaces it: the origin delegated
    /// again, and only the newest hand-off is waited on (the older one, if its delegate still
    /// uploads it, is resolved by deduplication).
    pub fn hand_off(
        &mut self,
        sealed: &SealedHandOff,
        clock_epoch: &str,
        now_ms: u64,
        deadline_ms: u64,
    ) {
        let held_deletes = self
            .delegations
            .remove(&sealed.asset_id)
            .map(|previous| previous.held_deletes)
            .unwrap_or_default();
        tracing::info!(
            asset_id = %sealed.asset_id,
            delegate = %sealed.delegate,
            hand_off = %sealed.address,
            deadline_ms,
            "delegation: recorded hand-off; the original stays pinned"
        );
        self.delegations.insert(
            sealed.asset_id,
            Delegation {
                asset_id: sealed.asset_id,
                delegate: sealed.delegate,
                hand_off: sealed.address.clone(),
                own: sealed.own.clone(),
                release: sealed.release.clone(),
                clock_epoch: clock_epoch.to_owned(),
                deadline_ms: now_ms.saturating_add(deadline_ms),
                state: DelegationState::HandedOff,
                requeued_because: None,
                held_deletes,
            },
        );
    }

    /// Take the delegate's acknowledgement. It must name the recorded hand-off and delegate.
    /// Changes nothing about custody.
    ///
    /// # Errors
    ///
    /// [`DelegationError::UnknownDelegation`] or [`DelegationError::AckMismatch`].
    pub fn acknowledge(&mut self, ack: &HandOffAck) -> Result<(), DelegationError> {
        let delegation = self
            .delegations
            .get_mut(&ack.asset_id)
            .ok_or(DelegationError::UnknownDelegation(ack.asset_id))?;
        if delegation.hand_off != ack.hand_off || delegation.delegate != ack.delegate_device {
            tracing::warn!(asset_id = %ack.asset_id, "delegation: an acknowledgement did not match");
            return Err(DelegationError::AckMismatch);
        }
        if delegation.state == DelegationState::HandedOff {
            delegation.state = DelegationState::Acknowledged;
        }
        tracing::info!(asset_id = %ack.asset_id, "delegation: delegate acknowledged; not custody");
        Ok(())
    }

    /// Re-queue every open delegation whose deadline passed on `clock_epoch` at `now_ms`, whose
    /// deadline was set on another clock, or whose delegate `trust` no longer admits. Returns
    /// the assets re-queued, for the caller to upload itself or delegate again.
    pub fn due(
        &mut self,
        trust: &PinnedTrust,
        clock_epoch: &str,
        now_ms: u64,
    ) -> Vec<(Uuid, RequeueReason)> {
        let mut requeued = Vec::new();
        for delegation in self.delegations.values_mut().filter(|d| d.is_open()) {
            let reason = if admit_device(trust, delegation.delegate).is_err() {
                Some(RequeueReason::DelegateRevoked)
            } else if delegation.clock_epoch != clock_epoch {
                Some(RequeueReason::ClockReset)
            } else if now_ms >= delegation.deadline_ms {
                Some(RequeueReason::DeadlineExpired)
            } else {
                None
            };
            if let Some(reason) = reason {
                tracing::warn!(
                    asset_id = %delegation.asset_id,
                    delegate = %delegation.delegate,
                    ?reason,
                    "delegation: re-queued; the origin takes the upload back"
                );
                delegation.state = DelegationState::Requeued;
                delegation.requeued_because = Some(reason);
                requeued.push((delegation.asset_id, reason));
            }
        }
        requeued
    }

    /// Release `asset_id`'s original only through the verify-before-destroy gate: `verifier`
    /// answers for the **origin's own** hashes and must be backed by facts the origin fetched
    /// with its own session ([`ReleaseCoordinator::resolve_custody`]), never anything the
    /// delegate relayed. `verify_asset_accepted` is the origin's own `verify_asset` of the asset,
    /// and `read_back` whether the origin's copy still hashes to the signed original.
    ///
    /// A released asset hands back its held deletes. Either upload's custody releases it — the
    /// gate names content hashes, not who uploaded them — so a re-queued asset the origin
    /// uploaded itself releases through here too.
    ///
    /// # Errors
    ///
    /// [`DelegationError::UnknownDelegation`].
    pub fn release<V: StorageVerifier + ?Sized>(
        &mut self,
        asset_id: Uuid,
        verifier: &V,
        verify_asset_accepted: bool,
        read_back: bool,
    ) -> Result<ReleaseOutcome, DelegationError> {
        let delegation = self
            .delegations
            .get_mut(&asset_id)
            .ok_or(DelegationError::UnknownDelegation(asset_id))?;
        if delegation.state == DelegationState::Released {
            return Ok(ReleaseOutcome::Released {
                deletes: Vec::new(),
            });
        }
        if !read_back {
            tracing::warn!(%asset_id, "delegation: the origin's copy does not read back; kept");
            return Ok(ReleaseOutcome::Retained(RetainReason::ReadBackMismatch));
        }
        match ReleaseGate::new(verifier).may_release(
            asset_id,
            &delegation.release,
            verify_asset_accepted,
        ) {
            ReleaseDecision::Release => {
                delegation.state = DelegationState::Released;
                let deletes = std::mem::take(&mut delegation.held_deletes);
                tracing::info!(
                    %asset_id,
                    held_deletes = deletes.len(),
                    "delegation: custody confirmed by the origin's own check; released"
                );
                Ok(ReleaseOutcome::Released { deletes })
            }
            ReleaseDecision::Retain(reason) => {
                tracing::debug!(%asset_id, ?reason, "delegation: not yet held; still pinned");
                Ok(ReleaseOutcome::Retained(reason))
            }
        }
    }

    /// A signed delete of `asset_id`: held while its create has no confirmed custody (an open
    /// or re-queued delegation), so the server never sees a delete for an asset it has not
    /// indexed; otherwise returned to send now.
    pub fn hold_delete(&mut self, asset_id: Uuid, delete: Vec<u8>) -> DeleteDisposition {
        match self.delegations.get_mut(&asset_id) {
            Some(delegation) if delegation.state != DelegationState::Released => {
                tracing::info!(%asset_id, "delegation: delete held until the create's custody");
                delegation.held_deletes.push(delete);
                DeleteDisposition::Held
            }
            _ => DeleteDisposition::SendNow(delete),
        }
    }

    /// Forget a released or re-queued delegation the caller has finished with.
    pub fn forget(&mut self, asset_id: &Uuid) -> Option<Delegation> {
        self.delegations.remove(asset_id)
    }
}

// ─── The delegate ─────────────────────────────────────────────────────────────

/// A hand-off the delegate verified: every member hashes to what its request declares, the
/// signed manifest verifies under the origin's published key, and every member is the one the
/// manifest commits to.
#[derive(Debug, Clone)]
pub struct VerifiedHandOff {
    /// The container's content address.
    pub address: String,
    /// The verified container.
    pub hand_off: HandOff,
}

impl VerifiedHandOff {
    /// The asset.
    #[must_use]
    pub fn asset_id(&self) -> Uuid {
        self.hand_off.header.asset_id
    }

    /// Each member's request and bytes, in ladder order.
    pub fn members(&self) -> impl Iterator<Item = (&CreateUploadRequest, &[u8])> {
        self.hand_off
            .header
            .members
            .iter()
            .zip(self.hand_off.blobs.iter().map(Vec::as_slice))
    }

    /// What custody of every member the server can receipt looks like, for [`settle`].
    #[must_use]
    pub fn held_release(&self) -> HeldRelease {
        let blobs = self
            .members()
            .filter_map(|(request, bytes)| {
                let role = match request.blob_role {
                    BlobRole::Original => CoreBlobRole::Original,
                    BlobRole::Derivative => CoreBlobRole::Derivative,
                    BlobRole::Metadata => CoreBlobRole::Metadata,
                    BlobRole::Provenance => CoreBlobRole::Provenance,
                    BlobRole::Backup | BlobRole::DerivativeManifest => return None,
                };
                Some(ReceiptExpectations {
                    ciphertext_hash: hash_bytes(bytes),
                    size: bytes.len() as u64,
                    role,
                    envelope_hash: (request.blob_role == BlobRole::Provenance)
                        .then(|| hash_bytes(bytes)),
                })
            })
            .collect();
        HeldRelease {
            asset_id: self.asset_id(),
            blobs,
        }
    }
}

/// Verify a hand-off addressed to `delegate_device`, all or nothing.
///
/// `trust` is the account's directory as this device pins it (admitted against `versions`, so a
/// stale directory cannot readmit a revoked origin); `max_protocol` is the newest upload
/// protocol this delegate speaks.
///
/// # Errors
///
/// The [`DelegationError`] naming the first check that failed. Nothing is persisted.
#[instrument(skip_all, fields(bytes = bytes.len(), delegate = %delegate_device))]
pub fn verify_hand_off(
    bytes: &[u8],
    trust: &PinnedTrust,
    versions: &dyn DirectoryVersions,
    delegate_device: Uuid,
    max_protocol: &str,
) -> Result<VerifiedHandOff, DelegationError> {
    admit_directory(trust, versions)?;
    let hand_off = HandOff::decode(bytes)?;
    let header = &hand_off.header;
    if header.delegate_device != delegate_device {
        return Err(DelegationError::WrongDelegate {
            addressed: header.delegate_device,
        });
    }
    if header.protocol_version.as_str() > max_protocol {
        tracing::info!(
            bundle = %header.protocol_version,
            ours = max_protocol,
            "delegation: refusing a bundle newer than this device speaks"
        );
        return Err(DelegationError::ProtocolUnsupported {
            bundle: header.protocol_version.clone(),
            ours: max_protocol.to_owned(),
        });
    }
    let origin = admit_device(trust, header.origin_device)?;

    let tampered = |what: String| DelegationError::Tampered(what);
    // Every member: its bytes are what its request declares, and its request is this bundle's.
    for (request, blob) in header.members.iter().zip(&hand_off.blobs) {
        let actual = hash_bytes(blob).to_hex();
        if actual != request.hash || request.manifest_envelope.ciphertext_hash != request.hash {
            return Err(tampered(format!(
                "a {:?} member does not hash to its declared address",
                request.blob_role
            )));
        }
        if request.protocol_version != header.protocol_version
            || request.manifest_envelope.protocol_version != header.protocol_version
        {
            return Err(tampered("a member declares another protocol".into()));
        }
        if request.manifest_envelope.file_id != header.asset_id.to_string()
            || request.manifest_envelope.created_by_device != header.origin_device.to_string()
        {
            return Err(tampered("a member names another asset or device".into()));
        }
    }

    let only = |role: BlobRole| {
        let mut found = header
            .members
            .iter()
            .zip(&hand_off.blobs)
            .filter(|(request, _)| request.blob_role == role);
        let first = found.next();
        if found.next().is_some() {
            return Err(tampered(format!("more than one {role:?} member")));
        }
        Ok(first)
    };
    let (_, provenance) =
        only(BlobRole::Provenance)?.ok_or_else(|| tampered("no provenance member".into()))?;
    let (_, original) =
        only(BlobRole::Original)?.ok_or_else(|| tampered("no original member".into()))?;
    let metadata = only(BlobRole::Metadata)?;

    // The signed manifest: the origin's, verifying under the key its directory entry publishes.
    let record = ProvenanceRecord::decode(provenance)
        .map_err(|error| tampered(format!("the provenance member: {error}")))?;
    // The chain head is the hash of these bytes, so a re-spelling of the same values is not the
    // record the origin produced: the server would refuse it, and the delegate refuses it first.
    if cbor::to_canonical_vec(&record).ok().as_deref() != Some(provenance) {
        return Err(tampered(
            "the provenance record is not canonically encoded".into(),
        ));
    }
    if record.asset_id != header.asset_id || !record.mirrors_manifest() {
        return Err(tampered(
            "the provenance record disagrees with its manifest".into(),
        ));
    }
    let manifest = &record.manifest;
    let core = &manifest.core;
    if core.file_id != header.asset_id || core.created_by_device != header.origin_device {
        return Err(tampered(
            "the manifest names another asset or device".into(),
        ));
    }
    verify_device_signature(manifest, &trust.directory)
        .map_err(|reason| tampered(format!("the manifest's signature: {reason:?}")))?;

    // Every member the manifest commits to is the member carried.
    if core.ciphertext_hash != hash_bytes(original) {
        return Err(tampered(
            "the original is not the one the manifest signs".into(),
        ));
    }
    match (core.metadata_blob_hash, metadata) {
        (Some(signed), Some((_, blob))) if signed == hash_bytes(blob) => {}
        (None, None) => {}
        _ => {
            return Err(tampered(
                "the metadata blob is not the one the manifest signs".into(),
            ));
        }
    }
    for request in &header.members {
        let envelope = &request.manifest_envelope;
        let agrees = envelope.created_by_user == core.created_by_user.to_string()
            && envelope.album_id.as_deref() == Some(core.album_id.to_string().as_str())
            && envelope.amk_version == core.amk_version.0
            && envelope.timestamp == core.timestamp
            && envelope.metadata_blob_hash == core.metadata_blob_hash.map(|hash| hash.to_hex())
            && envelope.prior_provenance_hash
                == core.prior_provenance_hash.map(|hash| hash.to_hex());
        if !agrees {
            return Err(tampered(format!(
                "a {:?} member's envelope is not the signed manifest's",
                request.blob_role
            )));
        }
    }

    // Derivatives: each derivative manifest is the origin's, for this asset, and each
    // derivative is one a derivative manifest signs.
    let mut signed_derivatives = BTreeSet::new();
    for (request, blob) in header.members.iter().zip(&hand_off.blobs) {
        if request.blob_role != BlobRole::DerivativeManifest {
            continue;
        }
        let derivative: DerivativeManifest = cbor::from_slice(blob)
            .map_err(|error| tampered(format!("a derivative manifest: {error}")))?;
        if cbor::to_canonical_vec(&derivative).ok().as_deref() != Some(blob.as_slice()) {
            return Err(tampered(
                "a derivative manifest is not canonically encoded".into(),
            ));
        }
        let signed_by_origin = derivative.core.generated_by_device == header.origin_device
            && derivative.device_sig.classical_algorithm()
                == origin.dsk_public.classical_algorithm()
            && origin
                .dsk_public
                .verify(&derivative.signing_bytes(), &derivative.device_sig);
        if derivative.core.source_asset_id != header.asset_id || !signed_by_origin {
            return Err(tampered(
                "a derivative manifest is not the origin's for this asset".into(),
            ));
        }
        signed_derivatives.insert((derivative.core.ciphertext_hash, derivative.core.format));
    }
    for (request, blob) in header.members.iter().zip(&hand_off.blobs) {
        if request.blob_role == BlobRole::Derivative
            && !signed_derivatives.contains(&(hash_bytes(blob), request.content_type.clone()))
        {
            return Err(tampered("a derivative no derivative manifest signs".into()));
        }
    }

    let address = HandOff::address(bytes);
    tracing::info!(
        %address,
        asset_id = %header.asset_id,
        origin = %header.origin_device,
        members = header.members.len(),
        "delegation: hand-off verified"
    );
    Ok(VerifiedHandOff { address, hand_off })
}

/// Where a delegate keeps the hand-offs it accepted until custody is confirmed. Never a library:
/// a delegate copy is not an asset of the delegate's.
pub trait DelegateInbox: Send + Sync {
    /// Persist `bytes` under `address` **durably** — written and synced, its directory entry
    /// too — before returning. An acknowledgement follows only after this returns.
    fn persist(&self, address: &str, bytes: &[u8]) -> io::Result<()>;
    /// The bytes held under `address`.
    fn load(&self, address: &str) -> io::Result<Option<Vec<u8>>>;
    /// Every address held.
    fn addresses(&self) -> io::Result<Vec<String>>;
    /// Delete the copy under `address`. Removing an absent one is not an error.
    fn remove(&self, address: &str) -> io::Result<()>;
}

/// An in-memory [`DelegateInbox`]: durable for the life of the process.
#[derive(Debug, Default)]
pub struct MemoryInbox(Mutex<BTreeMap<String, Vec<u8>>>);

impl MemoryInbox {
    fn held(&self) -> io::Result<std::sync::MutexGuard<'_, BTreeMap<String, Vec<u8>>>> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("the inbox lock is poisoned"))
    }
}

impl DelegateInbox for MemoryInbox {
    fn persist(&self, address: &str, bytes: &[u8]) -> io::Result<()> {
        self.held()?.insert(address.to_owned(), bytes.to_vec());
        Ok(())
    }

    fn load(&self, address: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(self.held()?.get(address).cloned())
    }

    fn addresses(&self) -> io::Result<Vec<String>> {
        Ok(self.held()?.keys().cloned().collect())
    }

    fn remove(&self, address: &str) -> io::Result<()> {
        self.held()?.remove(address);
        Ok(())
    }
}

/// Take a hand-off: verify it ([`verify_hand_off`]), persist it durably in `inbox`, and only
/// then acknowledge it.
///
/// # Errors
///
/// Whatever [`verify_hand_off`] refused, or [`DelegationError::Inbox`] when it could not be
/// persisted — in which case there is no acknowledgement and the origin keeps the upload.
pub fn accept_hand_off(
    bytes: &[u8],
    trust: &PinnedTrust,
    versions: &dyn DirectoryVersions,
    delegate_device: Uuid,
    max_protocol: &str,
    inbox: &dyn DelegateInbox,
) -> Result<(VerifiedHandOff, HandOffAck), DelegationError> {
    let verified = verify_hand_off(bytes, trust, versions, delegate_device, max_protocol)?;
    inbox
        .persist(&verified.address, bytes)
        .map_err(|error| DelegationError::Inbox(format!("persist the hand-off: {error}")))?;
    let ack = HandOffAck {
        asset_id: verified.asset_id(),
        hand_off: verified.address.clone(),
        delegate_device,
    };
    tracing::info!(asset_id = %ack.asset_id, hand_off = %ack.hand_off, "delegation: persisted and acknowledged");
    Ok((verified, ack))
}

/// What [`upload_hand_off`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DelegatedUpload {
    /// The members whose sessions this run drove, with how each resolved: `true` when the
    /// server already held the bytes (a merge), `false` when they were transferred.
    pub pushed: Vec<(String, bool)>,
    /// The members the delegate's own policy deferred to a later window.
    pub deferred: Vec<String>,
}

/// Upload a verified hand-off under the delegate's own session, in the order the delegate's own
/// `scheduler` permits — its battery and metering policy, not the origin's. Each member's
/// request is sent exactly as the origin built it.
///
/// # Errors
///
/// [`DelegationError::Upload`] for a member the server refused or a transfer that failed.
#[instrument(skip_all, fields(asset_id = %hand_off.asset_id(), hand_off = %hand_off.address))]
pub async fn upload_hand_off(
    client: &UploadClient,
    scheduler: &StagedScheduler,
    hand_off: &VerifiedHandOff,
) -> Result<DelegatedUpload, DelegationError> {
    let staged = StagedAsset::new(
        hand_off.asset_id().to_string(),
        hand_off
            .members()
            .map(|(request, _)| {
                TierBlob::new(tier_of(request.blob_role), &request.hash, request.size)
            })
            .collect(),
    );
    let planned = scheduler.plan_sessions(&staged);
    let mut report = DelegatedUpload {
        deferred: staged
            .blobs
            .iter()
            .filter(|blob| !planned.iter().any(|p| p.hash == blob.hash))
            .map(|blob| blob.hash.clone())
            .collect(),
        ..DelegatedUpload::default()
    };
    for tier_blob in planned {
        let Some((request, bytes)) = hand_off
            .members()
            .find(|(request, _)| request.hash == tier_blob.hash)
        else {
            continue;
        };
        tracing::info!(role = ?request.blob_role, hash = %request.hash, "delegation: uploading a member");
        let merged = match client.upload(request, &bytes).await? {
            UploadOutcome::Completed { .. } => false,
            UploadOutcome::AlreadyStored { .. } => true,
        };
        report.pushed.push((request.hash.clone(), merged));
    }
    tracing::info!(
        pushed = report.pushed.len(),
        deferred = report.deferred.len(),
        "delegation: upload run done"
    );
    Ok(report)
}

/// What [`settle`] did with the delegate's copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// Custody of every member is confirmed: the copy is deleted.
    Removed,
    /// Not yet: the copy is kept and the upload is retried later.
    Kept(RetainReason),
}

/// Delete the delegate's copy of `hand_off` once the server holds every member it can receipt,
/// by custody facts the delegate fetched with its own session through `coordinator`. Deferred
/// members (`deferred` non-empty) keep the copy.
///
/// # Errors
///
/// [`DelegationError::Inbox`] when the copy could not be deleted.
pub async fn settle<C: ReleaseClock>(
    coordinator: &ReleaseCoordinator<C>,
    hand_off: &VerifiedHandOff,
    deferred: &[String],
    inbox: &dyn DelegateInbox,
) -> Result<Settled, DelegationError> {
    if !deferred.is_empty() {
        return Ok(Settled::Kept(RetainReason::NotDurable));
    }
    let held = hand_off.held_release();
    let hashes: Vec<Hash32> = held.blobs.iter().map(|b| b.ciphertext_hash).collect();
    let verifier = coordinator
        .resolve_custody(std::slice::from_ref(&held))
        .await;
    // The delegate cannot run `verify_asset` (it holds no album key); what it verified is the
    // manifest's device signature and every member's hash, which `verify_hand_off` required.
    match ReleaseGate::new(&verifier).may_release(hand_off.asset_id(), &hashes, true) {
        ReleaseDecision::Release => {
            inbox
                .remove(&hand_off.address)
                .map_err(|error| DelegationError::Inbox(format!("delete the copy: {error}")))?;
            tracing::info!(asset_id = %hand_off.asset_id(), "delegation: custody confirmed; delegate copy deleted");
            Ok(Settled::Removed)
        }
        ReleaseDecision::Retain(reason) => {
            tracing::info!(asset_id = %hand_off.asset_id(), ?reason, "delegation: copy kept until custody");
            Ok(Settled::Kept(reason))
        }
    }
}

/// What [`dispose_failure`] did with a held hand-off a run could not deliver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldFailure {
    /// The failure is transient: the copy stays for the next run.
    Kept,
    /// The failure is permanent ([`DelegationError::is_permanent`]): the copy is deleted.
    Discarded,
}

/// Decide the fate of the hand-off held under `address` after `error`: a permanent refusal
/// deletes the delegate's copy, so it neither stays in the inbox forever nor fails every later
/// run; a transient one keeps it.
///
/// Discarding loses nothing: the origin keeps its original pinned until its own custody check
/// passes, and re-queues the upload itself when its deadline expires.
///
/// # Errors
///
/// [`DelegationError::Inbox`] when a permanently refused copy could not be deleted.
#[instrument(skip_all, fields(hand_off = %address, permanent = error.is_permanent()))]
pub fn dispose_failure(
    inbox: &dyn DelegateInbox,
    address: &str,
    error: &DelegationError,
) -> Result<HeldFailure, DelegationError> {
    if !error.is_permanent() {
        tracing::info!(%error, "delegation: transient failure; the copy is kept");
        return Ok(HeldFailure::Kept);
    }
    inbox
        .remove(address)
        .map_err(|io| DelegationError::Inbox(format!("discard the copy: {io}")))?;
    tracing::warn!(%error, "delegation: permanently refused; the copy is discarded");
    Ok(HeldFailure::Discarded)
}

#[cfg(test)]
mod tests;
