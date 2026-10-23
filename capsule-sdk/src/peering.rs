//! LAN peering: direct device-to-device sync within a single user's own devices
//! (SSoT: [Peering]).
//!
//! Peering is an **accelerator, never a replacement** for server sync: two of a user's
//! devices on the same network move a freshly-imported asset directly instead of
//! round-tripping every byte through the server. If no peer answers, discovery fails
//! silently and the device falls back to ordinary sync — nothing depends on it succeeding.
//!
//! This module owns only the two things peering introduces of its own — a LAN **discovery**
//! mechanism and a **transport** — and borrows everything else:
//!
//! - [`discovery`] — the opaque, rotating mDNS advertisement (it must leak neither a user
//!   handle nor a device name) and the [`Discovery`] seam. The mDNS
//!   responder is mocked in tests and is where a real pure-Rust responder (`mdns-sd`) plugs
//!   in; discovery stays deterministic so every rule is a unit test.
//! - [`channel`] — a mutually-authenticated **TLS 1.3** channel with no CA. The TLS layer
//!   proves only classical possession; the **hybrid** identity check (that the peer chains to
//!   the shared User IK exactly as published in the [device directory]) rides *above* the
//!   channel, before any payload byte, bound to the session via the RFC 5705 TLS exporter.
//! - [`session`] — the directory anti-rollback floor, the per-source handshake rate limit, and
//!   the per-request revocation check that quarantines deliveries made after a revocation.
//! - [`delta`] — "what's missing" is the complement of two content-address sets, reusing the
//!   sync-cursor model rather than inventing a diff.
//! - [`transfer`] — the payload is a **delta-scoped [backup artifact]**, fetched with ranged
//!   `GET` (resumable, idempotent) and ingested through the **same backup restore path**
//!   (`capsule-core::backup`), with chain-aware forward-vs-stale reconciliation so a peer can
//!   never resurrect a locally-superseded asset.
//! - [`delegation`] — upload delegation: a device hands sealed, signed upload bundles to another
//!   device of the account, which uploads them as an opaque courier; the origin releases its
//!   original only on custody it checked itself.
//!
//! Trust is *identity*, never *content*: the channel authenticates **who** you talk to; every
//! received asset is still re-verified by restore (ciphertext hash, STREAM tags, `verify_asset`).
//!
//! [Peering]: https://docs/design/peering/
//! [device directory]: https://docs/design/cryptography/keys/#device-directory
//! [backup artifact]: https://docs/design/backup-recovery/#backup-artifact

pub mod channel;
pub mod delegation;
pub mod delta;
pub mod discovery;
pub mod session;
pub mod transfer;

use std::net::IpAddr;

use capsule_core::backup::BackupError;
use capsule_core::crypto::CryptoError;
pub use channel::{
    ArtifactSecret, LocalPeer, Message, Offered, Offering, PEERING_PROTOCOL, PEERING_PROTOCOL_MIN,
    PeerChannel, PeerClient, PeerHello, PinnedTrust, RangeFailure, ServeReport, ServedRange,
    UpdateDevice, VerifiedPeer, VersionWindow, accept, connect, serve,
};
pub use delegation::{
    DelegateInbox, DelegatedUpload, Delegation, DelegationError, DelegationState,
    DeleteDisposition, HandOff, HandOffAck, HeldFailure, MemoryInbox, OriginLedger,
    PERMANENT_UPLOAD_REFUSALS, Quarantine, ReleaseOutcome, RequeueReason, SealedHandOff, Settled,
    TakenHandOff, TakenHandOffs, VerifiedHandOff, accept_hand_off, accept_hand_off_from,
    dispose_failure, offer_hand_offs, seal, settle, take_hand_offs, upload_hand_off,
    verify_hand_off,
};
pub use delta::{
    OFFER_PAGE_CAP, Offer, OfferPage, decode_offer_page, missing_from, offer_bucket,
    symmetric_difference,
};
pub use discovery::{
    DiscoveredPeer, Discovery, MockDiscovery, OpaqueAdvertisement, RotationEpoch, SERVICE_TYPE,
    new_boot_id, rotation_epoch,
};
pub use session::{
    DirectoryVersions, HandshakeLimiter, MemoryDirectoryVersions, PeerSession, SharedTrust,
    TrustSource,
};
use thiserror::Error;
pub use transfer::{
    ArtifactBlobSource, ChannelBlobSource, DeltaExport, LocalLibrary, PeerRestore,
    artifact_address, build_delta_artifact, ingest, pull_artifact, pull_over_channel,
};
use uuid::Uuid;

use crate::fetch::FetchScope;

/// Everything peering can fail with. Peering is best-effort — a caller treats most of these as
/// "no peer / fall back to server sync" — but the identity-check variants are load-bearing
/// security rejections that must never be silently downgraded.
#[derive(Debug, Error)]
pub enum PeeringError {
    /// A TLS handshake, config, or certificate-generation failure.
    #[error("peering TLS error: {0}")]
    Tls(String),
    /// A socket read/write failure on the peering channel.
    #[error("peering I/O error: {0}")]
    Io(String),
    /// The two devices' peering-protocol windows share no version. The channel is torn down
    /// before any payload byte, and `update` names the device that must update.
    #[error(
        "peering protocol windows are disjoint: ours {ours}, peer's {theirs}; update {update:?}"
    )]
    VersionDisjoint {
        /// This device's window.
        ours: VersionWindow,
        /// The peer's advertised window.
        theirs: VersionWindow,
        /// Which device must update for the windows to meet.
        update: UpdateDevice,
    },
    /// The peer's pinned directory is not signed by the User IK we trust — a foreign identity.
    #[error("peer directory does not chain to the pinned User IK")]
    ForeignIdentity,
    /// The pinned device directory is older than one this device has already verified: a stale
    /// directory could readmit a revoked device.
    #[error("pinned device directory version {pinned} is below the verified {highest}")]
    DirectoryRollback {
        /// The pinned directory's version.
        pinned: u64,
        /// The highest version this device has verified.
        highest: u64,
    },
    /// The peer presented a device directory older than the one this device pins.
    #[error("peer presented device directory version {theirs}, below ours {ours}")]
    PeerDirectoryStale {
        /// The version the peer presented.
        theirs: u64,
        /// The version this device pins.
        ours: u64,
    },
    /// The source address exceeded the handshake rate limit.
    #[error("peering handshake rate limit reached for {0}")]
    RateLimited(IpAddr),
    /// The receiver's synchronization scope does not eagerly hold originals, which every backup
    /// artifact carries; the receiver syncs from the server instead.
    #[error("receiver scope {0:?} does not take originals over peering")]
    OutOfScope(FetchScope),
    /// The peer presented a `device_id` that is absent from our pinned device directory: it is
    /// not one of this user's enrolled devices.
    #[error("peer device {0} is not in the pinned device directory")]
    UnknownDevice(Uuid),
    /// The peer's directory entry carries a `revoked_at`: a removed device cannot peer.
    #[error("peer device {0} has been revoked from the device directory")]
    RevokedDevice(Uuid),
    /// The application-layer hybrid signature over the channel-binding did not verify under the
    /// peer's published device key — the peer does not hold the private key it claims.
    #[error("peer hybrid identity proof failed to verify")]
    HybridCheckFailed,
    /// A discovery-layer failure (advertise/browse).
    #[error("peering discovery error: {0}")]
    Discovery(String),
    /// A framing / serialization failure exchanging the handshake hello.
    #[error("peering codec error: {0}")]
    Codec(String),
    /// An underlying cryptographic failure (e.g. a hardware signer refusing to sign the proof).
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// A failure opening, verifying, or restoring the transferred backup artifact.
    #[error(transparent)]
    Backup(#[from] BackupError),
    /// The ranged artifact transfer failed (drop budget exhausted, integrity mismatch).
    #[error("peering transfer failed: {0}")]
    Transfer(String),
    /// The peer did not complete a read or write on the channel within its bound: the handshake
    /// within [`channel::HANDSHAKE_TIMEOUT`], a frame within the channel's I/O timeout.
    #[error("peering channel timed out: {0}")]
    Timeout(&'static str),
    /// The peer answered a request with a refusal. `kind` is the peer's stable name for why
    /// (one of [`PeeringError::kind`] or [`DelegationError::kind`]); it is the peer's claim,
    /// reported, never acted on as a fact about this device.
    #[error("the peer refused the request ({kind}): {message}")]
    PeerRefused {
        /// The peer's stable name for the refusal.
        kind: String,
        /// The peer's English detail.
        message: String,
    },
    /// The address is not on the local network: peering is LAN-only ([`is_lan`]).
    #[error("{0} is not a local-network address; peering is LAN-only")]
    NotLan(IpAddr),
    /// The account's device directory could not be fetched to authorize a request; the request
    /// is refused rather than served under trust that may be stale (fail closed).
    #[error("the device directory is unavailable: {0}")]
    Directory(String),
}

impl PeeringError {
    /// The stable name a `--json` report and a channel refusal carry this error under. One per
    /// variant; never renamed.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Tls(_) => "peering_tls",
            Self::Io(_) => "peering_io",
            Self::VersionDisjoint { .. } => "version_disjoint",
            Self::ForeignIdentity => "foreign_identity",
            Self::DirectoryRollback { .. } => "directory_rollback",
            Self::PeerDirectoryStale { .. } => "peer_directory_stale",
            Self::RateLimited(_) => "rate_limited",
            Self::OutOfScope(_) => "out_of_scope",
            Self::UnknownDevice(_) => "unknown_device",
            Self::RevokedDevice(_) => "revoked_device",
            Self::HybridCheckFailed => "hybrid_check_failed",
            Self::Discovery(_) => "discovery",
            Self::Codec(_) => "codec",
            Self::Crypto(_) => "crypto",
            Self::Backup(_) => "backup",
            Self::Transfer(_) => "transfer",
            Self::Timeout(_) => "peering_timeout",
            Self::PeerRefused { .. } => "peer_refused",
            Self::NotLan(_) => "not_lan",
            Self::Directory(_) => "directory_unavailable",
        }
    }
}

/// Whether `ip` is on the local network: loopback, an RFC 1918 private or link-local IPv4
/// address, or an IPv6 unique-local (`fc00::/7`) or link-local (`fe80::/10`) address, including
/// an IPv4-mapped one of those. Peering dials and admits only these, so a peering channel never
/// leaves the LAN.
#[must_use]
pub fn is_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_lan(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests;
