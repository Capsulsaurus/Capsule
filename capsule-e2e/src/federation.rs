//! Two federating servers on loopback: what E2E case 4 stands up so one pulls from the other.
//!
//! # Why a TLS front
//!
//! A federated pull dials `https://{peer}/` and nothing else (`origin_of` in
//! `capsule_server::federation::pull::remote`): a peer id is a bare host name, the client is
//! HTTPS-only, and the served binary terminates no TLS — a deployment puts it behind a proxy
//! that does. [`TlsFront`] is that proxy, reduced to what a case needs: it terminates TLS for one
//! host name under a private CA it generates, and copies the bytes to the server's plain
//! listener. The pulling server reaches it through the `test-egress` seam, which routes the host
//! to the front's address and trusts the CA; the URL, the origin pin, the redirect policy and the
//! certificate check all stay as production has them.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, rustls};

/// A second PKCS#8 v1 Ed25519 key, base64, for the server that is not [`crate::JWT_ED25519_DER`]'s.
///
/// Two federating servers must hold different operational keys, or a key pinned for the wrong
/// peer would verify anyway and the case could not tell. Written by `openssl genpkey` for this
/// harness; it signs nothing anywhere else.
pub const SECOND_ED25519_DER: &str =
    "MC4CAQAwBQYDK2VwBCIEICjctbngasrzdZCph6Ht8OEJthXxC/aPt7kOM/tfgVGk";

/// A TLS terminator for one host name, in front of one plain listener.
///
/// Dropping it stops accepting; connections already open run to completion on their own tasks.
pub struct TlsFront {
    address: SocketAddr,
    ca_der: Vec<u8>,
    accept: tokio::task::JoinHandle<()>,
}

impl TlsFront {
    /// Generate a private CA and a leaf it signs for `host`, bind on loopback, and forward every
    /// connection that completes a handshake to `upstream`.
    ///
    /// # Panics
    ///
    /// When the certificates cannot be generated or the port cannot be bound.
    pub async fn start(host: &str, upstream: SocketAddr) -> Self {
        let ca_key = rcgen::KeyPair::generate().expect("a CA key");
        let mut ca_params =
            rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA parameters");
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).expect("a CA certificate");
        let leaf_key = rcgen::KeyPair::generate().expect("a leaf key");
        let leaf = rcgen::CertificateParams::new(vec![host.to_owned()])
            .expect("leaf parameters")
            .signed_by(&leaf_key, &ca, &ca_key)
            .expect("a leaf signed by the CA");

        // `ring`, named rather than taken from the process default: the stack every other
        // rustls user in the workspace links.
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("the ring provider serves the default versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone(), ca.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
        )
        .expect("a server config");
        let acceptor = TlsAcceptor::from(Arc::new(config));

        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("an ephemeral port binds");
        let address = listener
            .local_addr()
            .expect("a bound listener has an address");
        let accept = tokio::spawn(async move {
            loop {
                let (inbound, from) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        tracing::warn!(%error, "the TLS front stopped accepting");
                        return;
                    }
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let mut tls = match acceptor.accept(inbound).await {
                        Ok(tls) => tls,
                        Err(error) => {
                            tracing::warn!(%error, %from, "the TLS front refused a handshake");
                            return;
                        }
                    };
                    let Ok(mut plain) = TcpStream::connect(upstream).await else {
                        tracing::warn!(%upstream, "the TLS front could not reach its upstream");
                        return;
                    };
                    let copied = tokio::io::copy_bidirectional(&mut tls, &mut plain).await;
                    tracing::debug!(%from, ?copied, "the TLS front closed a connection");
                });
            }
        });
        tracing::info!(host, %address, %upstream, "TLS front listening");
        Self {
            address,
            ca_der: ca.der().to_vec(),
            accept,
        }
    }

    /// Where the front listens.
    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The private CA's certificate, DER: the root a client must trust to reach the front.
    #[must_use]
    pub fn ca_der(&self) -> &[u8] {
        &self.ca_der
    }
}

impl Drop for TlsFront {
    fn drop(&mut self) {
        self.accept.abort();
    }
}
