//! Pure (no-DB) unit tests for the federation runtime logic (slice `S-E2`).
//!
//! Coverage of the federation doc's Validation bullets that are unit-tier, plus the invariant
//! 19–21 rejecting tests (status + `error.*` code):
//! - `capability_token_verify_rejects_each_mutation` — bullet 1 (invariant 19).
//! - `revocation_list_fails_closed_when_stale` — bullet 2.
//! - `pull_boundary_rejects_each_invariant` — bullet 3 (invariant 20, checks 1–18 + 25).
//! - `soft_fail_table_is_bounded_lru` — bullet 6.
//! - `rate_budget_and_circuit_breaker` — bullets 4 & 5 at the state-machine level (invariant 21).
//! - `federation_reject_maps_to_status_and_code` — invariant 19/21 → HTTP status + `error.*` code.
//! - `scope_enforced_by_blob_role` — invariant-19 scope-by-role.
//! (The smoke/E2E halves — a full pull against a testcontainer Postgres, cross-peer consistency,
//! and E2E case 4 — live in `crate::tests::federation`.)

#![allow(clippy::unwrap_used)]

use base64::Engine as _;
use capsule_core::crypto::CRYPTO_SUITE_ID;
use capsule_core::crypto::encryption::{blob_ciphertext_hash, seal_blob};
use capsule_i18n::error_codes;
use jiff::{SignedDuration, Timestamp};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, encode};
use ring::signature::{Ed25519KeyPair, KeyPair};
use service::sync::{FeedBlobManifest, FeedBlobRef};

use super::FederationReject;
use super::capability::{
    CapabilityClaims, CapabilityIssuer, CapabilityReject, FederationScope, IssueParams,
    VerifyContext, album_urn, authorize_blob_role, verify_capability,
};
use super::compartment::{CompartmentReject, PeerLimits, PeerRegistry, PeerTier, PullCost};
use super::pull::{PullValidationContext, PulledEnvelope, revalidate_pulled};
use super::rejected::RejectedHashTable;
use super::revocation::{RevocationList, RevocationVerdict};

// Two independent Ed25519 pkcs8 keypairs (base64 DER) so a "verify under the wrong key" case is
// a genuine signature mismatch, not a tampered byte.
const ISSUER_PKCS8: &str = "MC4CAQAwBQYDK2VwBCIEIN6eTvXEL7xMZWHY8rTk7VbQSGSuRkle5MVfiiYUStLF";
const OTHER_PKCS8: &str = "MC4CAQAwBQYDK2VwBCIEIG73KilXg8qazIq8mNGzuPEHYPLY3WXR1uOS7ZxNkefV";

const ISSUER_ID: &str = "home.tld";
const PEER_ID: &str = "other.tld";
const ALBUM: &str = "album-abc";

fn keypair(pkcs8_b64: &str) -> (EncodingKey, DecodingKey) {
    let doc = base64::engine::general_purpose::STANDARD
        .decode(pkcs8_b64)
        .unwrap();
    let pair = Ed25519KeyPair::from_pkcs8_maybe_unchecked(&doc).unwrap();
    (
        EncodingKey::from_ed_der(&doc),
        DecodingKey::from_ed_der(pair.public_key().as_ref()),
    )
}

fn now() -> Timestamp {
    "2026-05-31T00:00:00Z".parse().unwrap()
}

fn verify_ctx(now: Timestamp) -> VerifyContext<'static> {
    VerifyContext {
        expected_issuer: ISSUER_ID,
        album_id: ALBUM,
        now,
    }
}

/// Sign an arbitrary claims payload under the issuer key (for constructing mutated tokens).
fn sign(claims: &CapabilityClaims, key: &EncodingKey) -> String {
    encode(&Header::new(Algorithm::EdDSA), claims, key).unwrap()
}

/// A well-formed set of claims: `read` scope, 1 h TTL, in-window protocol, all fields present.
fn valid_claims(now: Timestamp) -> CapabilityClaims {
    CapabilityClaims {
        iss: ISSUER_ID.to_string(),
        sub: PEER_ID.to_string(),
        aud: album_urn(ALBUM),
        scope: FederationScope::Read,
        iat: now.to_string(),
        exp: now
            .checked_add(SignedDuration::from_hours(1))
            .unwrap()
            .to_string(),
        nbf: now.to_string(),
        jti: "0192f000-0000-7000-8000-000000000001".to_string(),
        min_protocol_version: "2026-05-31".to_string(),
    }
}

// ─── Bullet 1: capability token verify (unit, invariant 19) ──────────────────────────────────

#[test]
fn capability_token_verify_rejects_each_mutation() {
    let (enc, dec) = keypair(ISSUER_PKCS8);
    let (_other_enc, other_dec) = keypair(OTHER_PKCS8);
    let now = now();
    let empty = RevocationList::owned(Vec::<String>::new());
    let ctx = verify_ctx(now);

    // A freshly issued token verifies.
    let issuer = CapabilityIssuer::new(ISSUER_ID, enc.clone());
    let minted = issuer
        .issue(
            &IssueParams {
                peer: PEER_ID,
                album_id: ALBUM,
                scope: FederationScope::Read,
                min_protocol_version: "2026-05-31",
                ttl: SignedDuration::from_hours(1),
            },
            now,
        )
        .unwrap();
    let ok = verify_capability(&minted.token, &dec, &ctx, &empty).unwrap();
    assert_eq!(ok.sub, PEER_ID);
    assert_eq!(ok.aud, album_urn(ALBUM));

    // Bad signature: verify under the wrong key.
    assert_eq!(
        verify_capability(&minted.token, &other_dec, &ctx, &empty),
        Err(CapabilityReject::BadSignature)
    );

    // Missing claim: empty jti.
    let mut c = valid_claims(now);
    c.jti = String::new();
    assert_eq!(
        verify_capability(&sign(&c, &enc), &dec, &ctx, &empty),
        Err(CapabilityReject::MissingClaim("jti"))
    );

    // Wrong audience: aud names a different album.
    let mut c = valid_claims(now);
    c.aud = album_urn("some-other-album");
    assert_eq!(
        verify_capability(&sign(&c, &enc), &dec, &ctx, &empty),
        Err(CapabilityReject::AudienceMismatch)
    );

    // Wrong issuer.
    let mut c = valid_claims(now);
    c.iss = "evil.tld".to_string();
    assert_eq!(
        verify_capability(&sign(&c, &enc), &dec, &ctx, &empty),
        Err(CapabilityReject::WrongIssuer)
    );

    // Expired: exp in the past.
    let mut c = valid_claims(now);
    c.iat = now
        .checked_sub(SignedDuration::from_hours(2))
        .unwrap()
        .to_string();
    c.nbf = c.iat.clone();
    c.exp = now
        .checked_sub(SignedDuration::from_hours(1))
        .unwrap()
        .to_string();
    assert_eq!(
        verify_capability(&sign(&c, &enc), &dec, &ctx, &empty),
        Err(CapabilityReject::Expired)
    );

    // Not yet valid: nbf in the future.
    let mut c = valid_claims(now);
    c.nbf = now
        .checked_add(SignedDuration::from_hours(1))
        .unwrap()
        .to_string();
    assert_eq!(
        verify_capability(&sign(&c, &enc), &dec, &ctx, &empty),
        Err(CapabilityReject::NotYetValid)
    );

    // Expiry too far: exp more than 24 h after iat.
    let mut c = valid_claims(now);
    c.exp = now
        .checked_add(SignedDuration::from_hours(25))
        .unwrap()
        .to_string();
    assert_eq!(
        verify_capability(&sign(&c, &enc), &dec, &ctx, &empty),
        Err(CapabilityReject::ExpiryTooFar)
    );

    // Revoked: the jti is on the list.
    let c = valid_claims(now);
    let revoked = RevocationList::owned([c.jti.clone()]);
    assert_eq!(
        verify_capability(&sign(&c, &enc), &dec, &ctx, &revoked),
        Err(CapabilityReject::Revoked)
    );

    // Wrong scope is enforced per blob role (a derivative-only token cannot fetch an original).
    assert_eq!(
        authorize_blob_role(FederationScope::ReadDerivativeOnly, "original"),
        Err(CapabilityReject::ScopeInsufficient)
    );
}

// ─── Bullet 2: revocation-list fail-closed (unit) ────────────────────────────────────────────

#[test]
fn revocation_list_fails_closed_when_stale() {
    let jti = "0192f000-0000-7000-8000-000000000009";
    let fetched = now();
    let max_staleness = SignedDuration::from_mins(15);
    let list = RevocationList::cached([jti], fetched, max_staleness);

    // Within the staleness window a non-revoked jti is confirmable; the revoked one is revoked.
    let fresh = fetched.checked_add(SignedDuration::from_mins(10)).unwrap();
    assert_eq!(
        list.check("some-other-jti", fresh),
        RevocationVerdict::NotRevoked
    );
    assert_eq!(list.check(jti, fresh), RevocationVerdict::Revoked);

    // Past the 15-minute bound with no refresh, EVERY jti is unverifiable — fail closed. A token
    // whose jti we can no longer confirm is not honored on the stale list.
    let stale = fetched.checked_add(SignedDuration::from_mins(16)).unwrap();
    assert_eq!(
        list.check("some-other-jti", stale),
        RevocationVerdict::Unverifiable
    );
    assert_eq!(list.check(jti, stale), RevocationVerdict::Unverifiable);

    // A successful refresh resets the clock.
    let mut list = list;
    list.refresh(Vec::<String>::new(), stale);
    assert_eq!(
        list.check("some-other-jti", stale),
        RevocationVerdict::NotRevoked
    );

    // An owned (own-tokens) list is never stale.
    let owned = RevocationList::owned([jti]);
    let far = fetched.checked_add(SignedDuration::from_hours(48)).unwrap();
    assert_eq!(owned.check(jti, far), RevocationVerdict::Revoked);
    assert_eq!(owned.check("x", far), RevocationVerdict::NotRevoked);
}

// ─── Bullet 3: pull boundary checks (unit, invariant 20 — checks 1–18 + 25) ─────────────────

const ALLOWED_TYPES: &[&str] = &["image/jpeg", "image/heic"];

fn valid_envelope() -> PulledEnvelope {
    PulledEnvelope {
        crypto_suite_id: CRYPTO_SUITE_ID,
        protocol_version: "2026-05-31".to_string(),
        album_id: Some(ALBUM.to_string()),
        file_id: "0192f000-0000-7000-8000-0000000000aa".to_string(),
        amk_version: 1,
        ciphertext_hash: format!("{:064x}", 1),
        plaintext_size: 100,
        chunk_size: 65_520,
        key_mode: "derived".to_string(),
        metadata_blob_hash: None,
        created_by_user: "0192f000-0000-7000-8000-0000000000bb".to_string(),
        created_by_device: "0192f000-0000-7000-8000-0000000000cc".to_string(),
        client_version: "test/1".to_string(),
        timestamp: "2026-05-31T00:00:00Z".to_string(),
        action: "create".to_string(),
        prior_provenance_hash: None,
        retention_until: None,
    }
}

fn original_blob(hash: &str, format: &str, size: u64) -> FeedBlobManifest {
    FeedBlobManifest {
        original: Some(FeedBlobRef {
            ciphertext_hash: hash.to_string(),
            role: "original".to_string(),
            format: format.to_string(),
            size,
        }),
        derivatives: Vec::new(),
    }
}

fn ctx() -> PullValidationContext<'static> {
    PullValidationContext {
        protocol_min: "2026-01-01",
        protocol_max: "2026-12-31",
        album_pin: "2026-05-31",
        device_added_at: "2026-05-30T00:00:00Z",
        server_clock: "2026-05-31T00:00:00Z",
        drift_days: 30,
        stored_chain_head: None,
        stored_amk_version: None,
        allowed_content_types: ALLOWED_TYPES,
        max_blob_size: 100_000_000,
    }
}

fn revalidate(env: &PulledEnvelope, blobs: &FeedBlobManifest, ctx: &PullValidationContext<'_>) {
    let cbor = env.to_canonical_cbor().unwrap();
    revalidate_pulled(&cbor, blobs, None, ctx).unwrap();
}

fn expect_invariant(
    env: &PulledEnvelope,
    blobs: &FeedBlobManifest,
    ctx: &PullValidationContext<'_>,
    invariant: u8,
) {
    let cbor = env.to_canonical_cbor().unwrap();
    let err = revalidate_pulled(&cbor, blobs, None, ctx).unwrap_err();
    assert_eq!(
        err.invariant, invariant,
        "expected invariant {invariant}, got {err:?}"
    );
    assert!(!err.code.is_empty(), "rejection carries an error.* code");
}

#[test]
fn pull_boundary_accepts_valid_and_rejects_each_invariant() {
    let good_hash = format!("{:064x}", 2);
    let blobs = original_blob(&good_hash, "image/jpeg", 4096);

    // Baseline: a valid create passes the whole battery.
    revalidate(&valid_envelope(), &blobs, &ctx());

    // Invariant 1: protocol outside the accepted window.
    let mut e = valid_envelope();
    e.protocol_version = "2099-01-01".to_string();
    // Keep album pin equal so this is a clean invariant-1 (not invariant-6) rejection.
    let mut c = ctx();
    c.album_pin = "2099-01-01";
    expect_invariant(&e, &blobs, &c, 1);

    // Invariant 2: unknown crypto suite.
    let mut e = valid_envelope();
    e.crypto_suite_id = 0x9999;
    expect_invariant(&e, &blobs, &ctx(), 2);

    // Invariant 3: blob content hash not 64-hex.
    let bad = original_blob("not-a-hash", "image/jpeg", 4096);
    expect_invariant(&valid_envelope(), &bad, &ctx(), 3);

    // Invariant 4: blob size outside (0, max].
    let big = original_blob(&good_hash, "image/jpeg", 200_000_000);
    expect_invariant(&valid_envelope(), &big, &ctx(), 4);

    // Invariant 5: content type not in the closed enum.
    let evil = original_blob(&good_hash, "application/x-evil", 4096);
    expect_invariant(&valid_envelope(), &evil, &ctx(), 5);

    // Invariant 6: album pin mismatch (protocol still in window).
    let mut c = ctx();
    c.album_pin = "2026-06-01";
    expect_invariant(&valid_envelope(), &blobs, &c, 6);

    // Invariant 7: producing device added after the manifest timestamp.
    let mut c = ctx();
    c.device_added_at = "2027-01-01T00:00:00Z";
    expect_invariant(&valid_envelope(), &blobs, &c, 7);

    // Invariant 8: timestamp beyond the drift bound (device added before it, so 7 passes first).
    let mut e = valid_envelope();
    e.timestamp = "2020-01-01T00:00:00Z".to_string();
    let mut c = ctx();
    c.device_added_at = "2019-01-01T00:00:00Z";
    expect_invariant(&e, &blobs, &c, 8);

    // Invariant 17: stale chain — a metadata-update whose prior != the stored head.
    let mut e = valid_envelope();
    e.action = "metadata-update".to_string();
    e.prior_provenance_hash = Some(format!("{:064x}", 0x11));
    let mut c = ctx();
    c.stored_chain_head = Some(capsule_core::crypto::hash::Hash32([0x22; 32]));
    c.stored_amk_version = Some(1);
    expect_invariant(&e, &blobs, &c, 17);

    // Invariant 18: amk regression (prior matches the stored head so 17 passes first).
    let head = capsule_core::crypto::hash::Hash32([0x33; 32]);
    let mut e = valid_envelope();
    e.action = "metadata-update".to_string();
    e.prior_provenance_hash = Some(hex(&head));
    e.amk_version = 1;
    let mut c = ctx();
    c.stored_chain_head = Some(head);
    c.stored_amk_version = Some(5);
    expect_invariant(&e, &blobs, &c, 18);

    // Invariant 25: bundled metadata blob whose hash != the committed metadata_blob_hash.
    let blob = seal_blob(&[0x44; 32], b"canonical sidecar");
    let committed = blob_ciphertext_hash(&blob);
    let mut e = valid_envelope();
    e.metadata_blob_hash = Some(hex(&committed));
    let cbor = e.to_canonical_cbor().unwrap();
    // Correct blob accepts.
    revalidate_pulled(&cbor, &blobs, Some(&blob), &ctx()).unwrap();
    // A one-byte-tampered blob is refused at invariant 25.
    let mut tampered = blob.clone();
    tampered[0] ^= 0x01;
    let err = revalidate_pulled(&cbor, &blobs, Some(&tampered), &ctx()).unwrap_err();
    assert_eq!(err.invariant, 25);
    assert_eq!(err.code, error_codes::UPLOAD_ENVELOPE_MISMATCH);
}

fn hex(h: &capsule_core::crypto::hash::Hash32) -> String {
    h.0.iter().map(|b| format!("{b:02x}")).collect()
}

// ─── Bullet 6: soft-fail bounded memory (unit) ──────────────────────────────────────────────

#[test]
fn soft_fail_table_is_bounded_lru() {
    let n = now();
    let mut table = RejectedHashTable::new(3, SignedDuration::from_hours(24 * 90));

    for i in 0..3 {
        table.remember(&format!("{i:064x}"), n);
    }
    assert_eq!(table.len(), 3);

    // Reference hash 0 so it becomes most-recently-referenced (no longer the LRU victim).
    assert!(table.contains(&format!("{:064x}", 0), n));

    // Insert two more past the cap: eviction is LRU-by-last-reference, so hashes 1 and 2 age out,
    // hash 0 (recently referenced) survives. Length never exceeds the cap.
    table.remember(&format!("{:064x}", 3), n);
    table.remember(&format!("{:064x}", 4), n);
    assert_eq!(table.len(), 3, "bounded — no unbounded growth");
    assert!(
        table.contains(&format!("{:064x}", 0), n),
        "recently-referenced entry retained"
    );
    assert!(
        !table.contains(&format!("{:064x}", 1), n),
        "LRU entry evicted"
    );
    assert!(
        table.contains(&format!("{:064x}", 4), n),
        "newest entry present"
    );

    // TTL eviction: an entry older than the TTL is pruned.
    let mut ttl_table = RejectedHashTable::new(10, SignedDuration::from_hours(1));
    ttl_table.remember(&format!("{:064x}", 7), n);
    let later = n.checked_add(SignedDuration::from_hours(2)).unwrap();
    assert!(
        !ttl_table.contains(&format!("{:064x}", 7), later),
        "expired entry pruned"
    );
    assert_eq!(ttl_table.len(), 0);
}

// ─── Bullets 4 & 5: rate budget + circuit breaker (unit, invariant 21) ──────────────────────

fn tight_limits() -> PeerLimits {
    PeerLimits {
        events_per_hour: 10,
        bytes_per_hour: 10_000,
        cpu_ms_per_hour: 1_000,
        probation_events_per_hour: 3,
        probation_bytes_per_hour: 3_000,
        probation_cpu_ms_per_hour: 300,
        error_budget: 2,
        probation_period: SignedDuration::from_hours(24),
        breaker_backoffs: vec![SignedDuration::from_mins(5), SignedDuration::from_mins(30)],
    }
}

#[test]
fn rate_budget_enforced_per_peer() {
    let reg = PeerRegistry::new(tight_limits());
    let n = now();

    // A registered peer starts established (10 events/h). The 11th event breaks the budget.
    for _ in 0..10 {
        reg.try_consume(
            PEER_ID,
            true,
            PullCost {
                events: 1,
                ..Default::default()
            },
            n,
        )
        .unwrap();
    }
    assert_eq!(
        reg.try_consume(
            PEER_ID,
            true,
            PullCost {
                events: 1,
                ..Default::default()
            },
            n
        ),
        Err(CompartmentReject::EventBudgetExceeded)
    );

    // After the 1-hour window rolls, pulls resume.
    let next_window = n.checked_add(SignedDuration::from_hours(1)).unwrap();
    reg.try_consume(
        PEER_ID,
        true,
        PullCost {
            events: 1,
            ..Default::default()
        },
        next_window,
    )
    .unwrap();

    // A first-contact (unregistered) peer is quarantined to the tighter probation budget.
    let reg2 = PeerRegistry::new(tight_limits());
    assert_eq!(reg2.tier("newbie.tld"), None);
    for _ in 0..3 {
        reg2.try_consume(
            "newbie.tld",
            false,
            PullCost {
                events: 1,
                ..Default::default()
            },
            n,
        )
        .unwrap();
    }
    assert_eq!(reg2.tier("newbie.tld"), Some(PeerTier::Probation));
    assert_eq!(
        reg2.try_consume(
            "newbie.tld",
            false,
            PullCost {
                events: 1,
                ..Default::default()
            },
            n
        ),
        Err(CompartmentReject::EventBudgetExceeded),
        "probation budget is tighter than established"
    );
}

#[test]
fn circuit_breaker_trips_and_recovers() {
    let reg = PeerRegistry::new(tight_limits());
    let n = now();

    // Error budget is 2 malformed inputs; the 3rd trips the breaker.
    assert!(reg.record_error(PEER_ID, n).is_none());
    assert!(reg.record_error(PEER_ID, n).is_none());
    let until = reg
        .record_error(PEER_ID, n)
        .expect("breaker trips on the 3rd error");
    assert!(reg.is_circuit_open(PEER_ID, n));

    // While open, further pulls are short-circuited.
    match reg.try_consume(
        PEER_ID,
        true,
        PullCost {
            events: 1,
            ..Default::default()
        },
        n,
    ) {
        Err(CompartmentReject::CircuitOpen { until: u }) => assert_eq!(u, until),
        other => panic!("expected CircuitOpen, got {other:?}"),
    }

    // After the back-off elapses, the breaker half-opens and pulls resume.
    let after = until.checked_add(SignedDuration::from_secs(1)).unwrap();
    assert!(!reg.is_circuit_open(PEER_ID, after));
    reg.try_consume(
        PEER_ID,
        true,
        PullCost {
            events: 1,
            ..Default::default()
        },
        after,
    )
    .unwrap();
}

#[test]
fn probation_graduates_after_clean_period() {
    let reg = PeerRegistry::new(tight_limits());
    let n = now();
    // First contact → probation.
    reg.try_consume(
        "fresh.tld",
        false,
        PullCost {
            events: 1,
            ..Default::default()
        },
        n,
    )
    .unwrap();
    assert_eq!(reg.tier("fresh.tld"), Some(PeerTier::Probation));

    // After the clean probation period, the next admitted pull graduates it to established.
    let later = n.checked_add(SignedDuration::from_hours(25)).unwrap();
    reg.try_consume(
        "fresh.tld",
        false,
        PullCost {
            events: 1,
            ..Default::default()
        },
        later,
    )
    .unwrap();
    assert_eq!(reg.tier("fresh.tld"), Some(PeerTier::Established));
}

// ─── Invariants 19 & 21: rejection → HTTP status + error.* code ─────────────────────────────

#[test]
fn federation_reject_maps_to_status_and_code() {
    // Invariant 19 — capability rejections: 401 (invalid/expired) or 403 (revoked/audience/scope).
    let cases: &[(CapabilityReject, u16, &str)] = &[
        (
            CapabilityReject::BadSignature,
            401,
            error_codes::FEDERATION_CAPABILITY_INVALID,
        ),
        (
            CapabilityReject::MissingClaim("jti"),
            401,
            error_codes::FEDERATION_CAPABILITY_INVALID,
        ),
        (
            CapabilityReject::WrongIssuer,
            401,
            error_codes::FEDERATION_CAPABILITY_INVALID,
        ),
        (
            CapabilityReject::NotYetValid,
            401,
            error_codes::FEDERATION_CAPABILITY_INVALID,
        ),
        (
            CapabilityReject::ExpiryTooFar,
            401,
            error_codes::FEDERATION_CAPABILITY_INVALID,
        ),
        (
            CapabilityReject::Expired,
            401,
            error_codes::FEDERATION_CAPABILITY_EXPIRED,
        ),
        (
            CapabilityReject::Revoked,
            403,
            error_codes::FEDERATION_CAPABILITY_REVOKED,
        ),
        (
            CapabilityReject::RevocationUnverifiable,
            403,
            error_codes::FEDERATION_CAPABILITY_REVOKED,
        ),
        (
            CapabilityReject::AudienceMismatch,
            403,
            error_codes::FEDERATION_AUDIENCE_MISMATCH,
        ),
        (
            CapabilityReject::ScopeInsufficient,
            403,
            error_codes::FEDERATION_SCOPE_INSUFFICIENT,
        ),
    ];
    for (reject, status, code) in cases {
        let fr = FederationReject::from(reject.clone());
        assert_eq!(fr.http_status(), *status, "status for {reject:?}");
        assert_eq!(fr.code(), *code, "code for {reject:?}");
    }

    // Invariant 21 — per-peer budget & breaker: 429.
    let budget = FederationReject::from(CompartmentReject::EventBudgetExceeded);
    assert_eq!(budget.http_status(), 429);
    assert_eq!(budget.code(), error_codes::FEDERATION_RATE_BUDGET_EXCEEDED);
    let breaker = FederationReject::from(CompartmentReject::CircuitOpen { until: now() });
    assert_eq!(breaker.http_status(), 429);
    assert_eq!(breaker.code(), error_codes::FEDERATION_CIRCUIT_OPEN);
}

// ─── Invariant 19: scope enforced by blob role ──────────────────────────────────────────────

#[test]
fn scope_enforced_by_blob_role() {
    // Full read covers every role.
    for role in ["original", "derivative", "metadata", "provenance", "backup"] {
        assert!(FederationScope::Read.permits_role(role));
        assert!(authorize_blob_role(FederationScope::Read, role).is_ok());
    }
    // Derivative-only covers derivatives but never originals.
    assert!(FederationScope::ReadDerivativeOnly.permits_role("derivative"));
    assert!(!FederationScope::ReadDerivativeOnly.permits_role("original"));
    assert_eq!(
        authorize_blob_role(FederationScope::ReadDerivativeOnly, "original"),
        Err(CapabilityReject::ScopeInsufficient)
    );
}
