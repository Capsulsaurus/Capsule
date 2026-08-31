//! The device-directory surface (slice `S-C9`), end to end.
//!
//! Two properties carry the whole slice: the bytes come back exactly as they went in, and a
//! version that does not strictly advance changes nothing. The first is what makes a signature
//! still verify after a round trip; the second is what stops a server rolling a directory back
//! to un-revoke a device somebody deliberately removed.

mod support;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::keys::hybrid_sig::HybridSigningKey;
use capsule_core::crypto::keys::{DeviceDirectory, DirectoryCore};
use kynos::http::StatusCode;
use serde_json::Value;
use support::{Fixture, user};
use uuid::Uuid;

/// An account's identity key.
///
/// Held by the caller rather than generated inside `signed`, because the server anchors an
/// account to the first key it sees (`S-C42`) and a fresh key per publish would make every
/// second one a `403`.
fn ik() -> HybridSigningKey {
    HybridSigningKey::generate()
}

/// The `X-Capsule-Identity-Key` header value for `ik`.
fn anchor(ik: &HybridSigningKey) -> String {
    BASE64.encode(ik.verifying_key().to_bytes())
}

/// The signed CBOR of a directory for `user` at `version`, signed by `ik`.
fn signed_by(ik: &HybridSigningKey, user: Uuid, version: u64) -> Vec<u8> {
    let directory: DeviceDirectory = DirectoryCore {
        user_id: user,
        directory_version: version,
        updated_at: "2026-01-01T00:00:00Z".to_owned(),
        devices: Vec::new(),
    }
    .sign(ik);
    capsule_core::cbor::to_canonical_vec(&directory).expect("a directory serializes")
}

/// The seeded account's own id, as a UUID.
fn account() -> Uuid {
    Uuid::parse_str(user().as_str()).expect("the seeded account id is a uuid")
}

/// The bearer for the seeded account.
async fn bearer(fixture: &Fixture) -> String {
    format!("Bearer {}", fixture.login().await.access_token)
}

/// Publish `document`, asserting the status, and return the body.
async fn publish(
    fixture: &Fixture,
    bearer: &str,
    ik: &HybridSigningKey,
    document: Vec<u8>,
    expect: StatusCode,
) -> kynos::test::TestResponse {
    let response = fixture
        .client
        .post("/v1/auth/devices/directory")
        .header("authorization", bearer)
        .header("x-capsule-identity-key", &anchor(ik))
        .body("application/cbor", document)
        .send()
        .await;
    response.assert_status(expect);
    response
}

// ===========================================================================================

/// The bytes a client signed are the bytes every reader gets back.
#[tokio::test]
async fn a_published_directory_is_served_back_byte_for_byte() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let ik = ik();
    let document = signed_by(&ik, account(), 1);

    let accepted: Value = publish(&fixture, &bearer, &ik, document.clone(), StatusCode::OK)
        .await
        .json();
    assert_eq!(accepted["directory_version"], 1);

    let fetched = fixture
        .client
        .get(&format!("/v1/auth/devices/directory/{}", user()))
        .header("authorization", &bearer)
        .send()
        .await;
    fetched.assert_status(StatusCode::OK);
    assert_eq!(
        fetched.header("content-type"),
        Some("application/cbor"),
        "the media type is what lets a generated client decode without guessing"
    );
    assert_eq!(
        fetched.bytes().as_ref(),
        document.as_slice(),
        "a re-encoded directory is a directory whose signature no longer verifies, and the \
         failure would look like the publisher's bug"
    );
}

/// Invariant 23: a version that does not strictly advance is refused and changes nothing.
#[tokio::test]
async fn a_non_advancing_version_is_refused_and_leaves_the_stored_document_alone() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let ik = ik();
    let held = signed_by(&ik, account(), 5);
    publish(&fixture, &bearer, &ik, held.clone(), StatusCode::OK).await;

    // Equal is refused too. A republished version could carry a *different* device list under
    // the same number, which is the rollback wearing the right version.
    for version in [5_u64, 4, 1] {
        let problem: Value = publish(
            &fixture,
            &bearer,
            &ik,
            signed_by(&ik, account(), version),
            StatusCode::CONFLICT,
        )
        .await
        .json();
        assert_eq!(problem["code"], "error.directory.version_conflict");
        assert_eq!(problem["stored"], 5);
        assert_eq!(
            problem["submitted"], version,
            "the client is told both numbers, which is the difference between re-reading and \
             retrying the same losing document forever"
        );
    }

    let fetched = fixture
        .client
        .get(&format!("/v1/auth/devices/directory/{}", user()))
        .header("authorization", &bearer)
        .send()
        .await;
    assert_eq!(
        fetched.bytes().as_ref(),
        held.as_slice(),
        "a refused publish must not have replaced the document a revocation is recorded in"
    );
}

/// A strictly advancing version replaces it.
#[tokio::test]
async fn an_advancing_version_replaces_the_stored_document() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let ik = ik();
    publish(
        &fixture,
        &bearer,
        &ik,
        signed_by(&ik, account(), 1),
        StatusCode::OK,
    )
    .await;
    let newer = signed_by(&ik, account(), 2);
    let accepted: Value = publish(&fixture, &bearer, &ik, newer.clone(), StatusCode::OK)
        .await
        .json();
    assert_eq!(accepted["directory_version"], 2);

    let fetched = fixture
        .client
        .get(&format!("/v1/auth/devices/directory/{}", user()))
        .header("authorization", &bearer)
        .send()
        .await;
    assert_eq!(fetched.bytes().as_ref(), newer.as_slice());
}

/// A document signed for another account cannot be published under this one.
#[tokio::test]
async fn a_document_signed_for_another_account_is_refused() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let somebody_else = Uuid::parse_str("01937b7c-0000-7000-8000-0000000000ff").expect("a uuid");

    let ik = ik();
    let problem: Value = publish(
        &fixture,
        &bearer,
        &ik,
        signed_by(&ik, somebody_else, 1),
        StatusCode::BAD_REQUEST,
    )
    .await
    .json();
    assert_eq!(problem["code"], "error.directory.malformed");
    assert!(
        problem["detail"]
            .as_str()
            .expect("a detail")
            .contains("different account"),
        "the account is decided by the signed core, not by the token alone"
    );
}

/// Bytes that are not a directory are refused before anything is stored.
#[tokio::test]
async fn a_body_that_is_not_a_directory_is_refused() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;

    let problem: Value = publish(
        &fixture,
        &bearer,
        &ik(),
        b"not a directory".to_vec(),
        StatusCode::BAD_REQUEST,
    )
    .await
    .json();
    assert_eq!(problem["code"], "error.directory.malformed");

    fixture
        .client
        .get(&format!("/v1/auth/devices/directory/{}", user()))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

/// A body in the wrong media type is refused by the extractor, not by the handler.
#[tokio::test]
async fn a_body_in_the_wrong_media_type_is_refused() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;

    fixture
        .client
        .post("/v1/auth/devices/directory")
        .header("authorization", &bearer)
        .header("x-capsule-identity-key", &anchor(&ik()))
        .body("application/json", "{}")
        .send()
        .await
        .assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

/// An account that has never published is a coded `404`, not an empty document.
#[tokio::test]
async fn an_unpublished_account_is_not_found() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;

    let response = fixture
        .client
        .get("/v1/auth/devices/directory/01937b7c-0000-7000-8000-0000000000ff")
        .header("authorization", &bearer)
        .send()
        .await;
    response.assert_status(StatusCode::NOT_FOUND);
    let problem: Value = response.json();
    assert_eq!(problem["code"], "error.directory.not_published");
}

/// Anyone authenticated may read anyone's directory — that is what a directory is for.
#[tokio::test]
async fn any_authenticated_caller_may_fetch_any_directory() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let ik = ik();
    let document = signed_by(&ik, account(), 1);
    publish(&fixture, &bearer, &ik, document.clone(), StatusCode::OK).await;

    // A second sign-in stands in for a second party: the fixture seeds one account, and what
    // is being asserted is that the *fetch* is not owner-scoped, not that two accounts exist.
    let other_session = format!("Bearer {}", fixture.login().await.access_token);
    let fetched = fixture
        .client
        .get(&format!("/v1/auth/devices/directory/{}", user()))
        .header("authorization", &other_session)
        .send()
        .await;
    fetched.assert_status(StatusCode::OK);
    assert_eq!(fetched.bytes().as_ref(), document.as_slice());
}

/// Both operations need a credential.
#[tokio::test]
async fn the_directory_surface_requires_a_credential() {
    let fixture = Fixture::working();

    fixture
        .client
        .post("/v1/auth/devices/directory")
        .header("x-capsule-identity-key", &anchor(&ik()))
        .body("application/cbor", signed_by(&ik(), account(), 1))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    fixture
        .client
        .get(&format!("/v1/auth/devices/directory/{}", user()))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

/// The account is anchored to the first identity key it publishes under (`S-C42`).
///
/// This is invariant 23's second clause reaching the wire. Before it, an authenticated caller
/// could publish a document that verifies under no key, and `S-C23`'s revoke-all — which
/// accepts a candidate IK only if it verifies the account's stored directory — would have been
/// permanently disabled for that account by a stolen session token. A stolen token cannot
/// revoke everything; it must not be able to make sure nobody can.
#[tokio::test]
async fn a_second_identity_key_cannot_take_over_an_anchored_account() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let anchored = ik();
    let attacker = ik();

    publish(
        &fixture,
        &bearer,
        &anchored,
        signed_by(&anchored, account(), 1),
        StatusCode::OK,
    )
    .await;

    let problem: Value = publish(
        &fixture,
        &bearer,
        &attacker,
        signed_by(&attacker, account(), 2),
        StatusCode::FORBIDDEN,
    )
    .await
    .json();
    assert_eq!(problem["code"], "error.directory.identity_mismatch");
    assert!(
        !problem.to_string().contains(&anchor(&anchored)),
        "the refusal must not echo the stored anchor: a refusal that answers a question the \
         caller did not ask is a refusal that will be used for it"
    );

    // The anchored key still works, so this is a lockout of the impostor and not of the account.
    publish(
        &fixture,
        &bearer,
        &anchored,
        signed_by(&anchored, account(), 2),
        StatusCode::OK,
    )
    .await;
}

/// A document that does not verify under the key it was published with is refused.
#[tokio::test]
async fn a_document_that_does_not_verify_under_its_declared_key_is_refused() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let declared = ik();
    let actual = ik();

    let problem: Value = publish(
        &fixture,
        &bearer,
        &declared,
        signed_by(&actual, account(), 1),
        StatusCode::BAD_REQUEST,
    )
    .await
    .json();
    assert_eq!(problem["code"], "error.directory.malformed");

    fixture
        .client
        .get(&format!("/v1/auth/devices/directory/{}", user()))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

/// A publish with no identity key is refused rather than stored unverified.
#[tokio::test]
async fn a_publish_without_an_identity_key_is_refused() {
    // The header is required, not optional-with-a-fallback. An optional anchor would mean the
    // unverified path still exists and every client that omits the header takes it.
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;

    let response = fixture
        .client
        .post("/v1/auth/devices/directory")
        .header("authorization", &bearer)
        .body("application/cbor", signed_by(&ik(), account(), 1))
        .send()
        .await;
    response.assert_status(StatusCode::BAD_REQUEST);
    let problem: Value = response.json();
    assert_eq!(problem["code"], "error.directory.malformed");
}

/// A malformed identity key is refused before the document is decoded.
#[tokio::test]
async fn an_identity_key_that_is_not_base64_is_refused() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;

    let response = fixture
        .client
        .post("/v1/auth/devices/directory")
        .header("authorization", &bearer)
        .header("x-capsule-identity-key", "not base64 at all!!")
        .body("application/cbor", signed_by(&ik(), account(), 1))
        .send()
        .await;
    response.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        response.json::<Value>()["code"],
        "error.directory.malformed"
    );
}
