//! Establishing the channel: mutually-authenticated TLS 1.3 with an application-layer hybrid
//! identity check.
//!
//! A peer connection is a framed request/response exchange over a **mutually-authenticated TLS
//! 1.3** channel with **no CA** — the device keys are the trust anchor. TLS 1.3 has no ML-DSA
//! certificate path, so the split
//! the peering doc mandates is honored exactly:
//!
//! 1. **Classical, at the TLS layer.** Each side presents a self-signed leaf ([`rcgen`]) and the
//!    handshake proves possession of its classical private key. The certificates carry no trust
//!    of their own — the accept-any verifiers below deliberately accept every well-formed peer
//!    cert, because trust is decided one layer up.
//! 2. **Hybrid, above the channel.** Once the handshake completes, each side derives the RFC 5705
//!    **TLS exporter** keying material — a value cryptographically bound to *this* TLS session —
//!    and signs it with its **hybrid** device key. The peer verifies that signature under the
//!    key published for the claimed `device_id` in the shared, IK-signed [device directory]. A
//!    MITM that re-terminated TLS would export different material and could not forge the
//!    victim's hybrid signature over it, so the hybrid identity is bound to the channel. The
//!    directory *is* the trust anchor: a device not in it — revoked, or a foreign user entirely —
//!    cannot pass this check, and it runs **before any payload byte**.
//!
//! Around that core the channel enforces the rest of the peering trust model:
//!
//! - **Version window.** Each hello advertises a `[Min, Max]` peering-protocol window and the
//!   pair speaks the highest common version; only disjoint windows end the connection, with a
//!   typed [`PeeringError::VersionDisjoint`] naming which device must update.
//! - **Directory anti-rollback.** The highest `directory_version` this device has verified is
//!   persisted behind [`DirectoryVersions`]; a pinned directory below it is refused (a stale
//!   directory cannot readmit a revoked device), and so is a peer presenting a lower version.
//! - **Handshake rate limit.** [`accept`] admits a source address only through a
//!   [`HandshakeLimiter`], so the LAN cannot probe a device for membership.
//! - **Artifact secret.** A second exporter label yields the per-session [`ArtifactSecret`]
//!   that seals a sync artifact: fresh per session, never stored, never a user passphrase.
//! - **Lower Effort marking.** The channel carries bulk bytes (hand-offs, sync artifacts), so
//!   both sides mark the socket with the Lower Effort DSCP before the handshake, best effort
//!   ([`crate::net::mark_lower_effort`]).
//!
//! # After the handshake
//!
//! [`accept`] and [`connect`] keep the verified TLS stream and return it as a [`PeerChannel`],
//! bound to the [`VerifiedPeer`] the hybrid check proved. There is no other path to a payload
//! byte: nothing is ever sent outside the verified TLS session, and a failed check returns no
//! channel at all.
//!
//! Over it the pair speaks small framed messages ([`Message`]): a one-byte tag, a big-endian
//! `u32` length, and the payload — JSON for a control frame, raw bytes for a data frame. Every
//! length is checked against its ceiling **before** a byte of the payload is read
//! ([`MAX_FRAME`], [`MAX_DATA_FRAME`], [`MAX_RANGE`]), and every read and write is bounded in
//! time, so a peer can neither make this device allocate without bound nor hold it forever.
//!
//! From [`OFFER_EXCHANGE_PROTOCOL`] the pulling side **opens** with its offer
//! ([`PeerClient::open_offering`], read by [`PeerChannel::receive_offer`]): the padded, capped
//! [offer pages](super::delta) of what it holds, or a frame saying it offers nothing. A pair
//! below that version skips the opening, and the serving side lists what it would have offered
//! without one.
//!
//! One side **serves** ([`serve`]): it lists the artifacts it offers ([`Offering`]), then
//! answers ranged pulls of exactly those and takes acknowledgements, authorizing **each
//! request** against the trust its [`TrustSource`] pins now ([`PeerSession::authorize`]), so a
//! peer revoked mid-session is refused at its next request. The other side **pulls**
//! ([`PeerClient`]): the transfer layer drives it through the shared resumable ranged fetch
//! ([`super::pull_over_channel`]), which verifies the reassembled bytes against their content
//! address. Every check runs against the identity the handshake verified
//! ([`PeerChannel::peer`]), never against a device id a frame claims.
//!
//! [device directory]: https://docs/design/cryptography/keys/#device-directory

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use capsule_core::crypto::keys::{
    DeviceDirectory, DeviceEntry, HybridSignature, HybridVerifyingKey, Signer,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    ClientConfig, DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex as AsyncMutex;
use tokio_rustls::{TlsAcceptor, TlsConnector, TlsStream};
use tracing::instrument;
use uuid::Uuid;

use super::PeeringError;
use super::delegation::{DelegationError, HandOffAck};
use super::delta::{MAX_OFFER_PAGE_BYTES, MAX_OFFER_PAGES, Offer, decode_offer_page};
use super::session::{DirectoryVersions, HandshakeLimiter, PeerSession, TrustSource};
use crate::net::negotiation::is_protocol_date;

/// The newest peering **transport protocol** version this build speaks — the top of its
/// [`VersionWindow`]. Date-based (`YYYY-MM-DD`), it shares the date grammar with the
/// client-server protocol but is its **own version space**: peering values compare only against
/// peering values, and the transport revs independently of the server protocol.
pub const PEERING_PROTOCOL: &str = OFFER_EXCHANGE_PROTOCOL;

/// The oldest peering transport protocol version this build still speaks — the bottom of its
/// [`VersionWindow`]. `2026-10-23` added the framed exchange after the hello ([`Message`]); a
/// peer older than that closes the channel after its hello and carries no payload, so it is
/// outside the window and the pair is told which device to update.
pub const PEERING_PROTOCOL_MIN: &str = "2026-10-23";

/// The peering protocol version that added the **opening offer** ([`Message::Offer`]): on a
/// channel negotiated at or above it, the pulling side opens with its offer (or says it offers
/// nothing) before the serving side lists, so a sync artifact is cut to what the receiver is
/// missing. A pair that negotiates below it exchanges no opening, and the serving side lists
/// what it would have offered without one. The addition is additive: either version speaks the
/// rest of the exchange unchanged.
pub const OFFER_EXCHANGE_PROTOCOL: &str = "2026-10-29";

/// How long a [`PeerClient`] that sent an opening waits for the listing: the serving side cuts
/// and seals its artifact once it has read the opening (an offer, or that the receiver offers
/// nothing) and only then lists, which can take far longer than one frame's I/O timeout. Added
/// to that timeout.
pub const LISTING_BUILD_ALLOWANCE: Duration = Duration::from_mins(10);

/// The bound on the whole handshake: TLS, both hellos and the hybrid check. A peer that stalls
/// it is dropped.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// The default bound on every frame read and write after the handshake
/// ([`PeerChannel::with_io_timeout`]).
pub const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// The slowest rate, in bytes per second, at which a [`PeerClient`] keeps pulling a ranged
/// answer. Each answer of `len` bytes must arrive whole within the channel's I/O timeout plus
/// `len` divided by this rate, so a peer that trickles frames just inside the per-frame timeout
/// cannot hold the pulling side indefinitely.
pub const MIN_PULL_RATE: u64 = 64 * 1024;

/// The slowest rate, in bytes per second, at which a serving session expects its peer to verify
/// and durably persist an artifact it pulled. While an offered artifact is unacknowledged, the
/// session waits for the peer's next request for the channel's I/O timeout plus the largest such
/// artifact's length at this rate ([`serve`]), so a large hand-off on slow storage is still
/// acknowledged over the channel instead of timing out before its acknowledgement arrives.
pub const MIN_PERSIST_RATE: u64 = 1024 * 1024;

/// The largest data frame. A ranged answer is cut into frames of at most this many bytes.
pub const MAX_DATA_FRAME: usize = 1024 * 1024;

/// The most bytes one ranged request is answered with, whatever it asked for.
pub const MAX_RANGE: u64 = crate::fetch::SINK_WINDOW;

/// The most artifacts one [`Offering`] lists; it keeps a listing within [`MAX_FRAME`].
pub const MAX_LISTING: usize = 256;

/// The largest artifact an [`Offering`] lists and a [`PeerClient`] pulls.
pub const MAX_ARTIFACT_LEN: u64 = 4 * 1024 * 1024 * 1024;

/// The tag of a control frame: a JSON [`Message`].
const TAG_CONTROL: u8 = 1;

/// The tag of a data frame: raw bytes of the ranged answer in flight.
const TAG_DATA: u8 = 2;

/// Length of the exported channel-binding material the hybrid proof covers.
const EXPORTER_LEN: usize = 32;

/// The RFC 5705 exporter label. Distinct from any other exporter use so the derived material is
/// domain-separated to peering's hybrid check.
const EXPORTER_LABEL: &[u8] = b"EXPORTER-capsule-peering-hybrid-v1";

/// The RFC 5705 exporter label the [`ArtifactSecret`] is derived under — distinct from
/// [`EXPORTER_LABEL`], so the value that seals an artifact is never the value either side signed.
const ARTIFACT_EXPORTER_LABEL: &[u8] = b"EXPORTER-capsule-peering-artifact-v1";

/// The DNS name the client presents. The server verifier accepts any name — trust is the
/// app-layer hybrid check — so this is only a syntactically-valid placeholder.
const PEER_SNI: &str = "capsule-peer.local";

/// Frame ceiling for the handshake hello (a `device_id`, a protocol string, and a hybrid
/// signature) and for every control frame after it. Bounds a malicious length prefix.
pub const MAX_FRAME: usize = 64 * 1024;

/// The trust a device pins to authenticate its peers: the shared account's User IK and the
/// IK-signed [`DeviceDirectory`]. Both same-user devices pin the *same* directory; a foreign
/// device is simply absent from it, and a foreign directory fails to verify under our IK.
#[derive(Debug, Clone)]
pub struct PinnedTrust {
    /// The shared account User IK public key — the directory's signer.
    pub user_ik: HybridVerifyingKey,
    /// The shared, IK-signed device directory listing this user's enrolled devices.
    pub directory: DeviceDirectory,
}

impl PinnedTrust {
    /// The `directory_version` of the pinned directory.
    #[must_use]
    pub fn directory_version(&self) -> u64 {
        self.directory.core.directory_version
    }
}

/// Admit the pinned directory: it must chain to the pinned User IK, and its version must not be
/// below the highest this device has already verified (anti-rollback). An admitted version is
/// recorded, so the floor only ever rises.
pub(crate) fn admit_directory(
    trust: &PinnedTrust,
    versions: &dyn DirectoryVersions,
) -> Result<(), PeeringError> {
    if !trust.directory.verify(&trust.user_ik) {
        return Err(PeeringError::ForeignIdentity);
    }
    let pinned = trust.directory_version();
    let highest = versions.highest()?;
    if pinned < highest {
        tracing::warn!(
            pinned,
            highest,
            "pinned device directory is older than one already verified; refusing"
        );
        return Err(PeeringError::DirectoryRollback { pinned, highest });
    }
    versions.record(pinned)?;
    Ok(())
}

/// The directory entry for `device_id`, which must be present and un-revoked.
pub(crate) fn admit_device(
    trust: &PinnedTrust,
    device_id: Uuid,
) -> Result<&DeviceEntry, PeeringError> {
    let entry = trust
        .directory
        .device(&device_id)
        .ok_or(PeeringError::UnknownDevice(device_id))?;
    if entry.revoked_at.is_some() {
        return Err(PeeringError::RevokedDevice(device_id));
    }
    Ok(entry)
}

/// A device's supported peering-protocol window, `[min, max]`, both `YYYY-MM-DD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionWindow {
    /// The oldest peering protocol version spoken.
    pub min: String,
    /// The newest peering protocol version spoken.
    pub max: String,
}

/// Which device must update for two disjoint windows to meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateDevice {
    /// This device's window lies wholly below the peer's: update this device.
    ThisDevice,
    /// The peer's window lies wholly below this device's: update the peer.
    Peer,
}

impl VersionWindow {
    /// This build's window, [`PEERING_PROTOCOL_MIN`]`..=`[`PEERING_PROTOCOL`].
    #[must_use]
    pub fn ours() -> Self {
        Self {
            min: PEERING_PROTOCOL_MIN.to_string(),
            max: PEERING_PROTOCOL.to_string(),
        }
    }

    /// A window over two protocol dates. Refuses a value that is not `YYYY-MM-DD`, or a
    /// `min` above `max`, as a codec error: such a hello cannot be negotiated against.
    pub fn new(min: &str, max: &str) -> Result<Self, PeeringError> {
        if !is_protocol_date(min) || !is_protocol_date(max) || min > max {
            return Err(PeeringError::Codec(format!(
                "malformed peering protocol window {min:?}..={max:?}"
            )));
        }
        Ok(Self {
            min: min.to_string(),
            max: max.to_string(),
        })
    }

    /// The highest version both windows contain, or [`PeeringError::VersionDisjoint`] naming
    /// which device must update. Dates in `YYYY-MM-DD` order lexically.
    pub fn negotiate(&self, theirs: &Self) -> Result<String, PeeringError> {
        let high = self.max.as_str().min(theirs.max.as_str());
        let low = self.min.as_str().max(theirs.min.as_str());
        if high < low {
            let update = if self.max < theirs.min {
                UpdateDevice::ThisDevice
            } else {
                UpdateDevice::Peer
            };
            return Err(PeeringError::VersionDisjoint {
                ours: self.clone(),
                theirs: theirs.clone(),
                update,
            });
        }
        Ok(high.to_string())
    }
}

impl fmt::Display for VersionWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..={}", self.min, self.max)
    }
}

/// The 256-bit secret that seals a peering sync artifact, derived from the channel's TLS exporter
/// under its own peering label. Fresh per session, never stored, never a user passphrase; its
/// `Debug` never prints the bytes.
#[derive(Clone)]
pub struct ArtifactSecret([u8; 32]);

impl ArtifactSecret {
    /// The secret's bytes, as the backup wrap takes them.
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// A secret over explicit bytes — only tests, which have no TLS session to export from.
    #[cfg(test)]
    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl fmt::Debug for ArtifactSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ArtifactSecret(..)")
    }
}

/// What this device brings to a handshake: who it is, the key it proves that with, the trust it
/// pins, and where it keeps the highest directory version it has verified.
#[derive(Clone, Copy)]
pub struct LocalPeer<'a> {
    /// This device's directory id.
    pub device_id: Uuid,
    /// This device's hybrid device-key signer.
    pub signer: &'a dyn Signer,
    /// The trust this device pins.
    pub trust: &'a PinnedTrust,
    /// The persisted anti-rollback floor.
    pub versions: &'a dyn DirectoryVersions,
}

/// A peer whose hybrid identity was verified over the established channel.
#[derive(Debug, Clone)]
pub struct VerifiedPeer {
    /// The peer's directory `device_id`, proven to hold the matching device key.
    pub device_id: Uuid,
    /// The peering protocol version the pair negotiated: the highest both windows contain.
    pub protocol: String,
    /// The channel-binding exporter material the proof covered (returned for provenance/tests).
    pub exporter: [u8; EXPORTER_LEN],
    /// The secret that seals this session's sync artifacts.
    pub artifact_secret: ArtifactSecret,
}

/// The handshake hello each side sends over the encrypted channel: who it claims to be, the
/// peering protocol window it speaks, the directory version it pins, and a hybrid signature over
/// the channel-binding exporter material.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerHello {
    /// The sender's directory device id.
    pub device_id: Uuid,
    /// The newest peering transport protocol version the sender speaks (its window's max).
    pub peering_protocol: String,
    /// The oldest version the sender speaks. Absent from a hello older than the window, which
    /// spoke exactly `peering_protocol`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peering_protocol_min: Option<String>,
    /// The `directory_version` of the directory the sender pins. Absent (`0`) from a hello older
    /// than the window.
    #[serde(default)]
    pub directory_version: u64,
    /// A hybrid signature over the TLS exporter material — the proof this device holds the key
    /// published for `device_id`.
    pub proof: HybridSignature,
}

impl PeerHello {
    /// The window this hello advertises.
    pub fn window(&self) -> Result<VersionWindow, PeeringError> {
        let max = self.peering_protocol.as_str();
        VersionWindow::new(self.peering_protocol_min.as_deref().unwrap_or(max), max)
    }
}

/// Which end of the channel a hello was sent from. The role is part of what the hybrid proof
/// signs, so a hello one end sent never verifies as the other end's: a listener cannot reflect a
/// dialer's own hello back at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeRole {
    /// The device that dialed ([`connect`]).
    Dialer,
    /// The device that accepted ([`accept`]).
    Listener,
}

impl HandshakeRole {
    /// The role at the other end of the channel.
    #[must_use]
    pub fn peer(self) -> Self {
        match self {
            Self::Dialer => Self::Listener,
            Self::Listener => Self::Dialer,
        }
    }

    fn tag(self) -> &'static [u8] {
        match self {
            Self::Dialer => b"dialer",
            Self::Listener => b"listener",
        }
    }
}

/// The domain prefix of the message a hello's proof signs.
const HELLO_PROOF_CONTEXT: &[u8] = b"capsule-peering-hello-v1/";

/// The exact bytes a hello sent by `role` signs: a domain prefix, the sender's role, a
/// separator, and the channel-binding `exporter`. Binding the role means the two hellos of one
/// session sign different messages even though both ends derive the same exporter.
pub(crate) fn hello_proof_message(role: HandshakeRole, exporter: &[u8; EXPORTER_LEN]) -> Vec<u8> {
    let tag = role.tag();
    let mut message = Vec::with_capacity(HELLO_PROOF_CONTEXT.len() + tag.len() + 1 + EXPORTER_LEN);
    message.extend_from_slice(HELLO_PROOF_CONTEXT);
    message.extend_from_slice(tag);
    message.push(0);
    message.extend_from_slice(exporter);
    message
}

/// Build this device's hello as `role`: sign the role-bound channel-binding `exporter` with the
/// device `signer`.
fn build_hello(
    local: &LocalPeer<'_>,
    role: HandshakeRole,
    exporter: &[u8; EXPORTER_LEN],
) -> Result<PeerHello, PeeringError> {
    let proof = local.signer.sign(&hello_proof_message(role, exporter))?;
    Ok(PeerHello {
        device_id: local.device_id,
        peering_protocol: PEERING_PROTOCOL.to_string(),
        peering_protocol_min: Some(PEERING_PROTOCOL_MIN.to_string()),
        directory_version: local.trust.directory_version(),
        proof,
    })
}

/// The application-layer hybrid check — the whole security of the channel. Run **before any
/// payload byte** against the locally-pinned trust. In order:
///
/// 1. **Version window.** The pair speaks the highest common version; disjoint windows abort
///    with [`PeeringError::VersionDisjoint`] before any identity check.
/// 2. **Directory chains to our IK.** The pinned directory must verify under our pinned User IK;
///    a directory signed by a foreign IK is rejected outright.
/// 3. **Anti-rollback.** The pinned directory must not be older than the highest this device has
///    verified, and the peer must not present a version older than the one we pin.
/// 4. **Not ourselves.** A hello claiming `local_device`, our own id, is refused with
///    [`PeeringError::SelfPeer`]: a device never peers with itself.
/// 5. **Enrolled + not revoked.** The claimed `device_id` must be present and un-revoked.
/// 6. **Hybrid proof.** The peer's published device key must verify the hybrid signature over the
///    exact exporter material *we* derived, bound to `from`: the role the peer holds on this
///    channel, the opposite of ours. That binds the hybrid identity to
///    this TLS session and to its far end, so a hello reflected back at its sender fails.
///
/// Returns the verified device id and the negotiated protocol version.
pub fn verify_hello(
    hello: &PeerHello,
    from: HandshakeRole,
    exporter: &[u8; EXPORTER_LEN],
    local_device: Uuid,
    trust: &PinnedTrust,
    versions: &dyn DirectoryVersions,
) -> Result<(Uuid, String), PeeringError> {
    let protocol = VersionWindow::ours().negotiate(&hello.window()?)?;
    admit_directory(trust, versions)?;
    let ours = trust.directory_version();
    if hello.directory_version < ours {
        tracing::warn!(
            device = %hello.device_id,
            theirs = hello.directory_version,
            ours,
            "peer presented an older device directory; refusing"
        );
        return Err(PeeringError::PeerDirectoryStale {
            theirs: hello.directory_version,
            ours,
        });
    }
    if hello.device_id == local_device {
        tracing::warn!(
            device = %hello.device_id,
            "peer hello claims this device's own id; refusing"
        );
        return Err(PeeringError::SelfPeer(local_device));
    }
    let entry = admit_device(trust, hello.device_id)?;
    if !entry
        .dsk_public
        .verify(&hello_proof_message(from, exporter), &hello.proof)
    {
        return Err(PeeringError::HybridCheckFailed);
    }
    tracing::debug!(
        device = %hello.device_id,
        %protocol,
        "peer hybrid identity verified over the channel"
    );
    Ok((hello.device_id, protocol))
}

// ── mTLS config (CA-less; accept-any classical verifiers) ─────────────────────

/// The pinned crypto provider (ring), so the peering stack never depends on an ambiguous
/// process-default `CryptoProvider` when another (aws-lc-rs, via reqwest) is also linked.
fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A fresh per-connection self-signed leaf + key. The certificate carries no trust — the peer's
/// accept-any verifier accepts it and the hybrid check decides identity — so an ephemeral cert
/// is exactly right.
fn self_signed() -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>), PeeringError> {
    let certified = rcgen::generate_simple_self_signed(vec![PEER_SNI.to_string()])
        .map_err(|e| PeeringError::Tls(e.to_string()))?;
    let cert = CertificateDer::from(certified.cert.der().to_vec());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der()));
    Ok((cert, key))
}

/// A rustls server config: TLS 1.3 only, **mandatory** client auth, accept-any client cert.
fn server_config() -> Result<ServerConfig, PeeringError> {
    let (cert, key) = self_signed()?;
    let p = provider();
    let verifier = Arc::new(AcceptAnyPeerCert {
        provider: p.clone(),
    });
    ServerConfig::builder_with_provider(p)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| PeeringError::Tls(e.to_string()))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![cert], key)
        .map_err(|e| PeeringError::Tls(e.to_string()))
}

/// A rustls client config: TLS 1.3 only, accept-any server cert, presenting our client cert.
fn client_config() -> Result<ClientConfig, PeeringError> {
    let (cert, key) = self_signed()?;
    let p = provider();
    let verifier = Arc::new(AcceptAnyPeerCert {
        provider: p.clone(),
    });
    ClientConfig::builder_with_provider(p)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| PeeringError::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![cert], key)
        .map_err(|e| PeeringError::Tls(e.to_string()))
}

/// The accept-any verifier used on **both** ends. It deliberately accepts every well-formed peer
/// certificate: in a CA-less design the certificate proves only classical key possession, and
/// *identity* is decided by the app-layer hybrid check. Signature checks (that the peer actually
/// holds the presented cert's key) are still delegated to the crypto provider — the handshake
/// remains a real mutual TLS 1.3 handshake.
#[derive(Debug)]
struct AcceptAnyPeerCert {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for AcceptAnyPeerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl ClientCertVerifier for AcceptAnyPeerCert {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }

    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }
}

// ── Handshake drivers ─────────────────────────────────────────────────────────

fn io_err(e: std::io::Error) -> PeeringError {
    PeeringError::Io(e.to_string())
}

async fn write_frame<W>(w: &mut W, payload: &[u8]) -> Result<(), PeeringError>
where
    W: AsyncWriteExt + Unpin,
{
    let len = u32::try_from(payload.len())
        .map_err(|_| PeeringError::Codec("hello frame exceeds u32".into()))?;
    w.write_u32(len).await.map_err(io_err)?;
    w.write_all(payload).await.map_err(io_err)?;
    w.flush().await.map_err(io_err)?;
    Ok(())
}

async fn read_frame<R>(r: &mut R) -> Result<Vec<u8>, PeeringError>
where
    R: AsyncReadExt + Unpin,
{
    let len = r.read_u32().await.map_err(io_err)? as usize;
    if len > MAX_FRAME {
        return Err(PeeringError::Codec("hello frame too large".into()));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await.map_err(io_err)?;
    Ok(buf)
}

fn encode(hello: &PeerHello) -> Result<Vec<u8>, PeeringError> {
    serde_json::to_vec(hello).map_err(|e| PeeringError::Codec(e.to_string()))
}

fn decode(bytes: &[u8]) -> Result<PeerHello, PeeringError> {
    serde_json::from_slice(bytes).map_err(|e| PeeringError::Codec(e.to_string()))
}

/// Derive both exporter values from an established TLS session: the channel-binding material the
/// hybrid proof signs, and the [`ArtifactSecret`] under its own label.
fn derive_exporters(
    export: impl Fn(&[u8]) -> Result<[u8; EXPORTER_LEN], rustls::Error>,
) -> Result<([u8; EXPORTER_LEN], ArtifactSecret), PeeringError> {
    let tls = |e: rustls::Error| PeeringError::Tls(e.to_string());
    let binding = export(EXPORTER_LABEL).map_err(tls)?;
    let secret = ArtifactSecret(export(ARTIFACT_EXPORTER_LABEL).map_err(tls)?);
    Ok((binding, secret))
}

/// Verify the hello the peer sent while we hold `role`, and assemble the [`VerifiedPeer`].
fn finish(
    peer_bytes: &[u8],
    role: HandshakeRole,
    exporter: [u8; EXPORTER_LEN],
    artifact_secret: ArtifactSecret,
    local: &LocalPeer<'_>,
) -> Result<VerifiedPeer, PeeringError> {
    let peer_hello = decode(peer_bytes)?;
    let (device_id, protocol) = verify_hello(
        &peer_hello,
        role.peer(),
        &exporter,
        local.device_id,
        local.trust,
        local.versions,
    )?;
    Ok(VerifiedPeer {
        device_id,
        protocol,
        exporter,
        artifact_secret,
    })
}

/// Run `step`, refusing with [`PeeringError::Timeout`] naming `what` once `limit` passes.
async fn within<T>(
    limit: Duration,
    what: &'static str,
    step: impl Future<Output = Result<T, PeeringError>>,
) -> Result<T, PeeringError> {
    tokio::time::timeout(limit, step)
        .await
        .map_err(|_| PeeringError::Timeout(what))?
}

/// **Server side** of the peering handshake. Admits the source address through `limiter`
/// (refusing with [`PeeringError::RateLimited`] before any TLS byte), completes a real mutual
/// TLS 1.3 handshake over `tcp`, derives the channel-binding exporter, exchanges hellos
/// (read-then-write, so the pair never deadlocks), and runs the [`verify_hello`] check, all
/// within [`HANDSHAKE_TIMEOUT`]. Returns the verified channel or the specific rejection — a
/// wrong/revoked/foreign/stale peer fails the whole handshake before any payload, and no channel
/// exists to send one over.
#[instrument(skip(tcp, local, limiter), fields(local = %local.device_id))]
pub async fn accept(
    tcp: TcpStream,
    local: LocalPeer<'_>,
    limiter: &HandshakeLimiter,
) -> Result<PeerChannel, PeeringError> {
    let source = tcp.peer_addr().map_err(io_err)?.ip();
    if !limiter.admit(source, Instant::now()) {
        tracing::warn!(%source, "peering handshake rate limit reached; refusing");
        return Err(PeeringError::RateLimited(source));
    }
    crate::net::mark_lower_effort(&tcp);
    let acceptor = TlsAcceptor::from(Arc::new(server_config()?));
    within(HANDSHAKE_TIMEOUT, "the handshake", async {
        let mut tls = acceptor.accept(tcp).await.map_err(io_err)?;

        let (exporter, secret) = {
            let (_io, conn) = tls.get_ref();
            derive_exporters(|label| conn.export_keying_material([0u8; EXPORTER_LEN], label, None))?
        };

        let peer_bytes = read_frame(&mut tls).await?;
        let our_hello = build_hello(&local, HandshakeRole::Listener, &exporter)?;
        write_frame(&mut tls, &encode(&our_hello)?).await?;

        let peer = finish(
            &peer_bytes,
            HandshakeRole::Listener,
            exporter,
            secret,
            &local,
        )?;
        Ok(PeerChannel::new(TlsStream::Server(tls), peer))
    })
    .await
}

/// **Client side** of the peering handshake. The dialing device connects, drives the TLS 1.3
/// handshake to completion, derives the same exporters, exchanges hellos (write-then-read), and
/// runs the hybrid check, all within [`HANDSHAKE_TIMEOUT`]. See [`accept`] for the security
/// contract.
#[instrument(skip(tcp, local), fields(local = %local.device_id))]
pub async fn connect(tcp: TcpStream, local: LocalPeer<'_>) -> Result<PeerChannel, PeeringError> {
    crate::net::mark_lower_effort(&tcp);
    let connector = TlsConnector::from(Arc::new(client_config()?));
    let name = ServerName::try_from(PEER_SNI)
        .map_err(|e| PeeringError::Tls(e.to_string()))?
        .to_owned();
    within(HANDSHAKE_TIMEOUT, "the handshake", async {
        let mut tls = connector.connect(name, tcp).await.map_err(io_err)?;

        let (exporter, secret) = {
            let (_io, conn) = tls.get_ref();
            derive_exporters(|label| conn.export_keying_material([0u8; EXPORTER_LEN], label, None))?
        };

        let our_hello = build_hello(&local, HandshakeRole::Dialer, &exporter)?;
        write_frame(&mut tls, &encode(&our_hello)?).await?;
        let peer_bytes = read_frame(&mut tls).await?;

        let peer = finish(&peer_bytes, HandshakeRole::Dialer, exporter, secret, &local)?;
        Ok(PeerChannel::new(TlsStream::Client(tls), peer))
    })
    .await
}

// ── The verified channel ──────────────────────────────────────────────────────

/// An established, verified peering channel: the TLS 1.3 stream the handshake completed, bound to
/// the peer whose hybrid identity it verified. Every frame after the handshake travels inside
/// this TLS session and nowhere else.
pub struct PeerChannel {
    stream: TlsStream<TcpStream>,
    peer: VerifiedPeer,
    io_timeout: Duration,
    /// Whether the opening offer has been sent or read, or the protocol has none.
    opened: bool,
}

impl fmt::Debug for PeerChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PeerChannel")
            .field("peer", &self.peer.device_id)
            .field("protocol", &self.peer.protocol)
            .field("io_timeout", &self.io_timeout)
            .finish_non_exhaustive()
    }
}

impl PeerChannel {
    fn new(stream: TlsStream<TcpStream>, peer: VerifiedPeer) -> Self {
        tracing::debug!(peer = %peer.device_id, protocol = %peer.protocol, "peering channel established");
        let opened = !speaks_offers(&peer.protocol);
        Self {
            stream,
            peer,
            io_timeout: DEFAULT_IO_TIMEOUT,
            opened,
        }
    }

    /// Test-only: speak `protocol` as if the pair had negotiated it, to stand in for an older
    /// peer on a real channel.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn speaking(mut self, protocol: &str) -> Self {
        self.peer.protocol = protocol.to_owned();
        self.opened = !speaks_offers(protocol);
        self
    }

    /// The serving side: read the pulling side's opening offer, before anything is listed.
    ///
    /// `None` when the pair negotiated a protocol below [`OFFER_EXCHANGE_PROTOCOL`] (an older
    /// peer sends no opening, and nothing is read), or when the peer said it offers nothing. An
    /// offer is at most [`MAX_OFFER_PAGES`] pages, each a data frame decoded by
    /// [`decode_offer_page`], and all of them must carry the same cursor.
    ///
    /// # Errors
    ///
    /// [`PeeringError::Codec`] for an opening read twice, a frame that is not an offer, too
    /// many pages, or a malformed page; [`PeeringError::PeerRefused`] when the peer refused the
    /// session; or the channel's I/O or timeout failure.
    #[instrument(skip_all, fields(peer = %self.peer.device_id))]
    pub async fn receive_offer(&mut self) -> Result<Option<Offer>, PeeringError> {
        if !speaks_offers(&self.peer.protocol) {
            tracing::debug!(protocol = %self.peer.protocol, "peering: the peer's protocol carries no offer");
            return Ok(None);
        }
        if self.opened {
            return Err(PeeringError::Codec(
                "the opening offer was already exchanged".into(),
            ));
        }
        self.opened = true;
        let pages = match self.recv().await? {
            Message::Offer { pages } => pages as usize,
            Message::Refused { kind, message } => {
                return Err(PeeringError::PeerRefused { kind, message });
            }
            other => {
                return Err(PeeringError::Codec(format!(
                    "expected an offer, the peer sent {}",
                    other.name()
                )));
            }
        };
        if pages == 0 {
            tracing::debug!("peering: the peer offers nothing");
            return Ok(None);
        }
        if pages > MAX_OFFER_PAGES {
            return Err(PeeringError::Codec(format!(
                "an offer of {pages} pages exceeds the {MAX_OFFER_PAGES}-page ceiling"
            )));
        }
        let mut decoded = Vec::with_capacity(pages);
        for _ in 0..pages {
            let page = self.recv_data(MAX_OFFER_PAGE_BYTES).await?;
            decoded.push(decode_offer_page(&page)?);
        }
        let offer = Offer::from_pages(&decoded)?;
        tracing::info!(
            pages,
            addresses = offer.addresses.len(),
            cursor = offer.cursor,
            "peering: the peer's offer received"
        );
        Ok(Some(offer))
    }

    /// The pulling side: send the opening — `offer`'s pages, or that it offers nothing — when
    /// the pair's protocol carries one and it has not been sent. Returns whether an opening
    /// went: the serving side builds its listing only once it has read one, whether it offers
    /// pages or nothing (an offer over the page ceiling degrades to nothing), so the caller
    /// waits for the listing with [`LISTING_BUILD_ALLOWANCE`] in either case.
    async fn send_opening(&mut self, offer: Option<&Offer>) -> Result<bool, PeeringError> {
        if self.opened {
            return Ok(false);
        }
        self.opened = true;
        let pages = match offer.map(Offer::encode_pages) {
            Some(pages) if pages.len() > MAX_OFFER_PAGES => {
                // A whole-library listing is still correct (ingest makes a held asset a no-op),
                // so an offer too large to carry degrades to none rather than ending the session.
                tracing::warn!(
                    pages = pages.len(),
                    "peering: the offer exceeds the page ceiling; offering nothing"
                );
                Vec::new()
            }
            Some(pages) => pages,
            None => Vec::new(),
        };
        // `pages.len() <= MAX_OFFER_PAGES`, which fits a u32.
        self.send(&Message::Offer {
            pages: pages.len() as u32,
        })
        .await?;
        for page in &pages {
            self.send_data(page).await?;
        }
        tracing::debug!(pages = pages.len(), "peering: opening sent");
        Ok(true)
    }

    /// The peer the handshake verified. Every authorization on this channel is decided against
    /// this identity, never against a device id a frame claims.
    #[must_use]
    pub fn peer(&self) -> &VerifiedPeer {
        &self.peer
    }

    /// The verified peer, dropping the stream (closing the channel).
    #[must_use]
    pub fn into_peer(self) -> VerifiedPeer {
        self.peer
    }

    /// Bound every later frame read and write by `timeout` instead of [`DEFAULT_IO_TIMEOUT`].
    #[must_use]
    pub fn with_io_timeout(mut self, timeout: Duration) -> Self {
        self.io_timeout = timeout;
        self
    }

    /// Send one control message.
    pub(crate) async fn send(&mut self, message: &Message) -> Result<(), PeeringError> {
        let payload =
            serde_json::to_vec(message).map_err(|e| PeeringError::Codec(e.to_string()))?;
        if payload.len() > MAX_FRAME {
            return Err(PeeringError::Codec(
                "control frame exceeds its ceiling".into(),
            ));
        }
        tracing::trace!(peer = %self.peer.device_id, kind = message.name(), "peering: send");
        self.write_tagged(TAG_CONTROL, &payload).await
    }

    /// Receive one control message.
    pub(crate) async fn recv(&mut self) -> Result<Message, PeeringError> {
        self.recv_within(self.io_timeout).await
    }

    /// Receive one control message, waiting at most `limit` for the whole frame rather than the
    /// channel's I/O timeout.
    pub(crate) async fn recv_within(&mut self, limit: Duration) -> Result<Message, PeeringError> {
        let payload = self.read_tagged(TAG_CONTROL, MAX_FRAME, limit).await?;
        let message: Message =
            serde_json::from_slice(&payload).map_err(|e| PeeringError::Codec(e.to_string()))?;
        tracing::trace!(peer = %self.peer.device_id, kind = message.name(), "peering: received");
        Ok(message)
    }

    /// Send one data frame (at most [`MAX_DATA_FRAME`] bytes).
    pub(crate) async fn send_data(&mut self, bytes: &[u8]) -> Result<(), PeeringError> {
        if bytes.len() > MAX_DATA_FRAME {
            return Err(PeeringError::Codec("data frame exceeds its ceiling".into()));
        }
        self.write_tagged(TAG_DATA, bytes).await
    }

    /// Receive one data frame of at most `at_most` bytes.
    pub(crate) async fn recv_data(&mut self, at_most: usize) -> Result<Vec<u8>, PeeringError> {
        self.read_tagged(TAG_DATA, at_most.min(MAX_DATA_FRAME), self.io_timeout)
            .await
    }

    /// Close the sending half, so the peer reads end-of-stream rather than waiting out its
    /// timeout. Best effort: the channel is being dropped either way.
    pub(crate) async fn close(&mut self) {
        let timeout = self.io_timeout;
        let _closed = tokio::time::timeout(timeout, self.stream.shutdown()).await;
    }

    async fn write_tagged(&mut self, tag: u8, payload: &[u8]) -> Result<(), PeeringError> {
        let len = u32::try_from(payload.len())
            .map_err(|_| PeeringError::Codec("frame exceeds u32".into()))?;
        let stream = &mut self.stream;
        within(self.io_timeout, "writing a frame", async {
            stream.write_u8(tag).await.map_err(io_err)?;
            stream.write_u32(len).await.map_err(io_err)?;
            stream.write_all(payload).await.map_err(io_err)?;
            stream.flush().await.map_err(io_err)
        })
        .await
    }

    /// Read one frame within `limit`, which must carry `tag` and at most `max` bytes. The length
    /// is checked before any of the payload is read or allocated.
    async fn read_tagged(
        &mut self,
        tag: u8,
        max: usize,
        limit: Duration,
    ) -> Result<Vec<u8>, PeeringError> {
        let stream = &mut self.stream;
        within(limit, "reading a frame", async {
            let found = stream.read_u8().await.map_err(io_err)?;
            if found != tag {
                return Err(PeeringError::Codec(format!(
                    "expected a frame tagged {tag}, the peer sent {found}"
                )));
            }
            let len = stream.read_u32().await.map_err(io_err)? as usize;
            if len > max {
                return Err(PeeringError::Codec(format!(
                    "a {len}-byte frame exceeds the {max}-byte ceiling"
                )));
            }
            let mut payload = vec![0u8; len];
            stream.read_exact(&mut payload).await.map_err(io_err)?;
            Ok(payload)
        })
        .await
    }
}

/// One artifact a serving peer offers: its content address and length. Both are the server's
/// claims until the pulled bytes hash to the address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offered {
    /// The artifact's content address (SHA-256, lowercase hex).
    pub address: String,
    /// Its length in bytes.
    pub len: u64,
}

/// The control messages the channel carries after the handshake. From
/// [`OFFER_EXCHANGE_PROTOCOL`] the pulling side opens with [`Offer`](Self::Offer); the serving
/// side then sends [`Listing`](Self::Listing); the pulling side then sends requests, each
/// answered before the next is sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// Client → server, first, from [`OFFER_EXCHANGE_PROTOCOL`]: the pulling side's offer
    /// follows in `pages` data frames, each one padded page ([`Offer::encode_pages`]). `0`
    /// says it offers nothing, and takes whatever is listed.
    Offer {
        /// How many page frames follow, at most [`MAX_OFFER_PAGES`].
        pages: u32,
    },
    /// Server → client: the artifacts this session serves. Nothing else is served.
    Listing {
        /// The offered artifacts.
        artifacts: Vec<Offered>,
    },
    /// Client → server: bytes of `address` from `start`, at most `max_len` (or [`MAX_RANGE`]).
    Range {
        /// The artifact's content address.
        address: String,
        /// The first byte wanted.
        start: u64,
        /// The most bytes wanted; the server never sends more than [`MAX_RANGE`].
        max_len: Option<u64>,
    },
    /// Server → client: `len` bytes follow, in data frames.
    RangeReply {
        /// How many bytes follow.
        len: u64,
    },
    /// Client → server: the delegate verified and durably persisted a hand-off.
    Ack {
        /// The acknowledgement.
        ack: HandOffAck,
    },
    /// Server → client: the acknowledgement was taken.
    Acked,
    /// Server → client: the request was refused. `kind` is a stable name
    /// ([`PeeringError::kind`], [`DelegationError::kind`]).
    Refused {
        /// The stable name of the refusal.
        kind: String,
        /// The English detail.
        message: String,
    },
    /// Client → server: the session is over.
    Done,
}

impl Message {
    /// The message's type, for logs.
    fn name(&self) -> &'static str {
        match self {
            Self::Offer { .. } => "offer",
            Self::Listing { .. } => "listing",
            Self::Range { .. } => "range",
            Self::RangeReply { .. } => "range_reply",
            Self::Ack { .. } => "ack",
            Self::Acked => "acked",
            Self::Refused { .. } => "refused",
            Self::Done => "done",
        }
    }
}

/// Whether a channel negotiated at `protocol` carries the opening offer. Dates in `YYYY-MM-DD`
/// order lexically.
fn speaks_offers(protocol: &str) -> bool {
    protocol >= OFFER_EXCHANGE_PROTOCOL
}

/// Whether `address` is a SHA-256 content address in its canonical spelling.
fn is_address(address: &str) -> bool {
    address.len() == 64
        && address
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// ── Serving ───────────────────────────────────────────────────────────────────

/// The artifacts one serving session offers, each under the content address this device
/// computed over its bytes.
#[derive(Default)]
pub struct Offering {
    artifacts: BTreeMap<String, Bytes>,
    /// Test-only: sever the channel once this many data bytes have been sent.
    #[cfg(test)]
    pub(crate) sever_after: Option<u64>,
}

impl fmt::Debug for Offering {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.listing()).finish()
    }
}

impl Offering {
    /// An empty offering.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer `bytes`, returning the content address it is served under. The offering shares
    /// `bytes` rather than copying them: a `Vec<u8>` converts without a copy, and a [`Bytes`]
    /// the caller keeps is the same allocation.
    ///
    /// # Errors
    ///
    /// [`PeeringError::Codec`] past [`MAX_LISTING`] artifacts or for an artifact over
    /// [`MAX_ARTIFACT_LEN`].
    pub fn offer(&mut self, bytes: impl Into<Bytes>) -> Result<String, PeeringError> {
        let bytes = bytes.into();
        if bytes.len() as u64 > MAX_ARTIFACT_LEN {
            return Err(PeeringError::Codec(
                "an artifact exceeds the offer ceiling".into(),
            ));
        }
        let address = super::transfer::artifact_address(&bytes);
        if !self.artifacts.contains_key(&address) && self.artifacts.len() >= MAX_LISTING {
            return Err(PeeringError::Codec(format!(
                "an offering lists at most {MAX_LISTING} artifacts"
            )));
        }
        self.artifacts.insert(address.clone(), bytes);
        Ok(address)
    }

    /// Test-only: the bytes served under `address`.
    #[cfg(test)]
    pub(crate) fn served(&self, address: &str) -> Option<&Bytes> {
        self.artifacts.get(address)
    }

    /// Whether `address` is offered.
    #[must_use]
    pub fn contains(&self, address: &str) -> bool {
        self.artifacts.contains_key(address)
    }

    /// What the listing frame carries.
    #[must_use]
    pub fn listing(&self) -> Vec<Offered> {
        self.artifacts
            .iter()
            .map(|(address, bytes)| Offered {
                address: address.clone(),
                len: bytes.len() as u64,
            })
            .collect()
    }

    /// How long a serving session waits for the peer's next request once `acks` are taken:
    /// `io_timeout`, plus the largest offered artifact no acknowledgement names yet at
    /// [`MIN_PERSIST_RATE`] — the peer may be verifying and persisting it before it asks again.
    /// Once every artifact is acknowledged, the wait is `io_timeout` alone.
    pub(crate) fn request_budget(&self, io_timeout: Duration, acks: &[HandOffAck]) -> Duration {
        let pending = self
            .artifacts
            .iter()
            .filter(|(address, _)| !acks.iter().any(|ack| &ack.hand_off == *address))
            .map(|(_, bytes)| bytes.len() as u64)
            .max()
            .unwrap_or(0);
        io_timeout.saturating_add(Duration::from_millis(
            pending.saturating_mul(1000) / MIN_PERSIST_RATE,
        ))
    }
}

/// One ranged answer a serving session sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedRange {
    /// The artifact.
    pub address: String,
    /// The first byte sent.
    pub start: u64,
    /// How many bytes were sent.
    pub len: u64,
}

/// What a serving session did.
#[derive(Debug, Clone, Default)]
pub struct ServeReport {
    /// Every ranged answer, in order.
    pub served: Vec<ServedRange>,
    /// Every acknowledgement taken, in order.
    pub acks: Vec<HandOffAck>,
}

/// Serve `offering` to the channel's verified peer until it says it is done.
///
/// Reads the peer's opening offer first when the caller has not ([`PeerChannel::receive_offer`])
/// and sets it aside, then sends the listing, then answers each request in turn:
///
/// - **Every request is authorized first** against the trust `trust` pins now
///   ([`PeerSession::authorize`]): a peer revoked, or gone from the directory, since the
///   handshake is refused and the session ends — as it does when `trust` cannot produce current
///   trust (fail closed).
/// - **A ranged pull** is answered only for an offered address, with at most [`MAX_RANGE`]
///   bytes in data frames of at most [`MAX_DATA_FRAME`].
/// - **An acknowledgement** is taken only when it names the verified peer as the delegate and an
///   offered address; then `on_ack` decides it. Its refusal is reported to the peer and the
///   session goes on.
/// - **The wait for each request** is the channel's I/O timeout plus the largest
///   unacknowledged offered artifact at [`MIN_PERSIST_RATE`]: the peer verifies and persists a
///   pulled artifact before it acknowledges it, and that takes longer than one frame's timeout
///   for a large artifact on slow storage.
///
/// # Errors
///
/// The [`PeeringError`] that ended the session: an authorization refusal, a malformed or
/// unexpected frame, a timeout, or the channel failing.
#[instrument(skip_all, fields(peer = %channel.peer.device_id, offered = offering.artifacts.len()))]
pub async fn serve<T, F>(
    channel: &mut PeerChannel,
    offering: &Offering,
    trust: &T,
    versions: &dyn DirectoryVersions,
    mut on_ack: F,
) -> Result<ServeReport, PeeringError>
where
    T: TrustSource,
    F: FnMut(&HandOffAck) -> Result<(), DelegationError> + Send,
{
    let mut session = PeerSession::new(&channel.peer);
    let mut report = ServeReport::default();
    #[cfg(test)]
    let mut sent_data: u64 = 0;
    if !channel.opened {
        // The caller cut its offering without the peer's offer (a delegation does), so the
        // opening is read and set aside: the offering is what this session serves either way.
        let offer = channel.receive_offer().await?;
        tracing::debug!(
            offered = offer.as_ref().map(|offer| offer.addresses.len()),
            "peering: an opening offer set aside"
        );
    }
    channel
        .send(&Message::Listing {
            artifacts: offering.listing(),
        })
        .await?;
    loop {
        let budget = offering.request_budget(channel.io_timeout, &report.acks);
        tracing::trace!(?budget, "peering: awaiting the next request");
        let request = channel.recv_within(budget).await?;
        if request == Message::Done {
            tracing::info!(
                served = report.served.len(),
                acks = report.acks.len(),
                "peering: session done"
            );
            channel.close().await;
            return Ok(report);
        }
        let authorized = match trust.current().await {
            Ok(now) => session.authorize(&now, versions),
            Err(error) => Err(error),
        };
        if let Err(error) = authorized {
            tracing::warn!(%error, "peering: request refused; ending the session");
            refuse(channel, error.kind(), &error.to_string()).await;
            channel.close().await;
            return Err(error);
        }
        match request {
            Message::Range {
                address,
                start,
                max_len,
            } => {
                let Some(bytes) = offering.artifacts.get(&address) else {
                    refuse(channel, "not_offered", "the artifact is not offered").await;
                    continue;
                };
                let total = bytes.len() as u64;
                if start > total {
                    refuse(
                        channel,
                        "range_not_satisfiable",
                        "the range starts past the end",
                    )
                    .await;
                    continue;
                }
                let len = max_len
                    .unwrap_or(MAX_RANGE)
                    .min(MAX_RANGE)
                    .min(total - start);
                channel.send(&Message::RangeReply { len }).await?;
                let from = usize::try_from(start)
                    .map_err(|_| PeeringError::Codec("range start exceeds usize".into()))?;
                let to = from
                    + usize::try_from(len)
                        .map_err(|_| PeeringError::Codec("range exceeds usize".into()))?;
                for frame in bytes[from..to].chunks(MAX_DATA_FRAME) {
                    #[cfg(test)]
                    if let Some(limit) = offering.sever_after
                        && sent_data + frame.len() as u64 > limit
                    {
                        let keep = usize::try_from(limit.saturating_sub(sent_data)).unwrap_or(0);
                        channel.send_data(&frame[..keep]).await?;
                        channel.close().await;
                        return Err(PeeringError::Io("severed by the test".into()));
                    }
                    channel.send_data(frame).await?;
                    #[cfg(test)]
                    {
                        sent_data += frame.len() as u64;
                    }
                }
                tracing::debug!(%address, start, len, "peering: served a range");
                report.served.push(ServedRange {
                    address,
                    start,
                    len,
                });
            }
            Message::Ack { ack } => {
                let verdict = if ack.delegate_device != channel.peer.device_id
                    || !offering.contains(&ack.hand_off)
                {
                    // An acknowledgement speaks for the verified peer about what this session
                    // offered, or for nothing.
                    Err(DelegationError::AckMismatch)
                } else {
                    on_ack(&ack)
                };
                match verdict {
                    Ok(()) => {
                        channel.send(&Message::Acked).await?;
                        tracing::info!(asset_id = %ack.asset_id, hand_off = %ack.hand_off, "peering: acknowledgement taken");
                        report.acks.push(ack);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "peering: acknowledgement refused");
                        refuse(channel, error.kind(), &error.to_string()).await;
                    }
                }
            }
            other => {
                let error = PeeringError::Codec(format!(
                    "the peer sent {} where a request belongs",
                    other.name()
                ));
                refuse(channel, error.kind(), &error.to_string()).await;
                channel.close().await;
                return Err(error);
            }
        }
    }
}

/// Tell the peer its request was refused. Best effort: a channel that cannot carry the refusal
/// is ending anyway.
async fn refuse(channel: &mut PeerChannel, kind: &str, message: &str) {
    let refusal = Message::Refused {
        kind: kind.to_owned(),
        message: message.to_owned(),
    };
    if let Err(error) = channel.send(&refusal).await {
        tracing::debug!(%error, "peering: could not deliver a refusal");
    }
}

// ── Pulling ───────────────────────────────────────────────────────────────────

/// A ranged pull that failed, with the bytes that arrived before it did, so a resumed pull
/// starts after them.
#[derive(Debug)]
pub struct RangeFailure {
    /// The bytes received before the failure, from the requested start.
    pub received: Vec<u8>,
    /// Why it failed.
    pub error: PeeringError,
}

/// The pulling side of a verified channel: it reads the serving peer's listing, then makes
/// requests one at a time. Its requests take `&self`, so it can back a
/// [`BlobSource`](crate::fetch::BlobSource) ([`super::ChannelBlobSource`]).
///
/// Once any exchange fails part-way, the stream's framing can no longer be trusted: the client
/// is **broken** and refuses every later request rather than reading a stray frame as an answer.
pub struct PeerClient {
    channel: AsyncMutex<PeerChannel>,
    peer: VerifiedPeer,
    listing: Vec<Offered>,
    broken: AtomicBool,
}

impl fmt::Debug for PeerClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PeerClient")
            .field("peer", &self.peer.device_id)
            .field("listing", &self.listing)
            .field("broken", &self.is_broken())
            .finish_non_exhaustive()
    }
}

impl PeerClient {
    /// Read the serving peer's listing, first saying this device offers nothing where the
    /// pair's protocol carries an opening offer. A listing with more than [`MAX_LISTING`]
    /// artifacts, an address that is not a content address, or an artifact longer than
    /// [`MAX_ARTIFACT_LEN`] is refused whole.
    ///
    /// # Errors
    ///
    /// [`PeeringError::Codec`] for a malformed listing, [`PeeringError::PeerRefused`] when the
    /// peer refused the session, or the channel's I/O or timeout failure.
    pub async fn open(channel: PeerChannel) -> Result<Self, PeeringError> {
        Self::open_with(channel, None).await
    }

    /// As [`open`](Self::open), first sending `offer` — what this device holds and its cursor —
    /// so the serving side lists an artifact cut to what this device is missing. On a channel
    /// negotiated below [`OFFER_EXCHANGE_PROTOCOL`] nothing is sent, and the peer lists what it
    /// would have without an offer. Once an opening is sent — this offer, or nothing where the
    /// offer exceeds [`MAX_OFFER_PAGES`] — the listing is awaited for the I/O timeout plus
    /// [`LISTING_BUILD_ALLOWANCE`], while the peer cuts and seals its artifact; so is the
    /// listing [`open`](Self::open) awaits on a channel that carries an opening.
    ///
    /// # Errors
    ///
    /// As [`open`](Self::open), and the channel's failure while sending the offer.
    pub async fn open_offering(channel: PeerChannel, offer: &Offer) -> Result<Self, PeeringError> {
        Self::open_with(channel, Some(offer)).await
    }

    #[instrument(skip_all, fields(peer = %channel.peer.device_id, offering = offer.is_some()))]
    async fn open_with(
        mut channel: PeerChannel,
        offer: Option<&Offer>,
    ) -> Result<Self, PeeringError> {
        let offered = channel.send_opening(offer).await?;
        let wait = if offered {
            channel.io_timeout.saturating_add(LISTING_BUILD_ALLOWANCE)
        } else {
            channel.io_timeout
        };
        let listing = match channel.recv_within(wait).await? {
            Message::Listing { artifacts } => artifacts,
            Message::Refused { kind, message } => {
                return Err(PeeringError::PeerRefused { kind, message });
            }
            other => {
                return Err(PeeringError::Codec(format!(
                    "expected a listing, the peer sent {}",
                    other.name()
                )));
            }
        };
        if listing.len() > MAX_LISTING
            || listing
                .iter()
                .any(|offered| !is_address(&offered.address) || offered.len > MAX_ARTIFACT_LEN)
        {
            return Err(PeeringError::Codec(
                "the peer's listing is malformed".into(),
            ));
        }
        tracing::debug!(peer = %channel.peer.device_id, offered = listing.len(), "peering: listing received");
        Ok(Self {
            peer: channel.peer.clone(),
            channel: AsyncMutex::new(channel),
            listing,
            broken: AtomicBool::new(false),
        })
    }

    /// The peer the handshake verified.
    #[must_use]
    pub fn peer(&self) -> &VerifiedPeer {
        &self.peer
    }

    /// What the peer offers.
    #[must_use]
    pub fn listing(&self) -> &[Offered] {
        &self.listing
    }

    /// Whether an exchange failed part-way, so the client refuses every later request.
    #[must_use]
    pub fn is_broken(&self) -> bool {
        self.broken.load(Ordering::Acquire)
    }

    fn check_usable(&self) -> Result<(), PeeringError> {
        if self.is_broken() {
            return Err(PeeringError::Io("the channel broke earlier".into()));
        }
        Ok(())
    }

    /// Run one exchange; any failure but a clean refusal breaks the client.
    fn settle<T>(&self, result: Result<T, PeeringError>) -> Result<T, PeeringError> {
        if let Err(error) = &result
            && !matches!(error, PeeringError::PeerRefused { .. })
        {
            self.broken.store(true, Ordering::Release);
        }
        result
    }

    /// Pull bytes of `address` from `start`: at most `max_len`, and never more than
    /// [`MAX_RANGE`]. The bytes are the peer's until the whole artifact hashes to its address.
    /// The answer must arrive whole within the channel's I/O timeout plus its length at
    /// [`MIN_PULL_RATE`].
    ///
    /// # Errors
    ///
    /// A [`RangeFailure`] carrying whatever arrived before the failure; its error is
    /// [`PeeringError::Timeout`] when a frame, or the whole answer, came too slowly.
    pub async fn range(
        &self,
        address: &str,
        start: u64,
        max_len: Option<u64>,
    ) -> Result<Vec<u8>, RangeFailure> {
        let fail = |received, error| RangeFailure { received, error };
        if let Err(error) = self.check_usable() {
            return Err(fail(Vec::new(), error));
        }
        let cap = max_len.unwrap_or(MAX_RANGE).min(MAX_RANGE);
        let mut channel = self.channel.lock().await;
        let reply = async {
            channel
                .send(&Message::Range {
                    address: address.to_owned(),
                    start,
                    max_len: Some(cap),
                })
                .await?;
            match channel.recv().await? {
                Message::RangeReply { len } if len <= cap => Ok(len),
                Message::RangeReply { len } => Err(PeeringError::Codec(format!(
                    "the peer answered {len} bytes to a request for at most {cap}"
                ))),
                Message::Refused { kind, message } => {
                    Err(PeeringError::PeerRefused { kind, message })
                }
                other => Err(PeeringError::Codec(format!(
                    "expected a range reply, the peer sent {}",
                    other.name()
                ))),
            }
        }
        .await;
        let len = match self.settle(reply) {
            Ok(len) => len,
            Err(error) => return Err(fail(Vec::new(), error)),
        };
        let budget = channel.io_timeout.saturating_add(Duration::from_millis(
            len.saturating_mul(1000) / MIN_PULL_RATE,
        ));
        let deadline = tokio::time::Instant::now() + budget;
        tracing::trace!(peer = %self.peer.device_id, len, ?budget, "peering: pulling a range");
        let mut received = Vec::new();
        while (received.len() as u64) < len {
            let remaining = usize::try_from(len - received.len() as u64).unwrap_or(usize::MAX);
            let Ok(frame) = tokio::time::timeout_at(deadline, channel.recv_data(remaining)).await
            else {
                // Every frame arrived inside its own timeout, but too slowly overall: the peer
                // is holding this pull open without making progress worth waiting for.
                self.broken.store(true, Ordering::Release);
                tracing::warn!(peer = %self.peer.device_id, len, received = received.len(), "peering: a range fell below the minimum pull rate");
                let error = PeeringError::Timeout("pulling a range below the minimum rate");
                return Err(fail(received, error));
            };
            match self.settle(frame) {
                Ok(frame) if frame.is_empty() => {
                    // An empty frame makes no progress: a peer sending them would hold this
                    // loop for ever.
                    self.broken.store(true, Ordering::Release);
                    let error = PeeringError::Codec("the peer sent an empty data frame".into());
                    return Err(fail(received, error));
                }
                Ok(frame) => received.extend_from_slice(&frame),
                Err(error) => return Err(fail(received, error)),
            }
        }
        Ok(received)
    }

    /// Deliver an acknowledgement and wait for the peer to take it.
    ///
    /// # Errors
    ///
    /// [`PeeringError::PeerRefused`] when the peer refused it, or the channel's failure.
    pub async fn acknowledge(&self, ack: &HandOffAck) -> Result<(), PeeringError> {
        self.check_usable()?;
        let mut channel = self.channel.lock().await;
        let result = async {
            channel.send(&Message::Ack { ack: ack.clone() }).await?;
            match channel.recv().await? {
                Message::Acked => Ok(()),
                Message::Refused { kind, message } => {
                    Err(PeeringError::PeerRefused { kind, message })
                }
                other => Err(PeeringError::Codec(format!(
                    "expected an acknowledgement reply, the peer sent {}",
                    other.name()
                ))),
            }
        }
        .await;
        self.settle(result)
    }

    /// End the session: tell the peer, and close the stream.
    ///
    /// # Errors
    ///
    /// The channel's failure, when the peer could not be told.
    pub async fn finish(&self) -> Result<(), PeeringError> {
        self.check_usable()?;
        let mut channel = self.channel.lock().await;
        let sent = channel.send(&Message::Done).await;
        channel.close().await;
        self.settle(sent)
    }
}
