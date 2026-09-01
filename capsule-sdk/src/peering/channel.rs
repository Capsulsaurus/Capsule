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
//! [device directory]: https://docs/design/cryptography/keys/#device-directory

use std::sync::Arc;

use capsule_core::crypto::keys::{DeviceDirectory, HybridSignature, HybridVerifyingKey, Signer};
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

/// The peering **transport protocol** version, date-based (`YYYY-MM-DD`) and exchanged in the
/// hello at channel establishment. It shares the date grammar with the client-server protocol
/// but is its **own version space** — peering values compare only against peering values, and the
/// transport revs independently of the server protocol. A mismatch tears the channel down before
/// any payload byte (there is no degraded-mode fallback); the device proceeds to server sync.
pub const PEERING_PROTOCOL: &str = "2026-07-10";

/// Length of the exported channel-binding material the hybrid proof covers.
const EXPORTER_LEN: usize = 32;

/// The RFC 5705 exporter label. Distinct from any other exporter use so the derived material is
/// domain-separated to peering's hybrid check.
const EXPORTER_LABEL: &[u8] = b"EXPORTER-capsule-peering-hybrid-v1";

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

/// A peer whose hybrid identity was verified over the established channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPeer {
    /// The peer's directory `device_id`, proven to hold the matching device key.
    pub device_id: Uuid,
    /// The channel-binding exporter material the proof covered (returned for provenance/tests).
    pub exporter: [u8; EXPORTER_LEN],
}

/// The handshake hello each side sends over the encrypted channel: who it claims to be, the
/// peering protocol it speaks, and a hybrid signature over the channel-binding exporter material.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerHello {
    /// The sender's directory device id.
    pub device_id: Uuid,
    /// The sender's peering transport protocol version.
    pub peering_protocol: String,
    /// A hybrid signature over the TLS exporter material — the proof this device holds the key
    /// published for `device_id`.
    pub proof: HybridSignature,
}

/// Build this device's hello: sign the channel-binding `exporter` with the device `signer`.
fn build_hello(
    device_id: Uuid,
    signer: &dyn Signer,
    exporter: &[u8; EXPORTER_LEN],
) -> Result<PeerHello, PeeringError> {
    let proof = signer.sign(exporter)?;
    Ok(PeerHello {
        device_id,
        peering_protocol: PEERING_PROTOCOL.to_string(),
        proof,
    })
}

/// The application-layer hybrid check — the whole security of the channel. Run **before any
/// payload byte** against the locally-pinned trust. In order:
///
/// 1. **Protocol.** A peering-protocol mismatch aborts (no degraded mode) — `426`-class.
/// 2. **Directory chains to our IK.** The pinned directory must verify under our pinned User IK;
///    a directory signed by a foreign IK is rejected outright.
/// 3. **Enrolled + not revoked.** The claimed `device_id` must be present and un-revoked.
/// 4. **Hybrid proof.** The peer's published device key must verify the hybrid signature over the
///    exact exporter material *we* derived — binding the hybrid identity to this TLS session.
pub fn verify_hello(
    hello: &PeerHello,
    exporter: &[u8; EXPORTER_LEN],
    trust: &PinnedTrust,
) -> Result<Uuid, PeeringError> {
    if hello.peering_protocol != PEERING_PROTOCOL {
        return Err(PeeringError::ProtocolMismatch {
            theirs: hello.peering_protocol.clone(),
            ours: PEERING_PROTOCOL.to_string(),
        });
    }
    if !trust.directory.verify(&trust.user_ik) {
        return Err(PeeringError::ForeignIdentity);
    }
    let entry = trust
        .directory
        .device(&hello.device_id)
        .ok_or(PeeringError::UnknownDevice(hello.device_id))?;
    if entry.revoked_at.is_some() {
        return Err(PeeringError::RevokedDevice(hello.device_id));
    }
    if !entry.dsk_public.verify(exporter, &hello.proof) {
        return Err(PeeringError::HybridCheckFailed);
    }
    tracing::debug!(device = %hello.device_id, "peer hybrid identity verified over the channel");
    Ok(hello.device_id)
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

/// **Server side** of the peering handshake. Completes a real mutual TLS 1.3 handshake over
/// `tcp`, derives the channel-binding exporter, exchanges hellos (read-then-write, so the pair
/// never deadlocks), and runs the [`verify_hello`] hybrid check. Returns the verified peer or the
/// specific rejection — a wrong/revoked/foreign peer fails the whole handshake before any payload.
#[instrument(skip(tcp, signer, trust), fields(local = %device_id))]
pub async fn accept(
    tcp: TcpStream,
    device_id: Uuid,
    signer: &dyn Signer,
    trust: &PinnedTrust,
) -> Result<VerifiedPeer, PeeringError> {
    let acceptor = TlsAcceptor::from(Arc::new(server_config()?));
    let mut tls = acceptor.accept(tcp).await.map_err(io_err)?;

    let exporter = {
        let (_io, conn) = tls.get_ref();
        conn.export_keying_material([0u8; EXPORTER_LEN], EXPORTER_LABEL, None)
            .map_err(|e| PeeringError::Tls(e.to_string()))?
    };

    let peer_bytes = read_frame(&mut tls).await?;
    let our_hello = build_hello(device_id, signer, &exporter)?;
    write_frame(&mut tls, &encode(&our_hello)?).await?;

    let peer_hello = decode(&peer_bytes)?;
    let peer_device = verify_hello(&peer_hello, &exporter, trust)?;
    Ok(VerifiedPeer {
        device_id: peer_device,
        exporter,
    })
}

/// **Client side** of the peering handshake. The dialing (behind) device connects, drives the
/// TLS 1.3 handshake to completion, derives the same exporter, exchanges hellos (write-then-read),
/// and runs the hybrid check. See [`accept`] for the security contract.
#[instrument(skip(tcp, signer, trust), fields(local = %device_id))]
pub async fn connect(
    tcp: TcpStream,
    device_id: Uuid,
    signer: &dyn Signer,
    trust: &PinnedTrust,
) -> Result<VerifiedPeer, PeeringError> {
    let connector = TlsConnector::from(Arc::new(client_config()?));
    let name = ServerName::try_from(PEER_SNI)
        .map_err(|e| PeeringError::Tls(e.to_string()))?
        .to_owned();
    let mut tls = connector.connect(name, tcp).await.map_err(io_err)?;

    let exporter = {
        let (_io, conn) = tls.get_ref();
        conn.export_keying_material([0u8; EXPORTER_LEN], EXPORTER_LABEL, None)
            .map_err(|e| PeeringError::Tls(e.to_string()))?
    };

    let our_hello = build_hello(device_id, signer, &exporter)?;
    write_frame(&mut tls, &encode(&our_hello)?).await?;
    let peer_bytes = read_frame(&mut tls).await?;

    let peer_hello = decode(&peer_bytes)?;
    let peer_device = verify_hello(&peer_hello, &exporter, trust)?;
    Ok(VerifiedPeer {
        device_id: peer_device,
        exporter,
    })
}
