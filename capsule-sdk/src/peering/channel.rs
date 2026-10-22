//! Establishing the channel: mutually-authenticated TLS 1.3 with an application-layer hybrid
//! identity check.
//!
//! A peer connection is HTTP over a **mutually-authenticated TLS 1.3** channel with **no CA** —
//! the device keys are the trust anchor. TLS 1.3 has no ML-DSA certificate path, so the split
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
//!
//! [device directory]: https://docs/design/cryptography/keys/#device-directory

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

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
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tracing::instrument;
use uuid::Uuid;

use super::PeeringError;
use super::session::{DirectoryVersions, HandshakeLimiter};
use crate::net::negotiation::is_protocol_date;

/// The newest peering **transport protocol** version this build speaks — the top of its
/// [`VersionWindow`]. Date-based (`YYYY-MM-DD`), it shares the date grammar with the
/// client-server protocol but is its **own version space**: peering values compare only against
/// peering values, and the transport revs independently of the server protocol.
pub const PEERING_PROTOCOL: &str = "2026-10-22";

/// The oldest peering transport protocol version this build still speaks — the bottom of its
/// [`VersionWindow`]. `2026-10-22` added the directory version, the window itself and the
/// exporter-derived artifact secret to the handshake; a peer older than that cannot satisfy the
/// anti-rollback rule, so it is outside the window.
pub const PEERING_PROTOCOL_MIN: &str = "2026-10-22";

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

/// Frame ceiling for the tiny handshake hello (a `device_id`, a protocol string, and a hybrid
/// signature). Bounds a malicious length prefix.
const MAX_FRAME: usize = 64 * 1024;

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

/// Build this device's hello: sign the channel-binding `exporter` with the device `signer`.
fn build_hello(
    local: &LocalPeer<'_>,
    exporter: &[u8; EXPORTER_LEN],
) -> Result<PeerHello, PeeringError> {
    let proof = local.signer.sign(exporter)?;
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
/// 4. **Enrolled + not revoked.** The claimed `device_id` must be present and un-revoked.
/// 5. **Hybrid proof.** The peer's published device key must verify the hybrid signature over the
///    exact exporter material *we* derived — binding the hybrid identity to this TLS session.
///
/// Returns the verified device id and the negotiated protocol version.
pub fn verify_hello(
    hello: &PeerHello,
    exporter: &[u8; EXPORTER_LEN],
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
    let entry = admit_device(trust, hello.device_id)?;
    if !entry.dsk_public.verify(exporter, &hello.proof) {
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

/// Verify the peer's hello and assemble the [`VerifiedPeer`].
fn finish(
    peer_bytes: &[u8],
    exporter: [u8; EXPORTER_LEN],
    artifact_secret: ArtifactSecret,
    local: &LocalPeer<'_>,
) -> Result<VerifiedPeer, PeeringError> {
    let peer_hello = decode(peer_bytes)?;
    let (device_id, protocol) = verify_hello(&peer_hello, &exporter, local.trust, local.versions)?;
    Ok(VerifiedPeer {
        device_id,
        protocol,
        exporter,
        artifact_secret,
    })
}

/// **Server side** of the peering handshake. Admits the source address through `limiter`
/// (refusing with [`PeeringError::RateLimited`] before any TLS byte), completes a real mutual
/// TLS 1.3 handshake over `tcp`, derives the channel-binding exporter, exchanges hellos
/// (read-then-write, so the pair never deadlocks), and runs the [`verify_hello`] check. Returns
/// the verified peer or the specific rejection — a wrong/revoked/foreign/stale peer fails the
/// whole handshake before any payload.
#[instrument(skip(tcp, local, limiter), fields(local = %local.device_id))]
pub async fn accept(
    tcp: TcpStream,
    local: LocalPeer<'_>,
    limiter: &HandshakeLimiter,
) -> Result<VerifiedPeer, PeeringError> {
    let source = tcp.peer_addr().map_err(io_err)?.ip();
    if !limiter.admit(source, Instant::now()) {
        tracing::warn!(%source, "peering handshake rate limit reached; refusing");
        return Err(PeeringError::RateLimited(source));
    }
    let acceptor = TlsAcceptor::from(Arc::new(server_config()?));
    let mut tls = acceptor.accept(tcp).await.map_err(io_err)?;

    let (exporter, secret) = {
        let (_io, conn) = tls.get_ref();
        derive_exporters(|label| conn.export_keying_material([0u8; EXPORTER_LEN], label, None))?
    };

    let peer_bytes = read_frame(&mut tls).await?;
    let our_hello = build_hello(&local, &exporter)?;
    write_frame(&mut tls, &encode(&our_hello)?).await?;

    finish(&peer_bytes, exporter, secret, &local)
}

/// **Client side** of the peering handshake. The dialing (behind) device connects, drives the
/// TLS 1.3 handshake to completion, derives the same exporters, exchanges hellos
/// (write-then-read), and runs the hybrid check. See [`accept`] for the security contract.
#[instrument(skip(tcp, local), fields(local = %local.device_id))]
pub async fn connect(tcp: TcpStream, local: LocalPeer<'_>) -> Result<VerifiedPeer, PeeringError> {
    let connector = TlsConnector::from(Arc::new(client_config()?));
    let name = ServerName::try_from(PEER_SNI)
        .map_err(|e| PeeringError::Tls(e.to_string()))?
        .to_owned();
    let mut tls = connector.connect(name, tcp).await.map_err(io_err)?;

    let (exporter, secret) = {
        let (_io, conn) = tls.get_ref();
        derive_exporters(|label| conn.export_keying_material([0u8; EXPORTER_LEN], label, None))?
    };

    let our_hello = build_hello(&local, &exporter)?;
    write_frame(&mut tls, &encode(&our_hello)?).await?;
    let peer_bytes = read_frame(&mut tls).await?;

    finish(&peer_bytes, exporter, secret, &local)
}
