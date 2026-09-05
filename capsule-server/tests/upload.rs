//! The upload surface: the envelope gate, the chunk rules, and finalization (slice `S-C1`).
//!
//! Every case here drives the **real server** through `TestClient` — the same router
//! production runs, over the in-memory adapters of the two state ports and the blob store. What
//! is doubled is what a deployment would put behind Postgres: the account directory and the
//! album/device authority. Nothing in this file reaches past a port.
//!
//! The rejection cases are organised the way the [upload
//! protocol](../../capsule-docs/src/content/docs/design/import/upload-protocol.md) organises
//! them — one case per invariant, and one per row of the strictness table — and each asserts
//! the exact status **and** the `error.*` code, because a client switches on the code and a
//! test that only checked the status would let a code change silently.

mod support;

use kynos::http::StatusCode;
use support::{
    Fixture, PROTOCOL_VERSION, album, checksum, create_request, device, owner, payload,
    second_album,
};

/// The blob every happy-path case transfers: two 4 KiB chunks.
const CHUNK: usize = 4096;

/// The two chunks of that blob, and the whole thing.
fn blob() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let first = payload(b'a', CHUNK);
    let second = payload(b'b', CHUNK);
    let whole: Vec<u8> = first.iter().chain(second.iter()).copied().collect();
    (first, second, whole)
}

/// The `code` extension a rejection carries.
fn code(body: &serde_json::Value) -> &str {
    body["code"].as_str().unwrap_or("<no code>")
}

// ===========================================================================================
// Opening a session
// ===========================================================================================

#[tokio::test]
async fn a_session_opens_and_says_where_to_send_bytes() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (_, _, whole) = blob();

    let response = fixture
        .client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&create_request(&fixture.clock, &whole, "original"))
        .send()
        .await;
    response.assert_status(StatusCode::CREATED);

    let body: serde_json::Value = response.json();
    let id = body["id"].as_str().expect("a session id");
    assert_eq!(body["upload_url"], format!("/v1/upload/{id}"));
    assert_eq!(
        body["suggested_chunk_size"], 262_144,
        "an 8 KiB blob is in the smallest tier"
    );
    response.assert_header("location", &format!("/v1/upload/{id}"));
    response.assert_header("x-capsule-suggested-chunk-size", "262144");

    // The session the server actually opened, read from the store rather than from the body.
    let record = fixture
        .uploads
        .read_for_test(id)
        .await
        .expect("the session is in the store");
    assert_eq!(record.total_size, whole.len() as u64);
    assert_eq!(record.received_bytes, 0);
    assert_eq!(record.expected_hash, checksum(&whole));
    assert_eq!(record.album_id.as_ref(), Some(&album()));
    assert_eq!(record.owner_id, owner());

    // And the stage the first chunk will be appended to.
    assert_eq!(
        fixture.blobs.staged_len_for_test(id).await,
        Some(0),
        "the stage is open and empty"
    );
}

#[tokio::test]
async fn a_duplicate_create_returns_the_active_session_and_its_offset() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, _, whole) = blob();

    let id = fixture.open_session(&whole, "original", &bearer).await;
    fixture
        .chunk(&id, 0, &first, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // The same `(owner, hash, album)` tuple again: the active session, never a second one.
    let response = fixture
        .client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&create_request(&fixture.clock, &whole, "original"))
        .send()
        .await;
    response.assert_status(StatusCode::OK);
    response.assert_header("x-capsule-offset", "4096");

    let body: serde_json::Value = response.json();
    assert_eq!(
        body["id"].as_str(),
        Some(id.as_str()),
        "a duplicate create must not open a second session for the same bytes"
    );
}

#[tokio::test]
async fn every_creation_invariant_is_refused_with_its_status_and_code() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (_, _, whole) = blob();

    // Each case makes exactly one thing wrong with an otherwise valid request.
    type Mutate = fn(&mut serde_json::Value);
    let cases: Vec<(&str, Mutate, StatusCode, &str)> = vec![
        (
            "invariant 1: a protocol version outside the window",
            |body| {
                body["protocol_version"] = "2020-01-01".into();
                body["manifest_envelope"]["protocol_version"] = "2020-01-01".into();
            },
            StatusCode::UPGRADE_REQUIRED,
            "error.protocol.version_unsupported",
        ),
        (
            "invariant 1: a protocol version that is not a date",
            |body| {
                body["protocol_version"] = "yesterday".into();
                body["manifest_envelope"]["protocol_version"] = "yesterday".into();
            },
            StatusCode::BAD_REQUEST,
            "error.upload.malformed_request",
        ),
        (
            "invariant 2: a suite the server does not implement",
            |body| {
                body["crypto_suite_id"] = 0x9999.into();
                body["manifest_envelope"]["crypto_suite_id"] = 0x9999.into();
            },
            StatusCode::BAD_REQUEST,
            "error.upload.unknown_crypto_suite",
        ),
        (
            "invariant 3: a hash that is not the suite's digest",
            |body| {
                body["hash"] = "abcd".into();
                body["manifest_envelope"]["ciphertext_hash"] = "abcd".into();
            },
            StatusCode::BAD_REQUEST,
            "error.upload.invalid_hash",
        ),
        (
            "invariant 4: a zero-length blob",
            |body| body["size"] = 0.into(),
            StatusCode::BAD_REQUEST,
            "error.upload.invalid_size",
        ),
        (
            "invariant 4: a blob past this deployment's ceiling",
            |body| body["size"] = (8_u64 * 1024 * 1024 * 1024).into(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "error.upload.file_too_large",
        ),
        (
            "invariant 5: a content type outside the closed enum",
            |body| body["content_type"] = "application/x-evil".into(),
            StatusCode::BAD_REQUEST,
            "error.upload.unsupported_content_type",
        ),
        (
            "invariant 6: an album the authority does not hold",
            |body| {
                body["album_id"] = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5eff".into();
                body["manifest_envelope"]["album_id"] =
                    "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5eff".into();
            },
            StatusCode::FORBIDDEN,
            "error.upload.album_access_denied",
        ),
        (
            "invariant 6: no album at all, which cannot be checked",
            |body| {
                body["album_id"] = serde_json::Value::Null;
                body["manifest_envelope"]["album_id"] = serde_json::Value::Null;
            },
            StatusCode::FORBIDDEN,
            "error.upload.album_access_denied",
        ),
        (
            "invariant 7: a device the directory does not carry",
            |body| {
                body["manifest_envelope"]["created_by_device"] =
                    "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5eaa".into();
            },
            StatusCode::FORBIDDEN,
            "error.upload.device_not_authorized",
        ),
        (
            "invariant 8: a timestamp far outside the drift window",
            |body| body["manifest_envelope"]["timestamp"] = "2020-01-01T00:00:00Z".into(),
            StatusCode::BAD_REQUEST,
            "error.upload.timestamp_out_of_range",
        ),
        (
            "invariant 15: a top-level hash the envelope contradicts",
            |body| {
                body["manifest_envelope"]["ciphertext_hash"] =
                    "2222222222222222222222222222222222222222222222222222222222222222".into();
            },
            StatusCode::BAD_REQUEST,
            "error.upload.envelope_mismatch",
        ),
        (
            "an action this surface does not open sessions for",
            |body| {
                body["manifest_envelope"]["action"] = "metadata-update".into();
                body["manifest_envelope"]["prior_provenance_hash"] =
                    "3333333333333333333333333333333333333333333333333333333333333333".into();
            },
            StatusCode::BAD_REQUEST,
            "error.upload.invalid_action",
        ),
        (
            "an upload on behalf of an owner with no verified relationship",
            |body| body["owner_id"] = "somebody-else".into(),
            StatusCode::FORBIDDEN,
            "error.upload.owner_not_permitted",
        ),
    ];

    for (name, mutate, status, expected) in cases {
        let mut body = create_request(&fixture.clock, &whole, "original");
        mutate(&mut body);

        let response = fixture
            .client
            .post("/v1/upload")
            .header("authorization", &bearer)
            .header("x-capsule-protocol", PROTOCOL_VERSION)
            .json(&body)
            .send()
            .await;
        assert_eq!(response.status(), status, "{name}");
        assert_eq!(
            code(&response.json::<serde_json::Value>()),
            expected,
            "{name}"
        );
    }
}

#[tokio::test]
async fn a_metadata_blob_must_be_the_one_its_manifest_committed_to() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (_, _, whole) = blob();

    // The fixture commits the metadata role's manifest to this blob's own address: accepted.
    let ok = fixture
        .client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&create_request(&fixture.clock, &whole, "metadata"))
        .send()
        .await;
    ok.assert_status(StatusCode::CREATED);

    // Committed to some other object: refused as a contradiction (invariant 25).
    let mut body = create_request(&fixture.clock, &whole, "metadata");
    body["manifest_envelope"]["metadata_blob_hash"] =
        "4444444444444444444444444444444444444444444444444444444444444444".into();
    let refused = fixture
        .client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&body)
        .send()
        .await;
    refused.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        code(&refused.json::<serde_json::Value>()),
        "error.upload.envelope_mismatch"
    );
}

#[tokio::test]
async fn an_unknown_field_is_a_client_bug_rather_than_something_to_ignore() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (_, _, whole) = blob();

    let mut body = create_request(&fixture.clock, &whole, "original");
    body["surprise"] = 1.into();

    // Strictness is serde's `deny_unknown_fields`, and Kynos classifies it as a schema
    // failure: `422`. The status is the framework's rather than the taxonomy's `400
    // error.upload.malformed_request`, and it carries no code — recorded, not papered over.
    fixture
        .client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&body)
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn the_handshake_gates_every_upload_request() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (_, _, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;

    // Missing: a coded 400, on every operation.
    let missing = fixture
        .client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .json(&create_request(&fixture.clock, &whole, "original"))
        .send()
        .await;
    missing.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        code(&missing.json::<serde_json::Value>()),
        "error.upload.malformed_request"
    );

    let head = fixture
        .client
        .head(&format!("/v1/upload/{id}"))
        .header("authorization", &bearer)
        .send()
        .await;
    head.assert_status(StatusCode::BAD_REQUEST);

    // Out of the window: `426`, carrying the window a client can act on.
    let refused = fixture
        .client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", "2020-01-01")
        .json(&create_request(&fixture.clock, &whole, "original"))
        .send()
        .await;
    refused.assert_status(StatusCode::UPGRADE_REQUIRED);
    let body: serde_json::Value = refused.json();
    assert_eq!(code(&body), "error.protocol.version_unsupported");
    assert_eq!(body["protocol_min"], "2026-01-01");
    assert_eq!(body["protocol_max"], "2026-12-31");
}

// ===========================================================================================
// Chunks
// ===========================================================================================

#[tokio::test]
async fn a_full_upload_finalizes_and_the_bytes_become_a_blob() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, second, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;

    let ack = fixture.chunk(&id, 0, &first, &bearer).send().await;
    ack.assert_status(StatusCode::NO_CONTENT);
    ack.assert_header("x-capsule-offset", "4096");

    let done = fixture.chunk(&id, 4096, &second, &bearer).send().await;
    done.assert_status(StatusCode::NO_CONTENT);
    done.assert_header("x-capsule-offset", "8192");

    // The session is terminal and successful…
    let record = fixture
        .uploads
        .read_for_test(&id)
        .await
        .expect("the session survives as a receipt");
    assert_eq!(record.status.as_str(), "completed");

    // …the stage is gone…
    assert_eq!(fixture.blobs.staged_len_for_test(&id).await, None);

    // …and the blob is at its content address, byte for byte.
    assert_eq!(
        fixture.blobs.blob_for_test(&checksum(&whole)).await,
        Some(whole),
        "the bytes the server stored are the bytes the client sent"
    );
}

#[tokio::test]
async fn a_provenance_blob_is_stored_exactly_as_it_arrived() {
    // `S-C30`: the signed manifest rides as a provenance blob and the server stores the signed
    // bytes verbatim. Nothing on this path re-serializes anything into manifest bytes, and this
    // is the case that says so.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, second, whole) = blob();
    let id = fixture.open_session(&whole, "provenance", &bearer).await;

    fixture
        .chunk(&id, 0, &first, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    fixture
        .chunk(&id, 4096, &second, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    assert_eq!(
        fixture.blobs.blob_for_test(&checksum(&whole)).await,
        Some(whole)
    );
}

#[tokio::test]
async fn identical_bytes_become_one_object() {
    // The same ciphertext legitimately belongs to assets in two albums — one thumbnail shared
    // between a copy in each. Neither is the other's duplicate (a `409` is the client's *merge*
    // trigger, and across albums there is nothing to merge), so both uploads proceed and the
    // blob store is what deduplicates them onto one address.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    fixture
        .authority
        .allow_album(&owner(), &second_album(), PROTOCOL_VERSION);
    let (first, second, whole) = blob();

    for album in [album(), second_album()] {
        let mut request = create_request(&fixture.clock, &whole, "original");
        request["album_id"] = serde_json::Value::String(album.as_str().to_owned());
        request["manifest_envelope"]["album_id"] =
            serde_json::Value::String(album.as_str().to_owned());
        // A distinct asset per album: the id is the manifest's, and reserving the *same* id
        // under a second album is a conflict, not a join.
        request["manifest_envelope"]["file_id"] = serde_json::Value::String(format!(
            "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e{:02x}",
            0x61 + u8::from(album == second_album())
        ));

        let id = fixture.open_session_with(&request, &bearer).await;
        fixture
            .chunk(&id, 0, &first, &bearer)
            .send()
            .await
            .assert_status(StatusCode::NO_CONTENT);
        fixture
            .chunk(&id, 4096, &second, &bearer)
            .send()
            .await
            .assert_status(StatusCode::NO_CONTENT);
    }

    assert_eq!(
        fixture.blobs.blob_count_for_test().await,
        1,
        "identical ciphertext is one blob, not two"
    );
}

#[tokio::test]
async fn the_same_bytes_in_the_same_album_are_refused_as_a_duplicate() {
    // The other half of the rule, and the one that makes the case above meaningful: within one
    // album a finalized address is `409 error.upload.duplicate_blob`, carrying the asset the
    // client must merge against.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, second, whole) = blob();

    let id = fixture.open_session(&whole, "original", &bearer).await;
    fixture
        .chunk(&id, 0, &first, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    fixture
        .chunk(&id, 4096, &second, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let refusal = fixture
        .client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&create_request(&fixture.clock, &whole, "original"))
        .send()
        .await;
    refusal.assert_status(StatusCode::CONFLICT);
    let problem: serde_json::Value = refusal.json();
    assert_eq!(problem["code"], "error.upload.duplicate_blob");
    assert_eq!(
        problem["existing_asset"], "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61",
        "the client is told which asset to merge against, and nothing else"
    );
}

#[tokio::test]
async fn a_replayed_chunk_is_a_no_op_that_answers_the_same_offset() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, _, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;

    let first_ack = fixture.chunk(&id, 0, &first, &bearer).send().await;
    first_ack.assert_status(StatusCode::NO_CONTENT);
    first_ack.assert_header("x-capsule-offset", "4096");

    // The same `(upload, offset, checksum)` tuple: the client lost the acknowledgement, not the
    // bytes. It gets the same answer and nothing is appended twice.
    let replay = fixture.chunk(&id, 0, &first, &bearer).send().await;
    replay.assert_status(StatusCode::NO_CONTENT);
    replay.assert_header("x-capsule-offset", "4096");

    assert_eq!(
        fixture.blobs.staged_len_for_test(&id).await,
        Some(4096),
        "a replay appends nothing"
    );
}

#[tokio::test]
async fn every_chunk_rule_is_refused_with_its_status_and_code() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, second, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;

    // Missing offset.
    let response = fixture
        .client
        .patch(&format!("/v1/upload/{id}"))
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .header("x-capsule-checksum", &checksum(&first))
        .body("application/octet-stream", first.clone())
        .send()
        .await;
    response.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.missing_offset"
    );

    // Missing checksum — the idempotency tuple is undefined without it.
    let response = fixture
        .client
        .patch(&format!("/v1/upload/{id}"))
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .header("x-capsule-offset", "0")
        .body("application/octet-stream", first.clone())
        .send()
        .await;
    response.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.missing_checksum"
    );

    // A checksum that is not the body's: nothing is persisted.
    let response = fixture
        .chunk(&id, 0, &first, &bearer)
        .header("x-capsule-checksum", &checksum(&second))
        .send()
        .await;
    response.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.checksum_mismatch"
    );
    assert_eq!(
        fixture.blobs.staged_len_for_test(&id).await,
        Some(0),
        "a corrupted chunk leaves the offset where it was"
    );

    // An empty body.
    let empty: Vec<u8> = Vec::new();
    let response = fixture.chunk(&id, 0, &empty, &bearer).send().await;
    response.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.empty_chunk"
    );

    // A non-final chunk that is not 4 KiB-aligned.
    let unaligned = payload(b'c', 4000);
    let response = fixture.chunk(&id, 0, &unaligned, &bearer).send().await;
    response.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.chunk_not_aligned"
    );

    // A body that is not opaque bytes.
    let response = fixture
        .client
        .patch(&format!("/v1/upload/{id}"))
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .header("x-capsule-offset", "0")
        .header("x-capsule-checksum", &checksum(&first))
        .json(&serde_json::json!({ "not": "bytes" }))
        .send()
        .await;
    response.assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.unsupported_media_type"
    );

    // A gapped offset names the authoritative one.
    let response = fixture.chunk(&id, 4096, &second, &bearer).send().await;
    response.assert_status(StatusCode::CONFLICT);
    let body: serde_json::Value = response.json();
    assert_eq!(code(&body), "error.upload.offset_mismatch");
    assert_eq!(body["offset"], 0, "the offset to resume from");
}

#[tokio::test]
async fn the_same_offset_with_different_bytes_is_a_conflict() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, second, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;

    fixture
        .chunk(&id, 0, &first, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // Offset 0 again, with different bytes: a client retrying with garbage, not a replay.
    let response = fixture.chunk(&id, 0, &second, &bearer).send().await;
    response.assert_status(StatusCode::CONFLICT);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.chunk_conflict"
    );
}

#[tokio::test]
async fn a_chunk_past_the_declared_size_fails_the_session() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, _, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;

    fixture
        .chunk(&id, 0, &first, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // 8 KiB more onto an 8 KiB declaration: the declaration is broken, so the session is
    // unsalvageable by definition.
    let too_much = payload(b'd', 8192);
    let response = fixture.chunk(&id, 4096, &too_much, &bearer).send().await;
    response.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.size_exceeded"
    );

    let record = fixture
        .uploads
        .read_for_test(&id)
        .await
        .expect("the session is retained as a receipt");
    assert_eq!(record.status.as_str(), "failed_processing");
    assert_eq!(
        fixture.blobs.staged_len_for_test(&id).await,
        None,
        "a failed session's bytes are dropped"
    );
}

#[tokio::test]
async fn a_chunk_past_the_protocol_ceiling_is_refused() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (_, _, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;

    // One byte past 16 MiB, and comfortably under the 32 MiB transport backstop — which is why
    // this rejection is the protocol's coded one rather than a bare framework `413`.
    let oversized = payload(b'e', 16 * 1024 * 1024 + 1);
    let response = fixture.chunk(&id, 0, &oversized, &bearer).send().await;
    response.assert_status(StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.chunk_too_large"
    );
}

#[tokio::test]
async fn only_the_uploader_may_append_and_the_session_is_hidden_from_others() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, _, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;

    let intruder = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;
    let response = fixture.chunk(&id, 0, &first, &intruder).send().await;
    response.assert_status(StatusCode::FORBIDDEN);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.forbidden"
    );

    let looking = fixture
        .client
        .head(&format!("/v1/upload/{id}"))
        .header("authorization", &intruder)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await;
    looking.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_unknown_session_is_indistinguishable_from_an_expired_one() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, _, _) = blob();

    let response = fixture
        .chunk("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5eee", 0, &first, &bearer)
        .send()
        .await;
    response.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.session_not_found"
    );
}

#[tokio::test]
async fn a_chunk_after_finalization_is_refused() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, second, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;

    fixture
        .chunk(&id, 0, &first, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    fixture
        .chunk(&id, 4096, &second, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // The terminal outcome is read through `HEAD`; a chunk against it is refused.
    let response = fixture.chunk(&id, 0, &first, &bearer).send().await;
    response.assert_status(StatusCode::CONFLICT);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.session_not_active"
    );
}

// ===========================================================================================
// Finalization
// ===========================================================================================

#[tokio::test]
async fn bytes_that_do_not_hash_to_the_declaration_fail_the_session() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, _, whole) = blob();

    // Declare the blob, then send different bytes for the second chunk. Each chunk's own
    // checksum is honest, so only the whole-blob recomputation can catch this — which is
    // exactly what invariant 14 is for.
    let id = fixture.open_session(&whole, "original", &bearer).await;
    fixture
        .chunk(&id, 0, &first, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let substituted = payload(b'z', 4096);
    let response = fixture.chunk(&id, 4096, &substituted, &bearer).send().await;
    response.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.content_hash_mismatch"
    );

    let record = fixture.uploads.read_for_test(&id).await.expect("a receipt");
    assert_eq!(record.status.as_str(), "failed_processing");
    assert_eq!(fixture.blobs.staged_len_for_test(&id).await, None);
    assert_eq!(
        fixture.blobs.blob_count_for_test().await,
        0,
        "nothing is committed when the hash does not match"
    );
}

#[tokio::test]
async fn finalization_re_checks_the_envelope_against_the_present() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, second, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;

    fixture
        .chunk(&id, 0, &first, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // The device is revoked while the transfer is in flight. Creation validated against a
    // directory that carried it; finalization must not.
    fixture.authority.revoke_device(&support::user(), device());

    let response = fixture.chunk(&id, 4096, &second, &bearer).send().await;
    response.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.envelope_rejected"
    );
    assert_eq!(
        fixture.blobs.blob_count_for_test().await,
        0,
        "a write refused at finalization commits nothing"
    );
}

#[tokio::test]
async fn a_closed_album_stops_a_transfer_that_was_already_in_flight() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, second, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;

    fixture
        .chunk(&id, 0, &first, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    fixture.authority.close_album(&owner(), &album());

    let response = fixture.chunk(&id, 4096, &second, &bearer).send().await;
    response.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.envelope_rejected"
    );
}

#[tokio::test]
async fn losing_the_finalize_claim_is_a_race_rather_than_a_failure() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, second, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;

    fixture
        .chunk(&id, 0, &first, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // A concurrent finalizer takes the claim in the one window this request cannot see: after
    // its own chunk is recorded and before it reaches `claim_finalize`. The chunk was still
    // accepted, so answering it with a `409` — as the Salvo server did — would tell the client
    // its own good work had failed.
    fixture.uploads.claim_after_next_progress();

    let response = fixture.chunk(&id, 4096, &second, &bearer).send().await;
    response.assert_status(StatusCode::NO_CONTENT);
    response.assert_header("x-capsule-offset", "8192");
}

#[tokio::test]
async fn an_acknowledged_chunk_that_did_not_land_is_the_servers_inconsistency() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, second, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;

    // A crash between the durable append and the counter that records it: the acknowledgement
    // stands, the bytes do not.
    fixture.blobs.swallow_next_append();
    fixture
        .chunk(&id, 0, &first, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let response = fixture.chunk(&id, 4096, &second, &bearer).send().await;
    response.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.storage_inconsistent"
    );
}

// ===========================================================================================
// Progress and cancellation
// ===========================================================================================

#[tokio::test]
async fn head_reports_the_authoritative_offset_and_state() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, _, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;

    let before = fixture
        .client
        .head(&format!("/v1/upload/{id}"))
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await;
    before.assert_status(StatusCode::OK);
    before.assert_header("x-capsule-offset", "0");
    before.assert_header("x-capsule-content-length", "8192");
    before.assert_header("x-capsule-upload-status", "pending");
    before.assert_header("cache-control", "no-store");
    assert!(before.bytes().is_empty(), "a HEAD carries no body");

    fixture
        .chunk(&id, 0, &first, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let after = fixture
        .client
        .head(&format!("/v1/upload/{id}"))
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await;
    after.assert_header("x-capsule-offset", "4096");
    after.assert_header("x-capsule-upload-status", "uploading");
}

#[tokio::test]
async fn cancelling_takes_the_session_and_its_bytes_together() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, _, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;
    fixture
        .chunk(&id, 0, &first, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    fixture
        .client
        .delete(&format!("/v1/upload/{id}"))
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    assert!(fixture.uploads.read_for_test(&id).await.is_none());
    assert_eq!(fixture.blobs.staged_len_for_test(&id).await, None);

    // And it is now indistinguishable from a session that never existed.
    fixture
        .client
        .head(&format!("/v1/upload/{id}"))
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_finalizing_session_is_not_interruptible() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (_, _, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;
    fixture.uploads.claim_for_test(&id).await;

    let response = fixture
        .client
        .delete(&format!("/v1/upload/{id}"))
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await;
    response.assert_status(StatusCode::CONFLICT);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.session_not_active"
    );
}

// ===========================================================================================
// Collaborator failure
// ===========================================================================================

#[tokio::test]
async fn a_store_that_cannot_answer_is_a_coded_500() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, _, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;

    fixture.uploads.set_unavailable(true);

    let chunking = fixture.chunk(&id, 0, &first, &bearer).send().await;
    chunking.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        code(&chunking.json::<serde_json::Value>()),
        "error.upload.unavailable"
    );

    let creating = fixture
        .client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&create_request(&fixture.clock, &whole, "original"))
        .send()
        .await;
    creating.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        code(&creating.json::<serde_json::Value>()),
        "error.upload.unavailable"
    );
}

#[tokio::test]
async fn an_authority_that_cannot_answer_refuses_rather_than_assuming() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (_, _, whole) = blob();

    fixture.authority.set_unavailable(true);

    let response = fixture
        .client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&create_request(&fixture.clock, &whole, "original"))
        .send()
        .await;
    response.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        code(&response.json::<serde_json::Value>()),
        "error.upload.unavailable"
    );
}

// ===========================================================================================
// The crash boundary
// ===========================================================================================

/// **E2E case 11.** A crash between the blob rename and the index commit leaves no dangling
/// reference, and the retry recovers.
///
/// The order finalization runs in is the contract, and it is the way round it is *because* of
/// this case (`upload/finalize.rs`): the blob is committed onto its content address — a rename
/// and an fsync, irreversible — and only then is it recorded against its asset. A crash in that
/// window leaves a blob nothing references, which is the **safe** half of the trade: an orphan
/// is what refcount GC exists to collect, while an asset row naming a blob the store does not
/// hold is a dangling reference the feed would serve and the scrub would report as an integrity
/// error that is never auto-repaired.
///
/// What is asserted, in the order a recovering operator would look at it:
///
/// 1. the session is terminal and **failed**, not left claimed forever;
/// 2. the bytes are at their content address — custody was taken, and telling the client
///    otherwise would be a lie about something the server holds;
/// 3. the asset row is still `Pending` and holds no sequence number, so nothing was published
///    and there is no zombie visible row;
/// 4. **nothing references the blob** — `find_reference` is `None` and `reference_count` is 0 —
///    which is the property the whole ordering exists to guarantee;
/// 5. the collector marks it, so the orphan is reclaimable rather than permanent;
/// 6. and the retry publishes, because `BlobStore::commit` is idempotent on identical ciphertext.
///
/// The crash is injected through the `AssetIndex` port itself (`support::fault`), so no
/// production code carries a test hook. A *process*-level restart — a real kill and a second
/// process over the same blob root and database — belongs to the binary-smoke tier and is filed
/// with the remaining durable adapters.
#[tokio::test]
async fn finalization_crash_between_rename_and_commit_leaves_no_dangling_reference() {
    use capsule_server::blob::ContentAddress;
    use capsule_server::gc::{CollectionContext, Mode, collect};
    use capsule_server::index::{AssetIndex, AssetState};
    use capsule_server::store::{AssetId, OwnerId};

    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first, second, whole) = blob();
    let id = fixture.open_session(&whole, "original", &bearer).await;
    let address = ContentAddress::parse(&checksum(&whole)).expect("the digest is an address");
    // The asset the suite's manifests name; the session reserved its row when it opened.
    let asset = AssetId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61");

    fixture
        .chunk(&id, 0, &first, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // The last chunk completes the declared size, so this request is the one that finalizes.
    fixture.index_fault.arm();
    let crashed = fixture.chunk(&id, 4096, &second, &bearer).send().await;
    crashed.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        fixture.index_fault.fired(),
        1,
        "the fault never fired, so everything below is describing an ordinary upload"
    );

    // 1. Terminal and failed. A claimed session that is never driven anywhere is the state the
    //    finalization state machine exists to make unreachable.
    let record = fixture
        .uploads
        .read_for_test(&id)
        .await
        .expect("the session survives as a receipt");
    assert_eq!(record.status.as_str(), "failed_processing");

    // 2. Custody was taken: the rename happened before the index call that was lost.
    assert_eq!(
        fixture.blobs.blob_for_test(&checksum(&whole)).await,
        Some(whole.clone()),
        "the bytes committed before the crash window are the bytes the server holds"
    );

    // 3. No zombie row: the asset is exactly where it was before the transfer.
    let row = fixture
        .index
        .read(&asset)
        .await
        .expect("the index answers")
        .expect("the session reserved a row when it opened");
    assert_eq!(row.state, AssetState::Pending);
    assert_eq!(row.sync_seq, None, "a lost transaction published nothing");
    assert!(row.blobs.is_empty(), "and recorded no blob");

    // 4. The property the ordering exists for. A dangling reference is the failure mode the
    //    other order would produce, and it is the one nothing can repair automatically.
    assert_eq!(
        fixture
            .index
            .find_reference(&address)
            .await
            .expect("the index answers"),
        None,
        "a blob nothing references must not be reachable through the serving path"
    );
    assert_eq!(
        fixture
            .index
            .reference_count(&address)
            .await
            .expect("the index answers"),
        0,
    );
    assert!(
        fixture
            .index
            .feed_page(&OwnerId::new(owner().as_str()), 0, 10)
            .await
            .expect("the index answers")
            .is_empty(),
        "nothing was published, so nothing reaches a client's feed"
    );

    // 5. The orphan is reclaimable. The collector marks a zero-reference blob on one pass and
    //    sweeps it on a later one once the grace window has passed, so a mark is the whole of
    //    what a first pass should do — and it is what makes "an orphan GC collects" true rather
    //    than a hope.
    let collection = CollectionContext::new(
        fixture.index.clone(),
        fixture.blobs.clone(),
        fixture.marks.clone(),
        fixture.quotas.clone(),
        fixture.clock.clone(),
        capsule_server::gc::DEFAULT_GRACE_WINDOW,
    );
    let report = collect(&collection, Mode::Apply)
        .await
        .expect("a collection pass runs");
    assert!(
        report.marked.contains(&address),
        "the crashed upload's blob must be collectable, got {report:?}"
    );
    assert!(
        report.dangling.is_empty(),
        "a crash in this window must never produce a dangling reference: {report:?}"
    );

    // 6. And the client retries. `BlobStore::commit` is idempotent on identical ciphertext, so
    //    the second transfer lands on the occupied address and the asset finally publishes.
    let retry = fixture.open_session(&whole, "original", &bearer).await;
    fixture
        .chunk(&retry, 0, &first, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    fixture
        .chunk(&retry, 4096, &second, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    assert_eq!(
        fixture
            .uploads
            .read_for_test(&retry)
            .await
            .expect("the retry's session survives")
            .status
            .as_str(),
        "completed",
    );
    let recovered = fixture
        .index
        .read(&asset)
        .await
        .expect("the index answers")
        .expect("the row is still there");
    assert_eq!(
        recovered.address_for(capsule_server::store::BlobRole::Original),
        Some(&address),
        "the retry recorded the blob the crash lost",
    );
    assert_eq!(
        fixture
            .index
            .reference_count(&address)
            .await
            .expect("the index answers"),
        1,
        "and the orphan is an orphan no longer",
    );
    assert_eq!(
        fixture.blobs.blob_for_test(&checksum(&whole)).await,
        Some(whole),
        "the bytes are unchanged: an identical ciphertext is one object",
    );
}
