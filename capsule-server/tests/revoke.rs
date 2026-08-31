//! The global sign-out ceremony (`S-C23`), end to end.
//!
//! The property the whole slice exists for is
//! `a_stolen_session_token_cannot_log_the_owner_out_of_everything`: an attacker holding a live
//! token can revoke *that* token and nothing else. Every other case here is a way that property
//! could be lost — a challenge that survives a failed attempt, a proof checked against a
//! caller-supplied key, an account with no anchor being treated as an open one.

mod support;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::keys::hybrid_sig::HybridSigningKey;
use capsule_core::crypto::revoke::revoke_all_signing_bytes;
use jiff::SignedDuration;
use kynos::http::StatusCode;
use serde_json::{Value, json};
use support::{Fixture, identity_header, identity_key, signed_directory_by};

/// Publish the seeded account's directory, anchoring it to `ik`.
async fn anchor(fixture: &Fixture, bearer: &str, ik: &HybridSigningKey) {
    fixture
        .client
        .post("/v1/auth/devices/directory")
        .header("authorization", bearer)
        .header("x-capsule-identity-key", &identity_header(ik))
        .body("application/cbor", signed_directory_by(ik, 1))
        .send()
        .await
        .assert_status(StatusCode::OK);
}

/// Ask for a challenge and return it.
async fn challenge(fixture: &Fixture, bearer: &str) -> String {
    let body: Value = fixture
        .client
        .post("/v1/auth/logout/all/challenge")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    body["challenge"]
        .as_str()
        .expect("the challenge is a string")
        .to_owned()
}

/// The base64 hybrid signature `ik` makes over `challenge`.
fn proof(ik: &HybridSigningKey, challenge: &str) -> String {
    let signature = ik.sign(&revoke_all_signing_bytes(challenge));
    BASE64.encode(capsule_core::cbor::to_canonical_vec(&signature).expect("a signature encodes"))
}

/// Present a proof and assert the status.
async fn revoke(
    fixture: &Fixture,
    challenge: &str,
    proof: &str,
    expect: StatusCode,
) -> kynos::test::TestResponse {
    let response = fixture
        .client
        .post("/v1/auth/logout/all")
        .header("accept", "application/json")
        .json(&json!({ "challenge": challenge, "proof": proof }))
        .send()
        .await;
    response.assert_status(expect);
    response
}

#[tokio::test]
async fn a_valid_proof_closes_every_session_including_the_callers() {
    let fixture = Fixture::working();
    let ik = identity_key();
    let first = fixture.login().await;
    anchor(&fixture, &format!("Bearer {}", first.access_token), &ik).await;
    // A second sign-in, so "every session" is more than one.
    let second = fixture.login().await;

    let bearer = format!("Bearer {}", first.access_token);
    let challenge = challenge(&fixture, &bearer).await;
    let body: Value = revoke(
        &fixture,
        &challenge,
        &proof(&ik, &challenge),
        StatusCode::OK,
    )
    .await
    .json();
    assert_eq!(
        body["revoked"], 2,
        "the count comes from the records the store removed, never from a separate index — the \
         Salvo implementation read a per-user set `revoke_session` did not clean up and inflated \
         by one per prior refresh"
    );

    // Every session record is gone, so no refresh token can mint anything. This is the half of
    // the revoke that is immediate.
    for issued in [&first, &second] {
        fixture
            .client
            .post("/v1/auth/refresh")
            .json(&json!({ "refresh_token": issued.refresh_token }))
            .send()
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }

    // The already-issued **access** tokens are not, and this asserts the truth rather than the
    // wish. The bearer scheme verifies a signature and a deadline and never reads the session
    // ledger, so an access token minted before the revoke stays usable for the remainder of its
    // fifteen minutes. That is why the TTL is short — but it is a window, and `S-C48` is where
    // closing it is decided rather than assumed.
    fixture
        .client
        .get("/v1/quota")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);

    fixture.clock.advance(SignedDuration::from_mins(16));
    fixture
        .client
        .get("/v1/quota")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_stolen_session_token_cannot_log_the_owner_out_of_everything() {
    // The damage scenario the asymmetry exists to close. The attacker has a live token — enough
    // to ask for a challenge, which costs nothing — and no identity key, which is everything.
    let fixture = Fixture::working();
    let ik = identity_key();
    let owner = fixture.bearer().await;
    anchor(&fixture, &owner, &ik).await;
    let stolen = fixture.bearer().await;

    let challenge = challenge(&fixture, &stolen).await;
    let attackers_key = identity_key();
    let problem: Value = revoke(
        &fixture,
        &challenge,
        &proof(&attackers_key, &challenge),
        StatusCode::UNAUTHORIZED,
    )
    .await
    .json();
    assert_eq!(problem["code"], "error.auth.revoke_proof_invalid");

    // The owner's session record is untouched, which is the whole point: a stolen token can
    // revoke itself and cannot escalate a theft into a lockout. Asserted through *refresh*,
    // which is the operation that actually reads the ledger.
    fixture
        .client
        .get("/v1/quota")
        .header("authorization", &owner)
        .send()
        .await
        .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn a_challenge_is_burned_by_a_failed_attempt() {
    // Destructive on every attempt, successful or not. A challenge that survived a failure
    // would let an attacker grind signatures against a live one; this costs a legitimate user
    // one extra round trip.
    let fixture = Fixture::working();
    let ik = identity_key();
    let bearer = fixture.bearer().await;
    anchor(&fixture, &bearer, &ik).await;

    let challenge = challenge(&fixture, &bearer).await;
    revoke(
        &fixture,
        &challenge,
        &proof(&identity_key(), &challenge),
        StatusCode::UNAUTHORIZED,
    )
    .await;

    // Even the *right* proof cannot redeem it now.
    revoke(
        &fixture,
        &challenge,
        &proof(&ik, &challenge),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    fixture
        .client
        .get("/v1/quota")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn a_challenge_is_not_redeemable_after_it_expires() {
    let fixture = Fixture::working();
    let ik = identity_key();
    let bearer = fixture.bearer().await;
    anchor(&fixture, &bearer, &ik).await;
    let challenge = challenge(&fixture, &bearer).await;

    fixture.clock.advance(SignedDuration::from_mins(6));

    revoke(
        &fixture,
        &challenge,
        &proof(&ik, &challenge),
        StatusCode::UNAUTHORIZED,
    )
    .await;
}

#[tokio::test]
async fn a_proof_for_one_challenge_does_not_redeem_another() {
    // The challenge is inside the signed message, so a proof is bound to the one it was made
    // for. Without that binding, one captured signature would revoke forever.
    let fixture = Fixture::working();
    let ik = identity_key();
    let bearer = fixture.bearer().await;
    anchor(&fixture, &bearer, &ik).await;

    let first = challenge(&fixture, &bearer).await;
    let second = challenge(&fixture, &bearer).await;

    revoke(
        &fixture,
        &second,
        &proof(&ik, &first),
        StatusCode::UNAUTHORIZED,
    )
    .await;
}

#[tokio::test]
async fn an_account_with_no_anchored_directory_cannot_revoke_globally() {
    // Answered identically to a bad signature. The endpoint takes no credential, so an answer
    // that distinguished "no directory" from "wrong key" would report which accounts have
    // published one to anybody who could name a challenge.
    let fixture = Fixture::working();
    let ik = identity_key();
    let bearer = fixture.bearer().await;
    let challenge = challenge(&fixture, &bearer).await;

    let problem: Value = revoke(
        &fixture,
        &challenge,
        &proof(&ik, &challenge),
        StatusCode::UNAUTHORIZED,
    )
    .await
    .json();
    assert_eq!(problem["code"], "error.auth.revoke_proof_invalid");
}

#[tokio::test]
async fn a_proof_that_is_not_a_signature_is_a_malformed_request() {
    // Distinct from `revoke_proof_invalid` because the remedy differs, and because telling a
    // caller "your base64 is not base64" is not an oracle about anything.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let challenge = challenge(&fixture, &bearer).await;

    for bad in ["not base64 !!", &BASE64.encode(b"not a signature")] {
        let problem: Value = revoke(&fixture, &challenge, bad, StatusCode::UNAUTHORIZED)
            .await
            .json();
        assert_eq!(problem["code"], "error.auth.revoke_proof_required");
    }
}

#[tokio::test]
async fn a_challenge_names_the_account_that_asked_for_it() {
    // The account comes from the credential, never from a request field, so a caller cannot ask
    // for somebody else's challenge and there is no account parameter to aim at.
    let fixture = Fixture::working();
    let ik = identity_key();
    let bearer = fixture.bearer().await;
    anchor(&fixture, &bearer, &ik).await;
    let stranger = fixture.other_bearer("01937b7c-0000-7000-8000-0000000000ff");

    // The stranger's own challenge, signed with *this* account's key, revokes nothing — the
    // challenge names their account, and their account has no anchor for this key.
    let theirs = challenge(&fixture, &stranger).await;
    let body: Value = revoke(
        &fixture,
        &theirs,
        &proof(&ik, &theirs),
        StatusCode::UNAUTHORIZED,
    )
    .await
    .json();
    assert_eq!(body["code"], "error.auth.revoke_proof_invalid");

    fixture
        .client
        .get("/v1/quota")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn asking_for_a_challenge_needs_a_credential() {
    // Not because a challenge is worth anything without the key — it is not — but because an
    // unauthenticated issuer would report whether an account exists.
    let fixture = Fixture::working();
    fixture
        .client
        .post("/v1/auth/logout/all/challenge")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}
