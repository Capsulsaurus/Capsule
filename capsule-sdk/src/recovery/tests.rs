use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use capsule_core::backup::recover_master_key;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::*;
use crate::auth::{AuthClient, PersistedSession};

/// Fast Argon2id params so the escrow crypto does not dominate the smoke tests
/// (same posture as the server's escrow integration test).
fn fast_params() -> Argon2Params {
    Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    }
}

// ── Binary-capable mock escrow server ─────────────────────────────────────
//
// The escrow surface is `application/octet-stream` on the way out, so unlike the auth
// mock (JSON strings) this one stores and serves raw bytes. A single shared slot models
// the server's single-active-escrow row.
//
// Two things it must now do that it did not have to when this module built its own URL.
// It **routes on the path**, answering `501` to anything that is not `/v1/auth/escrow`,
// because a mock that replies to whatever it is handed is exactly why a wrong route
// survived here. And its refusals carry a real RFC 9457 problem body: the generated
// client decodes a documented non-success status into `CodedProblem`, so a bare status
// with an empty body would arrive as a decode failure rather than the typed variant.

/// The one path the escrow operations are served on, per the committed document.
const ESCROW_ROUTE: &str = "/v1/auth/escrow";

#[derive(Clone, Default)]
struct EscrowStore {
    blob: Arc<Mutex<Option<Vec<u8>>>>,
    /// `PUT`s answered so far; the `n`-th is stamped `n` seconds past [`STORED_AT_BASE`],
    /// so every store has a strictly later `stored_at` than the one before it.
    puts: Arc<AtomicU64>,
    /// `GET`s answered so far — how a test sees whether `verify` refreshed.
    fetches: Arc<AtomicU64>,
}

impl EscrowStore {
    /// Serve the slot for a `GET`, counting it.
    fn fetch(&self) -> Option<Vec<u8>> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        self.blob.lock().unwrap().clone()
    }

    /// How many `GET`s the mock has answered.
    fn fetch_count(&self) -> u64 {
        self.fetches.load(Ordering::SeqCst)
    }
}

/// The instant the mock stamps its first `PUT` with.
const STORED_AT_BASE: &str = "2026-01-01T00:00:00Z";

/// The `stored_at` the mock answers its `n`-th `PUT` with (zero-based).
fn mock_stored_at(n: u64) -> Timestamp {
    STORED_AT_BASE
        .parse::<Timestamp>()
        .unwrap()
        .checked_add(jiff::SignedDuration::from_secs(n as i64))
        .unwrap()
}

struct MockRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

struct MockResponse {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl MockResponse {
    /// An `application/octet-stream` payload — the escrow itself.
    fn bytes(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type: "application/octet-stream",
            body,
        }
    }

    /// A JSON payload — `StoreEscrowResponse`, which the generated client decodes.
    fn json(status: u16, body: serde_json::Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: body.to_string().into_bytes(),
        }
    }

    /// An RFC 9457 problem, shaped as `CodedProblem` so the generated client can parse it
    /// into the operation's typed error.
    fn problem(status: u16, code: &str, detail: &str) -> Self {
        Self {
            status,
            content_type: "application/problem+json",
            body: serde_json::json!({
                "type": "about:blank",
                "title": "Refused",
                "status": status,
                "detail": detail,
                "code": code,
            })
            .to_string()
            .into_bytes(),
        }
    }
}

type BoxFut = Pin<Box<dyn Future<Output = MockResponse> + Send>>;
type Handler = Arc<dyn Fn(MockRequest) -> BoxFut + Send + Sync>;

async fn start_mock(handler: Handler) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    crate::net::negotiation::forget_addr(listener.local_addr().unwrap());
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let handler = handler.clone();
            tokio::spawn(async move {
                let _ = serve_conn(&mut socket, handler).await;
            });
        }
    });
    format!("http://{addr}")
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

async fn serve_conn(socket: &mut TcpStream, handler: Handler) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    let header_end = loop {
        if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        let n = socket.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default().to_string();
    let path = request_parts.next().unwrap_or_default().to_string();

    let mut content_length = 0usize;
    let mut authorized = false;
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_lowercase();
            let value = value.trim();
            if key == "content-length" {
                content_length = value.parse().unwrap_or(0);
            }
            if key == "authorization" && value.starts_with("Bearer ") {
                authorized = true;
            }
        }
    }

    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = socket.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }

    // Every escrow call is owner-scoped: reject anything without a bearer token.
    let response = if authorized {
        handler(MockRequest { method, path, body }).await
    } else {
        MockResponse::problem(
            401,
            error_codes::REQUEST_UNAUTHENTICATED,
            "no bearer credential",
        )
    };

    let payload = format!(
        "HTTP/1.1 {} STATUS\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len()
    );
    let mut out = payload.into_bytes();
    out.extend_from_slice(&response.body);
    socket.write_all(&out).await?;
    socket.flush().await?;
    Ok(())
}

/// A handler backed by the shared single-active-escrow slot: `PUT` overwrites it,
/// `GET` serves it verbatim (or `404`).
///
/// Anything off `/v1/auth/escrow` is `501`, which arrives as
/// [`RecoveryError::Unexpected`] rather than as a plausible-looking `NotEnrolled` — so a
/// route regression fails loudly here instead of reading as "no escrow stored".
fn escrow_handler(store: EscrowStore) -> Handler {
    Arc::new(move |req| {
        let store = store.clone();
        Box::pin(async move {
            if req.path != ESCROW_ROUTE {
                return MockResponse::bytes(501, Vec::new());
            }
            match req.method.as_str() {
                "PUT" => {
                    let replaced = store.blob.lock().unwrap().replace(req.body).is_some();
                    let n = store.puts.fetch_add(1, Ordering::SeqCst);
                    MockResponse::json(
                        200,
                        serde_json::json!({
                            "stored_at": mock_stored_at(n).to_string(),
                            "replaced": replaced,
                        }),
                    )
                }
                "GET" => match store.fetch() {
                    Some(bytes) => MockResponse::bytes(200, bytes),
                    None => MockResponse::problem(
                        404,
                        error_codes::ESCROW_NOT_STORED,
                        "no escrow has been stored for this account",
                    ),
                },
                _ => MockResponse::problem(
                    405,
                    error_codes::REQUEST_METHOD_NOT_ALLOWED,
                    "the escrow surface serves GET and PUT",
                ),
            }
        })
    })
}

/// Build an authenticated [`Session`] over the mock base with a far-future token, so
/// no refresh ever fires and the mock needs no `/refresh` endpoint.
fn session_for(base: &str) -> Session {
    let client = AuthClient::new(base).unwrap();
    client
        .resume(PersistedSession {
            access_token: "test-access".to_string().into(),
            refresh_token: "test-refresh".to_string().into(),
            access_expires_at_unix: jiff::Timestamp::now().as_second() + 3_600,
        })
        .unwrap()
}

/// A session over the mock whose access token expired an hour ago, so any call
/// pre-flight-refreshes — and the escrow mock serves no `/refresh`, so that refresh fails.
/// The result is a [`Session`] that cannot produce a bearer at all.
fn dead_session_for(base: &str) -> Session {
    AuthClient::new(base)
        .unwrap()
        .resume(PersistedSession {
            access_token: "test-access".to_string().into(),
            refresh_token: "test-refresh".to_string().into(),
            access_expires_at_unix: jiff::Timestamp::now().as_second() - 3_600,
        })
        .unwrap()
}

fn wrap(master: &[u8; 32], secret: &[u8]) -> WrappedSecret {
    pwkdf::wrap_with(master, secret, fast_params()).unwrap()
}

/// Store → fetch round-trips the escrow verbatim, and the fetched blob unwraps to
/// the original master key.
#[tokio::test]
async fn escrow_store_fetch_round_trip() {
    let store = EscrowStore::default();
    let base = start_mock(escrow_handler(store)).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();

    let master = [0x11u8; 32];
    let blob = wrap(&master, b"correct horse battery staple");
    client.store_escrow(&blob).await.unwrap();

    let cache = client.fetch_escrow().await.unwrap();
    assert_eq!(cache.blob(), &blob, "fetched blob equals stored blob");
    assert_eq!(
        cache.stored_at(),
        None,
        "GET serves only the bytes, so a fetched copy cannot claim a stored_at"
    );
    assert_eq!(
        recover_master_key(cache.blob(), b"correct horse battery staple").unwrap(),
        master
    );
}

/// `store_escrow` returns what the server answered: a first escrow is not a replacement,
/// the next one is, and each carries the server's `stored_at` parsed as an instant.
#[tokio::test]
async fn store_escrow_returns_stored_at_and_replaced() {
    let base = start_mock(escrow_handler(EscrowStore::default())).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();
    let master = [0x12u8; 32];

    let first = client.store_escrow(&wrap(&master, b"one")).await.unwrap();
    assert_eq!(
        first,
        EscrowStored {
            stored_at: mock_stored_at(0),
            replaced: false,
        }
    );
    assert_eq!(first.old_artifact_guidance(), None);

    let second = client.store_escrow(&wrap(&master, b"two")).await.unwrap();
    assert_eq!(
        second,
        EscrowStored {
            stored_at: mock_stored_at(1),
            replaced: true,
        }
    );
    assert_eq!(
        second.old_artifact_guidance(),
        Some(OldArtifactGuidance::ReexportOrDestroy)
    );
}

/// A `200` whose `stored_at` is not an instant is an unreadable answer, not a silent
/// success with a made-up time and not a refusal of the blob.
#[tokio::test]
async fn an_unparseable_stored_at_is_an_invalid_response() {
    let handler: Handler = Arc::new(|_req| {
        Box::pin(async move {
            MockResponse::json(
                200,
                serde_json::json!({ "stored_at": "yesterday", "replaced": false }),
            )
        })
    });
    let base = start_mock(handler).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();
    let error = client
        .store_escrow(&wrap(&[0x13u8; 32], b"whatever"))
        .await
        .expect_err("`yesterday` is not RFC 3339");
    assert!(
        matches!(error, RecoveryError::InvalidResponse(_)),
        "got {error:?}"
    );
    assert_eq!(error.error_code(), None);
}

/// `is_older_than` is a strict compare, and a copy with no known `stored_at` cannot prove
/// it is current.
#[test]
fn cache_staleness_against_a_latest_stored_at() {
    let blob = wrap(&[0x14u8; 32], b"s");
    let at = mock_stored_at(5);
    let known = EscrowCache::with_stored_at(blob.clone(), at);
    assert!(known.is_older_than(mock_stored_at(6)));
    assert!(!known.is_older_than(at), "the same instant is current");
    assert!(!known.is_older_than(mock_stored_at(4)));
    assert!(EscrowCache::new(blob).is_older_than(at));
}

/// Fetch with nothing stored is a typed `NotEnrolled`, not a panic or opaque error.
#[tokio::test]
async fn fetch_without_escrow_is_not_enrolled() {
    let base = start_mock(escrow_handler(EscrowStore::default())).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();
    assert!(matches!(
        client.fetch_escrow().await,
        Err(RecoveryError::NotEnrolled)
    ));
}

/// The correct secret verifies against the cached blob with a single fetch (the
/// initial enroll) and no refresh needed.
#[tokio::test]
async fn verify_correct_secret_against_cache() {
    let store = EscrowStore::default();
    let base = start_mock(escrow_handler(store.clone())).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();

    let master = [0x22u8; 32];
    let blob = wrap(&master, b"the-right-secret");
    client.store_escrow(&blob).await.unwrap();
    let mut cache = client.fetch_escrow().await.unwrap();

    let fetches = store.fetch_count();
    assert_eq!(
        client
            .verify(&mut cache, b"the-right-secret", &master, None)
            .await
            .unwrap(),
        VerifyOutcome::Verified
    );
    assert_eq!(
        store.fetch_count(),
        fetches,
        "a verify that passes fetches nothing"
    );
}

/// A cache stamped by this client's own store, verified against that same `stored_at`,
/// is current: the compare runs offline and nothing is fetched.
#[tokio::test]
async fn verify_against_a_current_stored_at_fetches_nothing() {
    let store = EscrowStore::default();
    let base = start_mock(escrow_handler(store.clone())).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();

    let master = [0x23u8; 32];
    let blob = wrap(&master, b"the-right-secret");
    let stored = client.store_escrow(&blob).await.unwrap();
    let mut cache = EscrowCache::with_stored_at(blob, stored.stored_at);

    assert_eq!(
        client
            .verify(
                &mut cache,
                b"the-right-secret",
                &master,
                Some(stored.stored_at)
            )
            .await
            .unwrap(),
        VerifyOutcome::Verified
    );
    assert_eq!(store.fetch_count(), 0);
}

/// **The stale-cache rule, read from `stored_at`.** Another device rotated the escrow and
/// this one has seen the newer `stored_at`: verify refreshes *before* comparing, so the
/// new secret passes on the first compare, the cache takes the newer instant, and a
/// wrong secret fails with one fetch — not the compare-fail-fetch-compare round the
/// failure-driven rule costs.
#[tokio::test]
async fn verify_refreshes_first_when_a_newer_stored_at_is_known() {
    let store = EscrowStore::default();
    let base = start_mock(escrow_handler(store.clone())).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();

    let master = [0x34u8; 32];
    let old_blob = wrap(&master, b"old-secret");
    let old = client.store_escrow(&old_blob).await.unwrap();
    let mut cache = EscrowCache::with_stored_at(old_blob, old.stored_at);

    // Another device rotates; this device learns the rotation's stored_at.
    let new_blob = wrap(&master, b"new-secret");
    let rotated = client.store_escrow(&new_blob).await.unwrap();
    assert!(rotated.stored_at > old.stored_at);

    assert_eq!(
        client
            .verify(&mut cache, b"new-secret", &master, Some(rotated.stored_at))
            .await
            .unwrap(),
        VerifyOutcome::Verified
    );
    assert_eq!(cache.blob(), &new_blob);
    assert_eq!(cache.stored_at(), Some(rotated.stored_at));
    assert_eq!(store.fetch_count(), 1, "one proactive refresh");

    // Now current: no proactive refresh runs, so a wrong secret fails the cached
    // compare and the failure-driven refresh runs once before the genuine failure.
    assert_eq!(
        client
            .verify(&mut cache, b"wrong", &master, Some(rotated.stored_at))
            .await
            .unwrap(),
        VerifyOutcome::NotVerified
    );
    assert_eq!(
        store.fetch_count(),
        2,
        "the failure-driven refresh still runs once"
    );
}

/// A known-stale cache verified with a wrong secret fails after exactly one fetch: the
/// proactive refresh already made the cache current, so no second round trip follows.
#[tokio::test]
async fn a_known_stale_cache_fails_a_wrong_secret_with_one_fetch() {
    let store = EscrowStore::default();
    let base = start_mock(escrow_handler(store.clone())).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();

    let master = [0x35u8; 32];
    let old_blob = wrap(&master, b"old-secret");
    let old = client.store_escrow(&old_blob).await.unwrap();
    let mut cache = EscrowCache::with_stored_at(old_blob, old.stored_at);
    let rotated = client
        .store_escrow(&wrap(&master, b"new-secret"))
        .await
        .unwrap();

    assert_eq!(
        client
            .verify(&mut cache, b"old-secret", &master, Some(rotated.stored_at))
            .await
            .unwrap(),
        VerifyOutcome::NotVerified,
        "the rotated-away secret no longer verifies"
    );
    assert_eq!(store.fetch_count(), 1);
}

/// The failure-driven refresh's `stored_at` handling, from a stamped cache with no
/// `latest_stored_at`: an unchanged blob keeps the cache's `stored_at`, and a changed
/// blob drops it, because `GET` does not say when the new blob was stored.
#[tokio::test]
async fn fallback_refresh_keeps_stored_at_only_when_the_blob_is_unchanged() {
    let store = EscrowStore::default();
    let base = start_mock(escrow_handler(store.clone())).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();

    let master = [0x36u8; 32];
    let old_blob = wrap(&master, b"old-secret");
    let old = client.store_escrow(&old_blob).await.unwrap();
    let mut cache = EscrowCache::with_stored_at(old_blob.clone(), old.stored_at);

    // Unchanged refresh: a wrong secret fetches the same blob, and the stamp survives.
    assert_eq!(
        client
            .verify(&mut cache, b"wrong", &master, None)
            .await
            .unwrap(),
        VerifyOutcome::NotVerified
    );
    assert_eq!(store.fetch_count(), 1);
    assert_eq!(cache.blob(), &old_blob);
    assert_eq!(cache.stored_at(), Some(old.stored_at));

    // Changed refresh: another device rotates, and the refreshed copy carries no stamp.
    let new_blob = wrap(&master, b"new-secret");
    client.store_escrow(&new_blob).await.unwrap();
    assert_eq!(
        client
            .verify(&mut cache, b"new-secret", &master, None)
            .await
            .unwrap(),
        VerifyOutcome::Verified
    );
    assert_eq!(store.fetch_count(), 2);
    assert_eq!(cache.blob(), &new_blob);
    assert_eq!(cache.stored_at(), None);
}

/// SSoT § Local Verification stale-cache rule: a rotation on another device makes
/// the *cached* blob stale; verify refreshes once and then passes with the new
/// secret — the stale cache never manufactures a false failure.
#[tokio::test]
async fn verify_refreshes_stale_cache_then_passes() {
    let store = EscrowStore::default();
    let base = start_mock(escrow_handler(store.clone())).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();

    let master = [0x33u8; 32];
    // Enroll and cache the OLD wrap.
    let old_blob = wrap(&master, b"old-secret");
    client.store_escrow(&old_blob).await.unwrap();
    let mut cache = client.fetch_escrow().await.unwrap();

    // Another device rotates the escrow to a NEW secret (server-side replace).
    let new_blob = wrap(&master, b"new-secret");
    *store.blob.lock().unwrap() = Some(capsule_core::cbor::to_canonical_vec(&new_blob).unwrap());

    // Verifying with the NEW secret against the STALE cache: the first compare
    // fails, the stale-cache refresh pulls the new blob, and the retry passes.
    assert_eq!(
        client
            .verify(&mut cache, b"new-secret", &master, None)
            .await
            .unwrap(),
        VerifyOutcome::Verified
    );
    // The cache was updated to the refreshed blob.
    assert_eq!(cache.blob(), &new_blob);
}

/// A genuinely wrong secret still fails after the refresh (the blob was current):
/// the stale-cache rule does not paper over a real failure.
#[tokio::test]
async fn verify_wrong_secret_fails_after_refresh() {
    let store = EscrowStore::default();
    let base = start_mock(escrow_handler(store)).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();

    let master = [0x44u8; 32];
    let blob = wrap(&master, b"real-secret");
    client.store_escrow(&blob).await.unwrap();
    let mut cache = client.fetch_escrow().await.unwrap();

    assert_eq!(
        client
            .verify(&mut cache, b"totally-wrong", &master, None)
            .await
            .unwrap(),
        VerifyOutcome::NotVerified
    );
}

/// SSoT § Guided re-wrap smoke: after the failure threshold the client re-wraps the
/// **same** master key under a fresh secret and replaces the server escrow.
///
/// Proves:
/// - the master key is UNCHANGED — the new escrow unwraps with the new secret to the
///   exact original master bytes;
/// - the old secret unwraps nothing (single active escrow — it is gone everywhere);
/// - re-wrap touches only the wrap: fixture asset blob hashes are byte-identical
///   before and after;
/// - Shamir is re-issued (old shares invalidated) and the old-artifact guidance is
///   surfaced as data.
#[tokio::test]
async fn guided_rewrap_keeps_master_key_and_blob_hashes() {
    use capsule_core::crypto::hash::hash_bytes;

    let store = EscrowStore::default();
    let base = start_mock(escrow_handler(store)).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();

    let master = [0x55u8; 32];
    let old_secret = b"the-old-lost-secret";

    // Enroll the original escrow.
    client
        .store_escrow(&wrap(&master, old_secret))
        .await
        .unwrap();

    // Fixture "asset" ciphertext blobs — re-wrap must not touch these.
    let asset_a = b"encrypted-asset-ciphertext-A".to_vec();
    let asset_b = b"encrypted-asset-ciphertext-B".to_vec();
    let hash_a_before = hash_bytes(&asset_a).to_hex();
    let hash_b_before = hash_bytes(&asset_b).to_hex();

    // Run the guided re-wrap (fast params, Shamir enrolled).
    let rewrap = client
        .guided_rewrap_with_params(&master, fast_params(), true)
        .await
        .unwrap();

    // The new escrow (as stored on the server) unwraps with the NEW secret to the
    // SAME master key.
    let refetched = client.fetch_escrow().await.unwrap();
    assert_eq!(
        refetched.blob(),
        rewrap.new_escrow.blob(),
        "server holds the new escrow"
    );
    assert_eq!(
        rewrap.stored,
        EscrowStored {
            stored_at: mock_stored_at(1),
            replaced: true,
        },
        "the re-wrap displaced the enrolled escrow"
    );
    assert_eq!(
        rewrap.new_escrow.stored_at(),
        Some(rewrap.stored.stored_at),
        "the cached copy carries the server's stored_at"
    );
    let recovered = recover_master_key(refetched.blob(), rewrap.secret.as_bytes()).unwrap();
    assert_eq!(recovered, master, "master key is UNCHANGED after re-wrap");

    // The OLD secret unwraps nothing — the lost secret is dead everywhere.
    assert!(recover_master_key(refetched.blob(), old_secret).is_err());

    // Re-wrap touched only the wrap: asset blob hashes are byte-identical.
    assert_eq!(hash_bytes(&asset_a).to_hex(), hash_a_before);
    assert_eq!(hash_bytes(&asset_b).to_hex(), hash_b_before);

    // Shamir re-issued, old shares invalidated; the new shares reconstruct the new
    // seed (2 of 3).
    let shamir = rewrap.shamir.expect("shamir re-issued when enrolled");
    assert!(shamir.old_shares_invalidated);
    assert_eq!(shamir.shares.len(), 3);
    let two = vec![shamir.shares[0].clone(), shamir.shares[2].clone()];
    assert_eq!(
        capsule_core::backup::recover_seed(&two).unwrap().as_slice(),
        rewrap.secret.as_bytes().as_slice()
    );

    // Type-back gate + old-artifact guidance surfaced as data.
    assert!(rewrap.type_back_required);
    assert!(rewrap.secret.matches(rewrap.secret.as_bytes()));
    assert!(!rewrap.secret.matches(b"something-else"));
    assert_eq!(
        rewrap.old_artifact_guidance,
        Some(OldArtifactGuidance::ReexportOrDestroy)
    );
}

/// A re-wrap on an account with no escrow yet replaced nothing, so it retired no secret
/// and surfaces no old-artifact guidance — noise for a first enrollment.
#[tokio::test]
async fn guided_rewrap_without_a_prior_escrow_has_no_old_artifact_guidance() {
    let base = start_mock(escrow_handler(EscrowStore::default())).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();

    let rewrap = client
        .guided_rewrap_with_params(&[0x56u8; 32], fast_params(), false)
        .await
        .unwrap();
    assert!(!rewrap.stored.replaced);
    assert_eq!(rewrap.old_artifact_guidance, None);
}

/// When Shamir was never enrolled, the re-wrap re-issues no shares.
#[tokio::test]
async fn guided_rewrap_no_shamir_when_not_enrolled() {
    let base = start_mock(escrow_handler(EscrowStore::default())).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();
    let master = [0x66u8; 32];
    client.store_escrow(&wrap(&master, b"old")).await.unwrap();

    let rewrap = client
        .guided_rewrap_with_params(&master, fast_params(), false)
        .await
        .unwrap();
    assert!(rewrap.shamir.is_none());
}

/// The mock's route guard is live: a client pointed one segment off the documented path
/// gets a loud `Unexpected`, not a plausible-looking `NotEnrolled`.
///
/// This is the regression this change exists for, asserted as a property of the *test
/// harness*: without it the mock would answer any path at all, and a wrong route would
/// once again read as "this account has escrowed nothing" — which is what let
/// `backup/escrow` survive a contract re-source.
#[tokio::test]
async fn a_call_off_the_documented_route_is_not_mistaken_for_an_empty_escrow() {
    let base = start_mock(escrow_handler(EscrowStore::default())).await;
    let client =
        RecoveryClient::new(session_for(&base), &format!("{base}/not-the-contract")).unwrap();
    let error = client
        .fetch_escrow()
        .await
        .expect_err("a path the server does not serve is not an empty escrow");
    assert!(
        matches!(error, RecoveryError::Unexpected { status: 501, .. }),
        "got {error:?}"
    );
}

/// A `400` refusal becomes the typed `Malformed` and carries **the server's own** code —
/// not the `Unexpected { status }` the hand-written path used to collapse it into, and not
/// a constant this module guessed.
#[tokio::test]
async fn a_refused_blob_is_malformed_with_the_servers_code() {
    let handler: Handler = Arc::new(|_req| {
        Box::pin(async move {
            MockResponse::problem(
                400,
                error_codes::ESCROW_MALFORMED,
                "the escrow blob is empty",
            )
        })
    });
    let base = start_mock(handler).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();
    let error = client
        .store_escrow(&wrap(&[0x77u8; 32], b"whatever"))
        .await
        .expect_err("the server refused the blob");
    assert!(
        matches!(error, RecoveryError::Malformed { .. }),
        "got {error:?}"
    );
    assert_eq!(error.error_code(), Some(error_codes::ESCROW_MALFORMED));
}

/// The store answering `500` is its own variant carrying `error.escrow.unavailable`, not a
/// bare transport failure: a client that localizes codes has one to show, and the cadence
/// can tell "the server is unwell" from "the network is gone".
#[tokio::test]
async fn an_unavailable_store_keeps_its_catalog_code() {
    let handler: Handler = Arc::new(|_req| {
        Box::pin(async move {
            MockResponse::problem(
                500,
                error_codes::ESCROW_UNAVAILABLE,
                "the escrow could not be read",
            )
        })
    });
    let base = start_mock(handler).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();
    let error = client.fetch_escrow().await.expect_err("the store is down");
    assert!(
        matches!(error, RecoveryError::Unavailable { .. }),
        "got {error:?}"
    );
    assert_eq!(error.error_code(), Some(error_codes::ESCROW_UNAVAILABLE));
}

/// **An unreachable server is not an expired session.** reqwest reports a refused
/// connection as a *request* error, which the generated taxonomy files under
/// `RequestConstruction` next to the genuine pre-flight failures — so classifying that
/// whole class as an auth failure would tell an offline device to sign in again, the one
/// remedy that cannot work without a network.
#[tokio::test]
async fn an_unreachable_endpoint_is_a_transport_failure_not_an_auth_one() {
    // Bind, read the port, then drop the listener: the address is now certain to refuse.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    crate::net::negotiation::forget_addr(listener.local_addr().unwrap());
    let addr = listener.local_addr().unwrap();
    drop(listener);

    // The session is built against a live mock, so the session itself is healthy and the
    // only thing wrong is the escrow origin.
    let live = start_mock(escrow_handler(EscrowStore::default())).await;
    let client = RecoveryClient::new(session_for(&live), &format!("http://{addr}")).unwrap();

    let error = client
        .fetch_escrow()
        .await
        .expect_err("nothing is listening there");
    assert!(
        matches!(error, RecoveryError::Transport(_)),
        "an unreachable endpoint must be transport, got {error:?}"
    );
    assert_eq!(error.error_code(), None);
}

/// **A session that cannot mint a bearer is an auth failure, not a network one.**
///
/// This is the other side of `an_unreachable_endpoint_is_a_transport_failure_not_an_auth_one`
/// and it pins the discrimination that separates them. Both arrive as the generated
/// taxonomy's `RequestConstruction`; the only thing telling them apart is the boxed source,
/// which is the runtime's own `AuthError` when — and only when — the bearer provider is
/// what failed. Without this case, a spargen change to how a provider failure is boxed
/// would silently demote every expired refresh token to `Transport`, and the FFI would tell
/// a user to retry where it must tell them to sign in again. The escrow calls themselves
/// never leave the process here: there is no bearer to send them with.
#[tokio::test]
async fn a_session_that_cannot_mint_a_bearer_is_an_auth_failure() {
    let base = start_mock(escrow_handler(EscrowStore::default())).await;
    let client = RecoveryClient::new(dead_session_for(&base), &base).unwrap();

    let error = client
        .fetch_escrow()
        .await
        .expect_err("the session cannot produce a token");
    assert!(
        matches!(error, RecoveryError::Unauthorized { code: None, .. }),
        "a dead session must reach the caller as an auth failure, got {error:?}"
    );
    assert_eq!(
        error.error_code(),
        None,
        "no server answered, so there is no catalog code to carry"
    );

    // And the same on the write path, which has a body to construct and still fails before
    // it is sent.
    let error = client
        .store_escrow(&wrap(&[0x99u8; 32], b"whatever"))
        .await
        .expect_err("the session cannot produce a token");
    assert!(
        matches!(error, RecoveryError::Unauthorized { code: None, .. }),
        "got {error:?}"
    );
}

/// A refusal whose body is not the coded problem the document promises — an intermediary
/// answering a bare `404`, say — is a broken path, not an empty escrow.
///
/// This is the deliberate class change the route fix rests on: the hand-written client
/// read *any* `404` as `NotEnrolled`, which is exactly why a wrong route looked like an
/// account that had escrowed nothing.
#[tokio::test]
async fn an_uncoded_404_is_not_read_as_an_empty_escrow() {
    let handler: Handler = Arc::new(|_req| {
        Box::pin(async move { MockResponse::bytes(404, b"<html>not found</html>".to_vec()) })
    });
    let base = start_mock(handler).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();
    let error = client
        .fetch_escrow()
        .await
        .expect_err("an unparseable refusal is not an enrollment state");
    assert!(
        matches!(error, RecoveryError::Transport(_)),
        "got {error:?}"
    );
}

/// A refused credential carries the problem body's code straight through, so a client
/// localizes what the *server* said rather than what this module assumed.
#[tokio::test]
async fn a_refused_credential_keeps_the_problem_code() {
    // No bearer reaches the mock's handler at all: it answers `401` at the door, which is
    // precisely the shape a revoked token produces.
    let handler: Handler = Arc::new(|_req| {
        Box::pin(async move {
            MockResponse::problem(
                401,
                error_codes::REQUEST_UNAUTHENTICATED,
                "the access token was refused",
            )
        })
    });
    let base = start_mock(handler).await;
    let client = RecoveryClient::new(session_for(&base), &base).unwrap();
    let error = client
        .fetch_escrow()
        .await
        .expect_err("the credential was refused");
    assert!(
        matches!(error, RecoveryError::Unauthorized { .. }),
        "got {error:?}"
    );
    assert_eq!(
        error.error_code(),
        Some(error_codes::REQUEST_UNAUTHENTICATED)
    );
}

/// The minted secret clears the ≥128-bit entropy floor (256-bit) and never prints
/// its material.
#[test]
fn minted_secret_is_256_bit_and_opaque() {
    let secret = MintedSecret::generate();
    assert_eq!(secret.as_bytes().len(), 32);
    assert_eq!(secret.to_hex().len(), 64);
    assert_eq!(format!("{secret:?}"), "MintedSecret { .. }");
}

/// **The protocol window (issue #674).** A `426` on either escrow write — documented or
/// undocumented — is the typed upgrade refusal with the protocol code, never `Unexpected`.
#[test]
fn an_escrow_write_426_is_the_typed_upgrade_refusal() {
    fn gate_426<E>() -> rest::Error<E> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "x-capsule-protocol-min",
            reqwest::header::HeaderValue::from_static("2099-01-01"),
        );
        rest::Error::UnexpectedStatus {
            status: reqwest::StatusCode::UPGRADE_REQUIRED,
            headers,
            body:
                br#"{"status":426,"detail":"too old","code":"error.protocol.version_unsupported"}"#
                    .to_vec()
                    .into(),
        }
    }
    for error in [
        store_escrow_error(gate_426()),
        store_sealed_identity_error(gate_426()),
        store_album_keys_error(gate_426()),
    ] {
        let RecoveryError::UpgradeRequired(refusal) = &error else {
            panic!("expected UpgradeRequired, got {error:?}");
        };
        assert_eq!(
            refusal.reason,
            crate::net::negotiation::UpgradeReason::ProtocolBelowWindow
        );
        assert_eq!(refusal.message, "too old");
        assert_eq!(
            error.error_code(),
            Some(error_codes::PROTOCOL_VERSION_UNSUPPORTED)
        );
    }
}

/// #741: a listing entry reads into its album id, its blob decoded and its instant, and an
/// entry whose id, blob or instant cannot be read is an `InvalidResponse`, never a guess.
#[test]
fn an_album_key_listing_entry_reads_or_names_what_is_unreadable() {
    let entry = |album_id: &str, blob: &str, stored_at: &str| rest::types::EscrowedAlbumKeys {
        album_id: album_id.to_owned(),
        blob: blob.to_owned(),
        stored_at: stored_at.to_owned(),
    };
    let album = "0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e35";
    let read = read_escrowed_album_keys(entry(album, "AP8B", "2026-10-22T10:00:00Z"))
        .expect("a well-formed entry reads");
    assert_eq!(read.album_id, Uuid::parse_str(album).unwrap());
    assert_eq!(read.blob, vec![0x00, 0xff, 0x01]);
    assert_eq!(
        read.stored_at,
        "2026-10-22T10:00:00Z".parse::<Timestamp>().unwrap()
    );

    for bad in [
        entry("not-a-uuid", "AP8B", "2026-10-22T10:00:00Z"),
        entry(album, "not base64!", "2026-10-22T10:00:00Z"),
        entry(album, "AP8B", "yesterday"),
    ] {
        assert!(matches!(
            read_escrowed_album_keys(bad),
            Err(RecoveryError::InvalidResponse(_))
        ));
    }
}
