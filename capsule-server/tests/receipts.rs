//! `GET /v1/upload/{id}/receipt` — custody receipts (slice `S-C15`), end to end.
//!
//! The case that carries the slice is `a_finalized_upload_yields_a_verifiable_receipt`: it
//! verifies the fetched bytes the way a **client** does — through `capsule_core`'s own
//! `verify_receipt`, under the key the fixture holds — because a receipt the client cannot
//! verify is worse than no receipt at all. It is the server claiming accountability it does not
//! actually have.

mod support;

use base64::Engine as _;
use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::receipts::{
    BlobRole, CustodyReceipt, ReceiptExpectations, ReceiptRejection, verify_receipt,
};
use kynos::http::StatusCode;
use serde_json::Value;
use support::{Fixture, PROTOCOL_VERSION, checksum, payload};

/// Two 4 KiB chunks and the whole they make.
fn blob() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let first = payload(b'a', 4096);
    let second = payload(b'b', 4096);
    let whole: Vec<u8> = first.iter().chain(second.iter()).copied().collect();
    (first, second, whole)
}

/// The bearer for the seeded account.
async fn token(fixture: &Fixture) -> String {
    format!("Bearer {}", fixture.login().await.access_token)
}

/// Fetch the receipt for `id`, asserting the status.
async fn fetch(
    fixture: &Fixture,
    bearer: &str,
    id: &str,
    expect: StatusCode,
) -> kynos::test::TestResponse {
    let response = fixture
        .client
        .get(&format!("/v1/upload/{id}/receipt"))
        .header("authorization", bearer)
        .send()
        .await;
    response.assert_status(expect);
    response
}

/// Upload `bytes` to completion and return the session id.
async fn upload(
    fixture: &Fixture,
    bearer: &str,
    first: &[u8],
    second: &[u8],
    whole: &[u8],
) -> String {
    let id = fixture.open_session(whole, "original", bearer).await;
    fixture
        .chunk(&id, 0, first, bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    fixture
        .chunk(&id, 4096, second, bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    id
}

// ===========================================================================================

/// The whole point: a client can verify what it fetched.
#[tokio::test]
async fn a_finalized_upload_yields_a_verifiable_receipt() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    let (first, second, whole) = blob();
    let id = upload(&fixture, &bearer, &first, &second, &whole).await;

    let response = fetch(&fixture, &bearer, &id, StatusCode::OK).await;
    assert_eq!(
        response.header("content-type"),
        Some("application/cbor"),
        "the signature covers the canonical encoding, so the wire carries what the log holds"
    );

    let receipt = CustodyReceipt::from_canonical_cbor(response.bytes())
        .expect("the served bytes decode as a receipt");

    // Verified the way a client does: core's own predicate, under the published key.
    assert_eq!(
        verify_receipt(
            &receipt,
            &[fixture.attestation_key.verifying_key()],
            &ReceiptExpectations {
                ciphertext_hash: hash_bytes(&whole),
                size: whole.len() as u64,
                role: BlobRole::Original,
                envelope_hash: receipt.core.envelope_hash,
            },
            receipt
                .core
                .received_at
                .parse::<jiff::Timestamp>()
                .expect("an instant")
                .as_second(),
        ),
        Ok(()),
    );
    assert_eq!(receipt.core.upload_id, id);
    assert_eq!(receipt.core.receipt_seq, 1);
    assert_eq!(receipt.core.protocol_version, PROTOCOL_VERSION);
}

/// The receipt attests to what the **server** recomputed, not to what the client declared.
#[tokio::test]
async fn the_receipt_names_the_bytes_the_server_hashed() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    let (first, second, whole) = blob();
    let id = upload(&fixture, &bearer, &first, &second, &whole).await;

    let receipt = CustodyReceipt::from_canonical_cbor(
        fetch(&fixture, &bearer, &id, StatusCode::OK).await.bytes(),
    )
    .expect("a receipt");

    assert_eq!(receipt.core.ciphertext_hash.to_hex(), checksum(&whole));
    assert_eq!(receipt.core.size, whole.len() as u64);
    assert_eq!(receipt.core.blob_role, "original");
    assert_eq!(
        verify_receipt(
            &receipt,
            &[fixture.attestation_key.verifying_key()],
            &ReceiptExpectations {
                // A client that uploaded *different* bytes cannot match this receipt.
                ciphertext_hash: hash_bytes(b"bytes nobody uploaded"),
                size: whole.len() as u64,
                role: BlobRole::Original,
                envelope_hash: receipt.core.envelope_hash,
            },
            receipt
                .core
                .received_at
                .parse::<jiff::Timestamp>()
                .expect("an instant")
                .as_second(),
        ),
        Err(ReceiptRejection::FieldMismatch("ciphertext_hash")),
    );
}

/// A session that has not finalized has no receipt, and says so distinguishably.
#[tokio::test]
async fn an_unfinalized_upload_has_no_receipt_yet() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    let (first, _, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;

    let problem: Value = fetch(&fixture, &bearer, &id, StatusCode::CONFLICT)
        .await
        .json();
    assert_eq!(problem["code"], "error.upload.receipt_not_available");

    // Half-uploaded is still not finalized.
    fixture
        .chunk(&id, 0, &first, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    fetch(&fixture, &bearer, &id, StatusCode::CONFLICT).await;
}

/// An unknown session and somebody else's are one answer.
#[tokio::test]
async fn an_unknown_session_has_no_receipt_and_no_disclosure() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    let problem: Value = fetch(
        &fixture,
        &bearer,
        "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f",
        StatusCode::NOT_FOUND,
    )
    .await
    .json();
    assert_eq!(problem["code"], "error.upload.session_not_found");
}

/// One custody event, one receipt — and two guards, not one.
///
/// The session state machine is the first: a finalized session is terminal, so a client
/// re-sending its last chunk is refused before issuance is reached. The log's own idempotency is
/// the second, covered by `a_reissued_upload_returns_the_same_receipt` in the port's suite. Two
/// signed statements about one custody event would be indistinguishable from the server
/// double-counting, so it is worth having both.
#[tokio::test]
async fn a_finalized_session_refuses_a_retry_and_its_receipt_is_unchanged() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    let (first, second, whole) = blob();
    let id = upload(&fixture, &bearer, &first, &second, &whole).await;
    let before = fetch(&fixture, &bearer, &id, StatusCode::OK)
        .await
        .bytes()
        .clone();

    // The client lost the acknowledgement and re-sends the last chunk.
    let retry = fixture.chunk(&id, 4096, &second, &bearer).send().await;
    retry.assert_status(StatusCode::CONFLICT);
    let problem: Value = retry.json();
    assert_eq!(problem["code"], "error.upload.session_not_active");

    let after = fetch(&fixture, &bearer, &id, StatusCode::OK)
        .await
        .bytes()
        .clone();
    assert_eq!(
        before, after,
        "the receipt is evidence a client keeps; re-reading it must return the same bytes"
    );
    let receipt = CustodyReceipt::from_canonical_cbor(&after).expect("a receipt");
    assert_eq!(receipt.core.receipt_seq, 1);
}

/// Two uploads chain, and the chain is over the signed bytes.
#[tokio::test]
async fn successive_receipts_chain() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;

    let (first, second, whole) = blob();
    let one = upload(&fixture, &bearer, &first, &second, &whole).await;

    // A second, distinct blob in another album — so it is not a duplicate.
    fixture.authority.allow_album(
        &support::owner(),
        &support::second_album(),
        PROTOCOL_VERSION,
    );
    let other = payload(b'z', 8192);
    let mut request = support::create_request(&fixture.clock, &other, "original");
    request["album_id"] = Value::String(support::second_album().as_str().to_owned());
    request["manifest_envelope"]["album_id"] =
        Value::String(support::second_album().as_str().to_owned());
    request["manifest_envelope"]["file_id"] =
        Value::String("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e62".to_owned());
    let two = fixture.open_session_with(&request, &bearer).await;
    for offset in [0_u64, 4096] {
        let start = usize::try_from(offset).expect("an offset");
        fixture
            .chunk(&two, offset, &other[start..start + 4096], &bearer)
            .send()
            .await
            .assert_status(StatusCode::NO_CONTENT);
    }

    let first_receipt = CustodyReceipt::from_canonical_cbor(
        fetch(&fixture, &bearer, &one, StatusCode::OK).await.bytes(),
    )
    .expect("a receipt");
    let second_receipt = CustodyReceipt::from_canonical_cbor(
        fetch(&fixture, &bearer, &two, StatusCode::OK).await.bytes(),
    )
    .expect("a receipt");

    assert_eq!(first_receipt.core.receipt_seq, 1);
    assert_eq!(second_receipt.core.receipt_seq, 2);
    assert_eq!(
        second_receipt.core.prior_receipt_hash,
        Some(hash_bytes(&first_receipt.to_canonical_cbor())),
        "the chain is over the signed bytes, so altering a predecessor breaks its successor"
    );
}

/// Reading a receipt needs a credential.
#[tokio::test]
async fn a_receipt_requires_a_credential() {
    let fixture = Fixture::working();
    fixture
        .client
        .get("/v1/upload/018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f/receipt")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

/// The published key resolves the receipt, which is what makes it evidence rather than a blob.
#[tokio::test]
async fn the_published_key_history_verifies_a_fetched_receipt() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    let (first, second, whole) = blob();
    let id = upload(&fixture, &bearer, &first, &second, &whole).await;

    let receipt = CustodyReceipt::from_canonical_cbor(
        fetch(&fixture, &bearer, &id, StatusCode::OK).await.bytes(),
    )
    .expect("a receipt");

    // The registry record is public: no credential, because a client pinning the key that
    // checks the server's own liability must not need the server's permission to do it.
    let published: Value = fixture
        .client
        .get("/.well-known/capsule/attestation-keys")
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();

    assert_eq!(published["server_id"], receipt.core.server_id);
    let keys = published["keys"].as_array().expect("a key history");
    assert_eq!(keys.len(), 1);
    assert_eq!(
        keys[0]["key_id"],
        receipt.core.server_key_id.to_hex(),
        "a receipt names the key that signed it, and that key has to resolve in the record"
    );
    assert_eq!(keys[0]["algorithm"], "hybrid-ed25519-mldsa65");
    assert_eq!(
        keys[0]["active_to"],
        Value::Null,
        "the active key has not stopped signing"
    );

    // And the published bytes are the key that actually verifies it.
    let public = base64::engine::general_purpose::STANDARD
        .decode(keys[0]["public"].as_str().expect("a base64 key"))
        .expect("base64");
    assert_eq!(
        public,
        fixture.attestation_key.verifying_key().to_bytes(),
        "publishing a key the server does not sign with would emit evidence nobody can check"
    );
}
