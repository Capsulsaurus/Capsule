//! An in-process OpenID Connect provider, speaking the real wire on loopback.
//!
//! Serves the discovery document, a JWK Set and a form-decoding token endpoint that mints
//! EdDSA-signed ID tokens, so `HttpIdentityProvider` is exercised over exactly the bytes a real
//! provider sends: discovery JSON, `application/jwk-set+json`, an
//! `application/x-www-form-urlencoded` `POST`, a compact JWS. Every negative case the relying
//! party has to refuse is a [`Tamper`] on the grant the test issues.
//!
//! `mise run test-rust` runs offline and container-free, so this stands in for the testcontainer
//! provider `design/authentication.md` names; the dex service in `capsule-server/compose.yaml` is
//! the manual run against a real one.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::jwk::{
    AlgorithmParameters, CommonParameters, EllipticCurve, Jwk, JwkSet, KeyAlgorithm,
    OctetKeyPairParameters, OctetKeyPairType,
};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use ring::signature::KeyPair as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The `client_id` the provider knows.
pub(crate) const CLIENT_ID: &str = "capsule";

/// How the provider should misbehave when it mints a token for one grant.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Tamper {
    /// A conforming token.
    #[default]
    None,
    /// `iss` set to this.
    Issuer(String),
    /// `aud` set to this.
    Audience(String),
    /// `exp` an hour in the past.
    Expired,
    /// `nonce` set to this instead of the grant's.
    Nonce(String),
    /// Signed by a key the JWK Set has never published, under a `kid` it never listed.
    UnpublishedKey,
    /// `alg: none`, unsigned.
    AlgNone,
}

/// A code the test issues for the relying party to redeem.
#[derive(Debug, Clone)]
pub(crate) struct Grant {
    pub(crate) code_challenge: String,
    pub(crate) nonce: String,
    pub(crate) redirect_uri: String,
    pub(crate) subject: String,
    pub(crate) email: Option<String>,
    pub(crate) tamper: Tamper,
}

/// One signing key and its published form.
struct Signer {
    kid: String,
    encoding: EncodingKey,
    public: Vec<u8>,
}

impl Signer {
    fn generate(kid: String) -> Self {
        let der = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
            .expect("the platform generates keys");
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(der.as_ref())
            .expect("a key just generated parses");
        Self {
            kid,
            encoding: EncodingKey::from_ed_der(der.as_ref()),
            public: pair.public_key().as_ref().to_vec(),
        }
    }

    fn jwk(&self) -> Jwk {
        Jwk {
            common: CommonParameters {
                key_id: Some(self.kid.clone()),
                key_algorithm: Some(KeyAlgorithm::EdDSA),
                ..CommonParameters::default()
            },
            algorithm: AlgorithmParameters::OctetKeyPair(OctetKeyPairParameters {
                key_type: OctetKeyPairType::OctetKeyPair,
                curve: EllipticCurve::Ed25519,
                x: URL_SAFE_NO_PAD.encode(&self.public),
            }),
        }
    }
}

struct State {
    issuer: String,
    /// Every key the JWK Set publishes; the last one signs.
    published: Mutex<Vec<Signer>>,
    grants: Mutex<BTreeMap<String, Grant>>,
    next_code: AtomicUsize,
    discovery_hits: AtomicUsize,
    jwks_hits: AtomicUsize,
    token_hits: AtomicUsize,
    discovery_down: AtomicBool,
}

/// A running mock provider. Dropping it aborts the accept loop and every connection.
pub(crate) struct MockIdp {
    state: Arc<State>,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for MockIdp {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl MockIdp {
    /// Bind on loopback and start serving, with one published key.
    pub(crate) async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback binds");
        let addr = listener.local_addr().expect("a bound address");
        let state = Arc::new(State {
            issuer: format!("http://{addr}/idp"),
            published: Mutex::new(vec![Signer::generate("k1".to_owned())]),
            grants: Mutex::new(BTreeMap::new()),
            next_code: AtomicUsize::new(1),
            discovery_hits: AtomicUsize::new(0),
            jwks_hits: AtomicUsize::new(0),
            token_hits: AtomicUsize::new(0),
            discovery_down: AtomicBool::new(false),
        });
        let serving = Arc::clone(&state);
        let handle = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                while connections.try_join_next().is_some() {}
                let state = Arc::clone(&serving);
                connections.spawn(async move {
                    serve(stream, state).await;
                });
            }
        });
        Self { state, handle }
    }

    /// The issuer the relying party is configured with. Loopback `http`, the carve-out.
    pub(crate) fn issuer(&self) -> String {
        self.state.issuer.clone()
    }

    /// Issue a code the token endpoint will redeem for a token minted from `grant`.
    pub(crate) fn grant(&self, grant: Grant) -> String {
        let code = format!(
            "code-{}",
            self.state.next_code.fetch_add(1, Ordering::SeqCst)
        );
        lock(&self.state.grants).insert(code.clone(), grant);
        code
    }

    /// Publish a new key and sign with it from now on.
    pub(crate) fn rotate(&self, kid: &str) {
        lock(&self.state.published).push(Signer::generate(kid.to_owned()));
    }

    /// Whether the discovery endpoint answers at all.
    pub(crate) fn set_discovery_down(&self, down: bool) {
        self.state.discovery_down.store(down, Ordering::SeqCst);
    }

    pub(crate) fn discovery_hits(&self) -> usize {
        self.state.discovery_hits.load(Ordering::SeqCst)
    }

    pub(crate) fn jwks_hits(&self) -> usize {
        self.state.jwks_hits.load(Ordering::SeqCst)
    }

    pub(crate) fn token_hits(&self) -> usize {
        self.state.token_hits.load(Ordering::SeqCst)
    }
}

/// The relying party's `code_challenge`, S256 (RFC 7636 §4.2).
fn challenge_of(verifier: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest.as_ref())
}

/// The token endpoint: verify the grant and mint the ID token.
fn token_response(
    state: &State,
    form: &BTreeMap<String, String>,
    basic: Option<&str>,
) -> (u16, String) {
    let refuse = |error: &str, description: &str| {
        (
            400,
            serde_json::json!({ "error": error, "error_description": description }).to_string(),
        )
    };
    if form.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return refuse("unsupported_grant_type", "only authorization_code");
    }
    // A public client names itself in the body; a confidential one authenticates instead.
    let client_id = form
        .get("client_id")
        .cloned()
        .or_else(|| basic.map(str::to_owned));
    if client_id.as_deref() != Some(CLIENT_ID) {
        return refuse("invalid_client", "unknown client");
    }
    let Some(code) = form.get("code") else {
        return refuse("invalid_request", "no code");
    };
    // Single-use at the provider, like a real one.
    let Some(grant) = lock(&state.grants).remove(code) else {
        return refuse("invalid_grant", "unknown or spent code");
    };
    if form.get("redirect_uri") != Some(&grant.redirect_uri) {
        return refuse("invalid_grant", "redirect_uri mismatch");
    }
    let verifier_ok = form
        .get("code_verifier")
        .is_some_and(|verifier| challenge_of(verifier) == grant.code_challenge);
    if !verifier_ok {
        return refuse("invalid_grant", "PKCE verification failed");
    }

    let now = jiff::Timestamp::now().as_second();
    let mut claims = serde_json::json!({
        "iss": state.issuer,
        "sub": grant.subject,
        "aud": CLIENT_ID,
        "exp": now + 300,
        "iat": now,
        "nonce": grant.nonce,
    });
    if let Some(email) = &grant.email {
        claims["email"] = serde_json::json!(email);
        claims["email_verified"] = serde_json::json!(true);
    }
    match &grant.tamper {
        Tamper::None | Tamper::UnpublishedKey | Tamper::AlgNone => {}
        Tamper::Issuer(issuer) => claims["iss"] = serde_json::json!(issuer),
        Tamper::Audience(audience) => claims["aud"] = serde_json::json!(audience),
        Tamper::Expired => claims["exp"] = serde_json::json!(now - 3600),
        Tamper::Nonce(nonce) => claims["nonce"] = serde_json::json!(nonce),
    }

    let id_token = match &grant.tamper {
        Tamper::AlgNone => {
            let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
            let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
            format!("{header}.{payload}.")
        }
        Tamper::UnpublishedKey => {
            let rogue = Signer::generate("k-never-published".to_owned());
            let mut header = Header::new(Algorithm::EdDSA);
            header.kid = Some(rogue.kid.clone());
            jsonwebtoken::encode(&header, &claims, &rogue.encoding).expect("the key signs")
        }
        _ => {
            let published = lock(&state.published);
            let signer = published.last().expect("a signing key");
            let mut header = Header::new(Algorithm::EdDSA);
            header.kid = Some(signer.kid.clone());
            jsonwebtoken::encode(&header, &claims, &signer.encoding).expect("the key signs")
        }
    };
    (
        200,
        serde_json::json!({
            "id_token": id_token,
            "access_token": "opaque-access-token",
            "token_type": "Bearer",
        })
        .to_string(),
    )
}

async fn serve(mut stream: tokio::net::TcpStream, state: Arc<State>) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break pos;
        }
        match stream.read(&mut tmp).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut content_length = 0usize;
    let mut basic = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().unwrap_or(0);
            }
            if name.eq_ignore_ascii_case("authorization")
                && let Some(encoded) = value.strip_prefix("Basic ")
                && let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded)
                && let Ok(text) = String::from_utf8(decoded)
                && let Some((user, _)) = text.split_once(':')
            {
                basic = Some(user.to_owned());
            }
        }
    }
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut tmp).await {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&tmp[..n]),
        }
    }
    body.truncate(content_length);

    let (status, content_type, payload) = match (method.as_str(), path.as_str()) {
        ("GET", "/idp/.well-known/openid-configuration") => {
            state.discovery_hits.fetch_add(1, Ordering::SeqCst);
            if state.discovery_down.load(Ordering::SeqCst) {
                (503, "text/plain", "down".to_owned())
            } else {
                let base = state.issuer.trim_end_matches("/idp");
                (
                    200,
                    "application/json",
                    serde_json::json!({
                        "issuer": state.issuer,
                        "authorization_endpoint": format!("{base}/idp/authorize"),
                        "token_endpoint": format!("{base}/idp/token"),
                        "jwks_uri": format!("{base}/idp/keys"),
                        "response_types_supported": ["code"],
                        "subject_types_supported": ["public"],
                        "id_token_signing_alg_values_supported": ["EdDSA"],
                    })
                    .to_string(),
                )
            }
        }
        ("GET", "/idp/keys") => {
            state.jwks_hits.fetch_add(1, Ordering::SeqCst);
            let set = JwkSet {
                keys: lock(&state.published).iter().map(Signer::jwk).collect(),
            };
            (
                200,
                "application/jwk-set+json",
                serde_json::to_string(&set).expect("a set serializes"),
            )
        }
        ("POST", "/idp/token") => {
            state.token_hits.fetch_add(1, Ordering::SeqCst);
            // `Url` decodes form bodies for free; the query-pair parser is the form parser.
            let form: BTreeMap<String, String> =
                reqwest::Url::parse(&format!("http://x/?{}", String::from_utf8_lossy(&body)))
                    .map(|url| {
                        url.query_pairs()
                            .map(|(k, v)| (k.into_owned(), v.into_owned()))
                            .collect()
                    })
                    .unwrap_or_default();
            let (status, payload) = token_response(&state, &form, basic.as_deref());
            (status, "application/json", payload)
        }
        _ => (404, "text/plain", "no such route".to_owned()),
    };

    let response = format!(
        "HTTP/1.1 {status} STATUS\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
}

/// The address the mock is bound to, for a case that wants to name it.
#[allow(dead_code, reason = "kept for a case that reads the socket address")]
pub(crate) fn address_of(issuer: &str) -> SocketAddr {
    reqwest::Url::parse(issuer)
        .ok()
        .and_then(|url| url.socket_addrs(|| None).ok())
        .and_then(|addrs| addrs.first().copied())
        .expect("the issuer names a loopback socket")
}
