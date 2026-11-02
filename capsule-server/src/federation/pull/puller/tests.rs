//! The production puller against a scripted home server: what it presents, what it writes, and
//! how each way a peer can fail is classified.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::provenance::Action;
use jiff::Timestamp;

use super::*;
use crate::federation::capability::{CapabilityCodec, MintRequest, Scope};
use crate::federation::pull::fixtures::{
    METADATA, PIN, album, asset, core, entry, now, page_json, record,
};
use crate::federation::pull::pulled::{InMemoryPulledAlbums, PulledAlbum};
use crate::federation::pull::signer::Ed25519RequestSigner;
use crate::federation::scheme::{REQUEST_SIGNATURE_HEADER, RequestClaim, RequestSignature};
use crate::federation::{InMemoryPeers, InMemorySubscriptions};
use crate::store::memory::ManualClock;

const HOME: &str = "home.test";
const HERE: &str = "here.test";

/// One scripted answer.
#[derive(Debug, Clone)]
struct Canned {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Canned {
    fn json(status: u16, body: &serde_json::Value) -> Self {
        Self {
            status,
            content_type: if status < 300 {
                "application/json"
            } else {
                "application/problem+json"
            },
            body: body.to_string().into_bytes(),
        }
    }

    fn problem(status: u16, code: &str) -> Self {
        Self::json(
            status,
            &serde_json::json!({ "type": "about:blank", "title": "refused", "status": status, "code": code }),
        )
    }
}

/// What the scripted home server was asked.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    url: reqwest::Url,
    bearer: Option<String>,
    signature: Option<String>,
}

/// A home server that answers by path: pages in order, blobs by address, one refresh answer.
#[derive(Debug, Default)]
struct HomeServer {
    pages: Mutex<VecDeque<Canned>>,
    blobs: Mutex<BTreeMap<String, Vec<u8>>>,
    refresh: Mutex<Option<Canned>>,
    seen: Mutex<Vec<Seen>>,
}

impl HomeServer {
    fn serve_page(&self, page: Canned) {
        self.pages.lock().expect("unpoisoned").push_back(page);
    }

    fn serve_blob(&self, bytes: &[u8]) {
        self.blobs
            .lock()
            .expect("unpoisoned")
            .insert(hash_bytes(bytes).to_hex(), bytes.to_vec());
    }

    fn serve_blob_at(&self, address: &str, bytes: &[u8]) {
        self.blobs
            .lock()
            .expect("unpoisoned")
            .insert(address.to_owned(), bytes.to_vec());
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("unpoisoned").clone()
    }

    fn seen_at(&self, path: &str) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|seen| seen.url.path() == path)
            .collect()
    }

    fn answer(&self, request: &reqwest::Request) -> Canned {
        let path = request.url().path();
        if path == "/v1/sync" {
            return self
                .pages
                .lock()
                .expect("unpoisoned")
                .pop_front()
                .unwrap_or_else(|| Canned::problem(500, "error.internal"));
        }
        if path == "/v1/federation/capabilities/refresh" {
            return self
                .refresh
                .lock()
                .expect("unpoisoned")
                .clone()
                .unwrap_or_else(|| Canned::problem(500, "error.internal"));
        }
        if let Some(address) = path.strip_prefix("/v1/blob/") {
            return match self.blobs.lock().expect("unpoisoned").get(address) {
                Some(bytes) => Canned {
                    status: 200,
                    content_type: "application/octet-stream",
                    body: bytes.clone(),
                },
                None => Canned::problem(404, "error.blob.not_found"),
            };
        }
        Canned::problem(404, "error.not_found")
    }
}

impl rest::HttpBackend for HomeServer {
    fn execute(&self, request: reqwest::Request) -> rest::ExecuteFuture<'_> {
        let header = |name: &str| {
            request
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        self.seen.lock().expect("unpoisoned").push(Seen {
            method: request.method().to_string(),
            url: request.url().clone(),
            bearer: header("authorization")
                .and_then(|value| value.strip_prefix("Bearer ").map(str::to_owned)),
            signature: header(REQUEST_SIGNATURE_HEADER),
        });
        let canned = self.answer(&request);
        Box::pin(async move {
            let mut response = http::Response::new(canned.body);
            *response.status_mut() =
                reqwest::StatusCode::from_u16(canned.status).expect("a status");
            response.headers_mut().insert(
                reqwest::header::CONTENT_TYPE,
                reqwest::header::HeaderValue::from_static(canned.content_type),
            );
            Ok(reqwest::Response::from(response))
        })
    }
}

/// A puller, the home server it pulls from, and every store it reads and writes.
struct Harness {
    home: Arc<HomeServer>,
    issuer: CapabilityCodec,
    clock: Arc<ManualClock>,
    subscriptions: Arc<InMemorySubscriptions>,
    pulled: Arc<InMemoryPulledAlbums>,
    signer: Arc<Ed25519RequestSigner>,
    puller: RemotePuller,
}

fn job() -> JobKey {
    JobKey::new(PeerId::new(HOME), album())
}

fn pkcs8() -> Vec<u8> {
    let random = ring::rand::SystemRandom::new();
    ring::signature::Ed25519KeyPair::generate_pkcs8(&random)
        .expect("a key")
        .as_ref()
        .to_vec()
}

impl Harness {
    /// A member of this server subscribed to the album with a capability that lives a day.
    async fn new() -> Self {
        let clock = Arc::new(ManualClock::new(now()));
        let issuer = CapabilityCodec::from_pkcs8(&pkcs8(), HOME, clock.clone()).expect("a codec");
        let peers = Arc::new(InMemoryPeers::new());
        let key: [u8; 32] = issuer.public_key().try_into().expect("32 bytes");
        peers
            .pin(&PeerId::new(HOME), key, Timestamp::UNIX_EPOCH)
            .await
            .expect("pinned");
        let subscriptions = Arc::new(InMemorySubscriptions::new());
        let pulled = Arc::new(InMemoryPulledAlbums::new());
        let signer = Arc::new(Ed25519RequestSigner::from_pkcs8(&pkcs8()).expect("a signer"));
        let home = Arc::new(HomeServer::default());
        let puller = RemotePuller::new(RemotePullerParts {
            subscriptions: subscriptions.clone(),
            peers,
            pulled: pulled.clone(),
            signer: signer.clone(),
            backend: home.clone(),
            clock: clock.clone(),
            policy: UploadPolicy::default(),
            server_id: PeerId::new(HERE),
        });
        let harness = Self {
            home,
            issuer,
            clock,
            subscriptions,
            pulled,
            signer,
            puller,
        };
        let token = harness.mint(SignedDuration::from_hours(24));
        harness.subscribe(&token).await;
        harness
    }

    /// A capability the home server issues to this server for the album.
    fn mint(&self, ttl: SignedDuration) -> String {
        self.issuer
            .mint(&MintRequest {
                peer: PeerId::new(HERE),
                album: album(),
                scope: Scope::Read,
                min_protocol_version: PIN.to_owned(),
                ttl,
            })
            .expect("minted")
            .token
    }

    async fn subscribe(&self, token: &str) {
        let grant = self.issuer.verify(token).expect("it verifies");
        self.subscriptions
            .put(Subscription {
                subscriber: crate::store::UserId::new("member"),
                peer: PeerId::new(HOME),
                album: grant.album,
                scope: grant.scope,
                jti: grant.jti,
                token: CapabilityToken::new(token),
                issued_at: grant.issued_at,
                expires_at: grant.expires_at,
                min_protocol_version: grant.min_protocol_version,
                subscribed_at: now(),
                updated_at: now(),
            })
            .await
            .expect("subscribed");
    }

    async fn pull(&self, tier: PeerTier) -> PullReport {
        self.puller.pull(&job(), tier).await
    }

    async fn state(&self) -> PulledAlbum {
        self.pulled.read(&job()).await.expect("read")
    }

    async fn tokens(&self) -> Vec<String> {
        self.subscriptions
            .of_job(&job())
            .await
            .expect("read")
            .into_iter()
            .map(|subscription| subscription.token.expose().to_owned())
            .collect()
    }
}

fn create(n: u128) -> Vec<u8> {
    record(core(asset(n), Action::Create, None, 1))
}

fn a_page(entries: Vec<serde_json::Value>, cursor: &str, has_more: bool) -> Canned {
    Canned::json(200, &page_json(entries, cursor, has_more))
}

fn failure(report: &PullReport) -> &'static str {
    match &report.result {
        Ok(()) => "ok",
        Err(failure) => failure.kind(),
    }
}

#[tokio::test]
async fn a_pull_writes_every_page_that_verifies_and_reports_its_validation_time() {
    let harness = Harness::new().await;
    let (one, two) = (create(1), create(2));
    harness.home.serve_page(a_page(
        vec![entry(asset(1), 1, "created", Some(&one))],
        "c1",
        true,
    ));
    harness.home.serve_page(a_page(
        vec![entry(asset(2), 2, "created", Some(&two))],
        "c2",
        false,
    ));
    harness.home.serve_blob(METADATA);

    let report = harness.pull(PeerTier::Established).await;
    assert_eq!(report.result, Ok(()));
    assert!(
        !report.validation.is_zero(),
        "screening real manifests takes time"
    );

    let written = harness.pulled.entries(&job());
    assert_eq!(written.len(), 2);
    assert_eq!(written[0].manifest.as_deref(), Some(one.as_slice()));
    assert_eq!(
        harness.pulled.metadata_blob(&job(), &hash_bytes(METADATA)),
        Some(METADATA.to_vec())
    );
    let state = harness.state().await;
    assert_eq!(state.cursor.as_deref(), Some("c2"));
    assert_eq!(state.high_water, Some(2));

    // The second page resumed from the first one's cursor.
    let syncs = harness.home.seen_at("/v1/sync");
    assert_eq!(syncs.len(), 2);
    let cursor = |seen: &Seen| {
        seen.url
            .query_pairs()
            .find(|(key, _)| key == "cursor")
            .map(|(_, value)| value.into_owned())
    };
    assert_eq!(cursor(&syncs[0]), None);
    assert_eq!(cursor(&syncs[1]).as_deref(), Some("c1"));
}

#[tokio::test]
async fn every_request_goes_to_the_pinned_origin_presenting_a_signed_capability() {
    let harness = Harness::new().await;
    let one = create(1);
    harness.home.serve_page(a_page(
        vec![entry(asset(1), 1, "created", Some(&one))],
        "c1",
        false,
    ));
    harness.home.serve_blob(METADATA);
    assert_eq!(harness.pull(PeerTier::Established).await.result, Ok(()));

    let token = harness.tokens().await.remove(0);
    let seen = harness.home.seen();
    assert_eq!(seen.len(), 2, "a page and its metadata blob");
    for request in seen {
        assert_eq!(request.url.scheme(), "https");
        assert_eq!(request.url.host_str(), Some(HOME));
        assert_eq!(request.bearer.as_deref(), Some(token.as_str()));
        let signature =
            RequestSignature::parse(request.signature.as_deref().expect("signed")).expect("parses");
        let claim = RequestClaim::new(
            &request.method,
            request.url.path(),
            &token,
            signature.signed_at,
            &signature.nonce,
        );
        signature
            .verify(&claim, &harness.signer.public_key())
            .expect("the home server's verifier admits it");
        assert!(signature.is_fresh(now()));
    }
}

#[tokio::test]
async fn a_page_that_fails_validation_writes_nothing_and_spends_the_error_budget() {
    let harness = Harness::new().await;
    let one = create(1);
    harness.home.serve_page(a_page(
        vec![entry(asset(1), 1, "created", Some(&one))],
        "c1",
        true,
    ));
    let mut stray = entry(asset(2), 2, "created", Some(&create(2)));
    stray["album_id"] = serde_json::json!(uuid::Uuid::from_u128(99).to_string());
    harness.home.serve_page(a_page(vec![stray], "c2", false));
    harness.home.serve_blob(METADATA);

    let report = harness.pull(PeerTier::Established).await;
    assert_eq!(failure(&report), "validation");
    assert!(!report.validation.is_zero());
    // The first page verified and stays written; nothing of the second is.
    assert_eq!(harness.pulled.entries(&job()).len(), 1);
    assert_eq!(harness.state().await.high_water, Some(1));
}

#[tokio::test]
async fn a_metadata_blob_that_is_not_its_address_writes_nothing() {
    let harness = Harness::new().await;
    let one = create(1);
    harness.home.serve_page(a_page(
        vec![entry(asset(1), 1, "created", Some(&one))],
        "c1",
        false,
    ));
    let mut forged = METADATA.to_vec();
    forged[0] ^= 1;
    harness
        .home
        .serve_blob_at(&hash_bytes(METADATA).to_hex(), &forged);

    let report = harness.pull(PeerTier::Established).await;
    assert_eq!(failure(&report), "validation");
    assert!(harness.pulled.entries(&job()).is_empty());
    assert_eq!(harness.state().await, PulledAlbum::default());
}

#[tokio::test]
async fn a_response_past_its_bound_is_a_validation_failure() {
    let harness = Harness::new().await;
    let rules = Rules::for_tier(PeerTier::Probation);
    let padding = "x".repeat(usize::try_from(rules.max_page_bytes).expect("fits"));
    let mut oversized = page_json(Vec::new(), "c", false);
    oversized["padding"] = serde_json::json!(padding);
    harness.home.serve_page(Canned::json(200, &oversized));

    let report = harness.pull(PeerTier::Probation).await;
    assert_eq!(failure(&report), "validation");
    assert_eq!(harness.state().await, PulledAlbum::default());
}

#[tokio::test]
async fn a_peer_that_fails_on_its_side_or_refuses_only_backs_off() {
    for answer in [
        Canned::problem(503, "error.internal"),
        Canned::problem(500, "error.internal"),
        Canned::problem(403, "error.federation.capability_revoked"),
        Canned::problem(429, "error.federation.rate_budget_exceeded"),
    ] {
        let harness = Harness::new().await;
        harness.home.serve_page(answer.clone());
        let report = harness.pull(PeerTier::Established).await;
        assert_eq!(failure(&report), "transport", "{}", answer.status);
        assert!(
            report.validation.is_zero(),
            "nothing of the peer's was validated"
        );
        assert_eq!(harness.state().await, PulledAlbum::default());
    }
}

#[tokio::test]
async fn a_stale_cursor_restarts_the_album_from_zero_keeping_its_heads() {
    let harness = Harness::new().await;
    let one = create(1);
    harness.home.serve_page(a_page(
        vec![entry(asset(1), 1, "created", Some(&one))],
        "c1",
        false,
    ));
    harness.home.serve_blob(METADATA);
    assert_eq!(harness.pull(PeerTier::Established).await.result, Ok(()));

    harness
        .home
        .serve_page(Canned::problem(409, "error.sync.cursor_stale"));
    let report = harness.pull(PeerTier::Established).await;
    assert_eq!(failure(&report), "transport");
    let state = harness.state().await;
    assert_eq!(state.cursor, None);
    assert_eq!(state.high_water, None);
    assert_eq!(state.assets[&asset(1).to_string()].head, hash_bytes(&one));

    // The resync from zero serves the held head again, which is no revival.
    harness.home.serve_page(a_page(
        vec![entry(asset(1), 1, "created", Some(&one))],
        "c1",
        false,
    ));
    assert_eq!(harness.pull(PeerTier::Established).await.result, Ok(()));
}

#[tokio::test]
async fn a_job_with_no_live_capability_asks_the_peer_nothing() {
    let harness = Harness::new().await;
    harness.clock.advance(SignedDuration::from_hours(25));
    let report = harness.pull(PeerTier::Established).await;
    assert_eq!(failure(&report), "transport");
    assert!(harness.home.seen().is_empty());
}

#[tokio::test]
async fn the_longest_lived_capability_is_presented() {
    let harness = Harness::new().await;
    harness.clock.advance(SignedDuration::from_hours(1));
    // A second member's capability, minted an hour later, outlives the first.
    let later = harness.mint(SignedDuration::from_hours(24));
    let grant = harness.issuer.verify(&later).expect("verifies");
    harness
        .subscriptions
        .put(Subscription {
            subscriber: crate::store::UserId::new("another member"),
            peer: PeerId::new(HOME),
            album: grant.album,
            scope: grant.scope,
            jti: grant.jti,
            token: CapabilityToken::new(later.clone()),
            issued_at: grant.issued_at,
            expires_at: grant.expires_at,
            min_protocol_version: grant.min_protocol_version,
            subscribed_at: now(),
            updated_at: now(),
        })
        .await
        .expect("subscribed");
    harness.home.serve_page(a_page(Vec::new(), "c0", false));

    assert_eq!(harness.pull(PeerTier::Established).await.result, Ok(()));
    assert_eq!(
        harness.home.seen()[0].bearer.as_deref(),
        Some(later.as_str())
    );
}

/// The refresh answer the home server would give for `token`.
fn refreshed(issuer: &CapabilityCodec, token: &str) -> Canned {
    let grant = issuer.verify(token).expect("verifies");
    Canned::json(
        200,
        &serde_json::json!({
            "token": token,
            "jti": grant.jti,
            "expires_at": grant.expires_at.to_string(),
            "not_after": grant.expires_at.to_string(),
            "replayed": false,
        }),
    )
}

#[tokio::test]
async fn a_capability_near_its_end_is_refreshed_verified_kept_and_presented() {
    let harness = Harness::new().await;
    harness.clock.advance(SignedDuration::from_hours(19));
    let successor = harness.mint(SignedDuration::from_hours(24));
    *harness.home.refresh.lock().expect("unpoisoned") =
        Some(refreshed(&harness.issuer, &successor));
    harness.home.serve_page(a_page(Vec::new(), "c0", false));

    let report = harness.pull(PeerTier::Established).await;
    assert_eq!(report.result, Ok(()));
    assert!(
        !report.validation.is_zero(),
        "verifying the successor is validation"
    );
    assert_eq!(harness.tokens().await, vec![successor.clone()]);

    let refresh = harness.home.seen_at("/v1/federation/capabilities/refresh");
    assert_eq!(refresh.len(), 1);
    assert_eq!(refresh[0].method, "POST");
    assert_ne!(refresh[0].bearer.as_deref(), Some(successor.as_str()));
    let sync = harness.home.seen_at("/v1/sync");
    assert_eq!(sync[0].bearer.as_deref(), Some(successor.as_str()));
}

#[tokio::test]
async fn a_capability_with_time_left_is_not_refreshed() {
    let harness = Harness::new().await;
    harness.home.serve_page(a_page(Vec::new(), "c0", false));
    assert_eq!(harness.pull(PeerTier::Established).await.result, Ok(()));
    assert!(
        harness
            .home
            .seen_at("/v1/federation/capabilities/refresh")
            .is_empty()
    );
}

#[tokio::test]
async fn a_successor_that_does_not_verify_is_never_kept() {
    let harness = Harness::new().await;
    let before = harness.tokens().await;
    harness.clock.advance(SignedDuration::from_hours(19));
    // Signed by a key the operator never pinned for the home server.
    let impostor =
        CapabilityCodec::from_pkcs8(&pkcs8(), HOME, harness.clock.clone()).expect("a codec");
    let forged = impostor
        .mint(&MintRequest {
            peer: PeerId::new(HERE),
            album: album(),
            scope: Scope::Read,
            min_protocol_version: PIN.to_owned(),
            ttl: SignedDuration::from_hours(24),
        })
        .expect("minted")
        .token;
    *harness.home.refresh.lock().expect("unpoisoned") = Some(refreshed(&impostor, &forged));

    let report = harness.pull(PeerTier::Established).await;
    assert_eq!(failure(&report), "validation");
    assert_eq!(harness.tokens().await, before);
    assert!(harness.home.seen_at("/v1/sync").is_empty());
}

#[tokio::test]
async fn a_successor_for_another_album_is_never_kept() {
    let harness = Harness::new().await;
    let before = harness.tokens().await;
    harness.clock.advance(SignedDuration::from_hours(19));
    let other = harness
        .issuer
        .mint(&MintRequest {
            peer: PeerId::new(HERE),
            album: crate::store::AlbumId::new(uuid::Uuid::from_u128(99).to_string()),
            scope: Scope::Read,
            min_protocol_version: PIN.to_owned(),
            ttl: SignedDuration::from_hours(24),
        })
        .expect("minted")
        .token;
    *harness.home.refresh.lock().expect("unpoisoned") = Some(refreshed(&harness.issuer, &other));

    let report = harness.pull(PeerTier::Established).await;
    assert_eq!(failure(&report), "validation");
    assert_eq!(harness.tokens().await, before);
}

#[tokio::test]
async fn a_refused_refresh_pulls_with_the_capability_still_live() {
    let harness = Harness::new().await;
    let before = harness.tokens().await;
    harness.clock.advance(SignedDuration::from_hours(19));
    *harness.home.refresh.lock().expect("unpoisoned") =
        Some(Canned::problem(403, "error.federation.capability_expired"));
    harness.home.serve_page(a_page(Vec::new(), "c0", false));

    assert_eq!(harness.pull(PeerTier::Established).await.result, Ok(()));
    assert_eq!(harness.tokens().await, before);
    assert_eq!(
        harness.home.seen_at("/v1/sync")[0].bearer.as_deref(),
        Some(before[0].as_str())
    );
}

#[tokio::test]
async fn a_peer_on_probation_is_read_in_smaller_steps() {
    let harness = Harness::new().await;
    let rules = Rules::for_tier(PeerTier::Probation);
    // A peer that always claims more: the pull stops at its page budget.
    for seq in 1..=u64::from(rules.max_pages) + 2 {
        harness
            .home
            .serve_page(a_page(Vec::new(), &format!("c{seq}"), true));
    }
    let report = harness.pull(PeerTier::Probation).await;
    assert_eq!(report.result, Ok(()));
    // An empty page ends the pull even when the peer claims more.
    let syncs = harness.home.seen_at("/v1/sync");
    assert_eq!(syncs.len(), 1);
    let size = syncs[0]
        .url
        .query_pairs()
        .find(|(key, _)| key == "page_size")
        .map(|(_, value)| value.into_owned());
    assert_eq!(size, Some(rules.page_size.to_string()));
}

#[tokio::test]
async fn a_pull_stops_at_its_page_budget() {
    let harness = Harness::new().await;
    let rules = Rules::for_tier(PeerTier::Probation);
    let extra = u64::from(rules.max_pages) + 2;
    for seq in 1..=extra {
        let bytes = create(u128::from(seq));
        harness.home.serve_page(a_page(
            vec![entry(asset(u128::from(seq)), seq, "created", Some(&bytes))],
            &format!("c{seq}"),
            true,
        ));
    }
    harness.home.serve_blob(METADATA);
    assert_eq!(harness.pull(PeerTier::Probation).await.result, Ok(()));
    assert_eq!(
        harness.home.seen_at("/v1/sync").len(),
        rules.max_pages as usize
    );
    assert_eq!(
        harness.state().await.high_water,
        Some(u64::from(rules.max_pages))
    );
}

#[tokio::test]
async fn a_successor_longer_than_a_subscription_keeps_is_never_kept() {
    let harness = Harness::new().await;
    let before = harness.tokens().await;
    harness.clock.advance(SignedDuration::from_hours(19));
    *harness.home.refresh.lock().expect("unpoisoned") = Some(Canned::json(
        200,
        &serde_json::json!({
            "token": "t".repeat(MAX_CAPABILITY_BYTES + 1),
            "jti": "j",
            "expires_at": "2026-06-02T00:00:00Z",
            "not_after": "2026-06-02T00:00:00Z",
            "replayed": false,
        }),
    ));

    let report = harness.pull(PeerTier::Established).await;
    assert_eq!(failure(&report), "validation");
    assert_eq!(harness.tokens().await, before);
}
