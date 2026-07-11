//! LAN peering: direct device-to-device sync within a single user's own devices
//! (SSoT: [Peering]; slice `S-E3` in the repo-root `SLICES.md`).
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
//!   handle nor a device name) and the [`Discovery`](discovery::Discovery) seam. The mDNS
//!   responder is mocked in tests and is where a real pure-Rust responder (`mdns-sd`) plugs
//!   in; discovery stays deterministic so every rule is a unit test.
//! - [`channel`] — a mutually-authenticated **TLS 1.3** channel with no CA. The TLS layer
//!   proves only classical possession; the **hybrid** identity check (that the peer chains to
//!   the shared User IK exactly as published in the [device directory]) rides *above* the
//!   channel, before any payload byte, bound to the session via the RFC 5705 TLS exporter.
//! - [`delta`] — "what's missing" is the complement of two content-address sets, reusing the
//!   sync-cursor model rather than inventing a diff.
//! - [`transfer`] — the payload is a **delta-scoped [backup artifact]**, fetched with ranged
//!   `GET` (resumable, idempotent) and ingested through the **same backup restore path**
//!   (`capsule-core::backup`), with chain-aware forward-vs-stale reconciliation so a peer can
//!   never resurrect a locally-superseded asset.
//!
//! Trust is *identity*, never *content*: the channel authenticates **who** you talk to; every
//! received asset is still re-verified by restore (ciphertext hash, STREAM tags, `verify_asset`).
//!
//! [Peering]: https://docs/design/peering/
//! [device directory]: https://docs/design/cryptography/keys/#device-directory
//! [backup artifact]: https://docs/design/backup-recovery/#backup-artifact

pub mod channel;
pub mod delta;
pub mod discovery;
pub mod transfer;

use capsule_core::backup::BackupError;
use capsule_core::crypto::CryptoError;
pub use channel::{PEERING_PROTOCOL, PeerHello, PinnedTrust, VerifiedPeer, accept, connect};
pub use delta::{Offer, missing_from, symmetric_difference};
pub use discovery::{
    DiscoveredPeer, Discovery, MockDiscovery, OpaqueAdvertisement, SERVICE_TYPE, rotation_epoch,
};
use thiserror::Error;
pub use transfer::{
    ArtifactBlobSource, DeltaExport, PeerRestore, artifact_address, build_delta_artifact, ingest,
    pull_artifact,
};
use uuid::Uuid;

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
    /// The peer advertised a peering **transport protocol** this device does not speak. There is
    /// no degraded-mode fallback — the channel is torn down (`426 Upgrade Required` in framing)
    /// before any payload byte, and the device proceeds to ordinary server sync.
    #[error("peering protocol mismatch: peer speaks {theirs}, we speak {ours}")]
    ProtocolMismatch {
        /// The peer's advertised peering-protocol value.
        theirs: String,
        /// This device's peering-protocol value.
        ours: String,
    },
    /// The peer's pinned directory is not signed by the User IK we trust — a foreign identity.
    #[error("peer directory does not chain to the pinned User IK")]
    ForeignIdentity,
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
}

#[cfg(test)]
mod tests;
