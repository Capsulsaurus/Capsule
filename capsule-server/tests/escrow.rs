//! The master-key escrow surface (slice `S-C12`), end to end.
//!
//! The case that carries the slice is `a_rotation_replaces_the_escrow_and_the_old_one_is_gone`:
//! after a guided re-wrap the lost recovery secret must unwrap nothing, and a server that kept
//! the previous blob would preserve exactly the artifact the rotation exists to destroy.

mod support;

use kynos::http::StatusCode;
use serde_json::Value;
use support::Fixture;

/// A blob that is not a real wrap and does not need to be — the server cannot tell.
fn wrapped(seed: u8, len: usize) -> Vec<u8> {
    (0..len).map(|i| seed ^ (i as u8)).collect()
}

/// Store `blob` and assert the status.
async fn store(
    fixture: &Fixture,
    bearer: &str,
    blob: Vec<u8>,
    expect: StatusCode,
) -> kynos::test::TestResponse {
    let response = fixture
        .client
        .put("/v1/auth/escrow")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .body("application/octet-stream", blob)
        .send()
        .await;
    response.assert_status(expect);
    response
}

/// Fetch the caller's escrow and assert the status.
async fn fetch(fixture: &Fixture, bearer: &str, expect: StatusCode) -> kynos::test::TestResponse {
    let response = fixture
        .client
        .get("/v1/auth/escrow")
        .header("authorization", bearer)
        .send()
        .await;
    response.assert_status(expect);
    response
}

#[tokio::test]
async fn a_stored_escrow_comes_back_byte_for_byte() {
    // The bytes are what a client runs its KDF against. A re-encoded wrap is a wrap that no
    // longer opens, and the failure would look like a lost master key.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let blob = wrapped(0xA5, 512);

    let body: Value = store(&fixture, &bearer, blob.clone(), StatusCode::OK)
        .await
        .json();
    assert_eq!(body["replaced"], false);

    let fetched = fetch(&fixture, &bearer, StatusCode::OK).await;
    assert_eq!(
        fetched.header("content-type"),
        Some("application/octet-stream"),
        "the escrow is ciphertext with no schema the server knows, and says so"
    );
    assert_eq!(fetched.bytes().as_ref(), blob.as_slice());
}

#[tokio::test]
async fn a_rotation_replaces_the_escrow_and_the_old_one_is_gone() {
    // The single-active-escrow rule, which is the whole point of a guided re-wrap: the lost
    // recovery secret has to stop working.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let first = wrapped(0x11, 256);
    let second = wrapped(0x22, 300);

    store(&fixture, &bearer, first.clone(), StatusCode::OK).await;
    let body: Value = store(&fixture, &bearer, second.clone(), StatusCode::OK)
        .await
        .json();
    assert_eq!(
        body["replaced"], true,
        "a rotation and a first escrow are different events, and a client acts on which it was"
    );

    let fetched = fetch(&fixture, &bearer, StatusCode::OK).await;
    assert_eq!(fetched.bytes().as_ref(), second.as_slice());
    assert_ne!(
        fetched.bytes().as_ref(),
        first.as_slice(),
        "the previous wrap must be unreachable, not merely superseded"
    );
}

#[tokio::test]
async fn an_account_with_no_escrow_is_a_distinct_answer_from_a_failure() {
    // A client tells "you have no recovery backup" (a setup prompt) from "we could not read it"
    // (a retry) by the code, and getting that wrong sends a user to the wrong screen.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    let problem: Value = fetch(&fixture, &bearer, StatusCode::NOT_FOUND).await.json();
    assert_eq!(problem["code"], "error.escrow.not_stored");

    fixture.escrows.set_unavailable(true);
    let problem: Value = fetch(&fixture, &bearer, StatusCode::INTERNAL_SERVER_ERROR)
        .await
        .json();
    assert_eq!(problem["code"], "error.escrow.unavailable");
}

#[tokio::test]
async fn one_accounts_escrow_is_not_another_s() {
    // There is no `{user_id}` on this path, so fetching somebody else's is unrepresentable
    // rather than forbidden — this pins that the scoping is real and not incidental.
    let fixture = Fixture::working();
    let mine = fixture.bearer().await;
    let theirs = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;

    store(&fixture, &mine, wrapped(0x33, 128), StatusCode::OK).await;
    fetch(&fixture, &theirs, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn a_body_that_cannot_be_an_escrow_is_refused() {
    // The one judgement the server is entitled to make. It is a size bound, not a format check:
    // the server cannot tell a real wrap from noise of the same length, and pretending
    // otherwise would put a format it does not own on its critical path.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    let problem: Value = store(&fixture, &bearer, Vec::new(), StatusCode::BAD_REQUEST)
        .await
        .json();
    assert_eq!(problem["code"], "error.escrow.malformed");

    let huge = wrapped(0x44, capsule_server::escrow::MAX_ESCROW_BYTES + 1);
    let problem: Value = store(&fixture, &bearer, huge, StatusCode::BAD_REQUEST)
        .await
        .json();
    assert_eq!(problem["code"], "error.escrow.malformed");

    fetch(&fixture, &bearer, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn a_refused_store_leaves_the_held_escrow_alone() {
    // A rejected rotation must not be a deletion. Otherwise a client bug that sends an empty
    // body once would take out the account's only recovery path.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let held = wrapped(0x55, 200);
    store(&fixture, &bearer, held.clone(), StatusCode::OK).await;

    store(&fixture, &bearer, Vec::new(), StatusCode::BAD_REQUEST).await;

    let fetched = fetch(&fixture, &bearer, StatusCode::OK).await;
    assert_eq!(fetched.bytes().as_ref(), held.as_slice());
}

#[tokio::test]
async fn the_escrow_surface_requires_a_credential() {
    let fixture = Fixture::working();
    fixture
        .client
        .get("/v1/auth/escrow")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    fixture
        .client
        .put("/v1/auth/escrow")
        .body("application/octet-stream", wrapped(0x66, 64))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_stored_at_a_put_echoes_is_the_stored_at_the_store_holds() {
    // The durable adapter keeps microseconds. A route that echoed the clock's nanoseconds would
    // tell the client one `stored_at` and hold another, and the stale-cache comparison the field
    // exists for would call a current escrow stale.
    use capsule_server::escrow::EscrowStore as _;
    use capsule_server::store::UserId;

    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    fixture
        .clock
        .advance(jiff::SignedDuration::from_nanos(123_456_789));

    let body: Value = store(&fixture, &bearer, wrapped(0x77, 64), StatusCode::OK)
        .await
        .json();
    let echoed: jiff::Timestamp = body["stored_at"]
        .as_str()
        .expect("stored_at is a string")
        .parse()
        .expect("stored_at is RFC 3339");
    assert_eq!(
        echoed.subsec_nanosecond() % 1_000,
        0,
        "the echoed instant must carry no precision an adapter drops: {echoed}"
    );
    let held = fixture
        .escrows
        .fetch(&UserId::new(support::owner().as_str()))
        .await
        .expect("the store answers")
        .expect("an escrow is held");
    assert_eq!(held.stored_at, echoed);
}

// ===========================================================================================
// The sealed identity beside the escrow (#544)
// ===========================================================================================

/// Where the sealed identity lives.
const IDENTITY: &str = "/v1/auth/escrow/identity";

/// Store `blob` as the sealed identity and assert the status.
async fn store_identity(
    fixture: &Fixture,
    bearer: &str,
    blob: Vec<u8>,
    expect: StatusCode,
) -> kynos::test::TestResponse {
    let response = fixture
        .client
        .put(IDENTITY)
        .header("authorization", bearer)
        .header("accept", "application/json")
        .body("application/octet-stream", blob)
        .send()
        .await;
    response.assert_status(expect);
    response
}

/// Fetch the caller's sealed identity and assert the status.
async fn fetch_identity(
    fixture: &Fixture,
    bearer: &str,
    expect: StatusCode,
) -> kynos::test::TestResponse {
    let response = fixture
        .client
        .get(IDENTITY)
        .header("authorization", bearer)
        .send()
        .await;
    response.assert_status(expect);
    response
}

#[tokio::test]
async fn a_sealed_identity_comes_back_byte_for_byte_and_replaces() {
    // A recovered device opens the seal under the master key it recovered; a re-encoded seal
    // would not open, and the failure would look like the wrong master key.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let first = wrapped(0x81, 120);
    let resealed = wrapped(0x82, 140);

    let body: Value = store_identity(&fixture, &bearer, first, StatusCode::OK)
        .await
        .json();
    assert_eq!(body["replaced"], false);
    let body: Value = store_identity(&fixture, &bearer, resealed.clone(), StatusCode::OK)
        .await
        .json();
    assert_eq!(
        body["replaced"], true,
        "only one sealed identity per account"
    );

    let fetched = fetch_identity(&fixture, &bearer, StatusCode::OK).await;
    assert_eq!(
        fetched.header("content-type"),
        Some("application/octet-stream")
    );
    assert_eq!(fetched.bytes().as_ref(), resealed.as_slice());
}

#[tokio::test]
async fn the_sealed_identity_and_the_escrow_are_stored_independently() {
    // A recovery-secret rotation replaces the escrow and must leave the identity alone, and an
    // identity stored first must not read as an escrow.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let identity = wrapped(0x91, 100);

    store_identity(&fixture, &bearer, identity.clone(), StatusCode::OK).await;
    fetch(&fixture, &bearer, StatusCode::NOT_FOUND).await;

    let body: Value = store(&fixture, &bearer, wrapped(0x92, 256), StatusCode::OK)
        .await
        .json();
    assert_eq!(body["replaced"], false, "an identity is not an escrow");
    store(&fixture, &bearer, wrapped(0x93, 256), StatusCode::OK).await;

    let fetched = fetch_identity(&fixture, &bearer, StatusCode::OK).await;
    assert_eq!(fetched.bytes().as_ref(), identity.as_slice());
}

#[tokio::test]
async fn a_missing_sealed_identity_has_its_own_code() {
    // An account with an escrow and no sealed identity cannot be recovered as itself, and the
    // remedy (finish the backup from a device that still holds the identity) differs from the
    // one for a missing escrow, so a client must be able to tell them apart.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    store(&fixture, &bearer, wrapped(0xA1, 256), StatusCode::OK).await;

    let problem: Value = fetch_identity(&fixture, &bearer, StatusCode::NOT_FOUND)
        .await
        .json();
    assert_eq!(problem["code"], "error.escrow.identity_not_stored");

    fixture.escrows.set_unavailable(true);
    let problem: Value = fetch_identity(&fixture, &bearer, StatusCode::INTERNAL_SERVER_ERROR)
        .await
        .json();
    assert_eq!(problem["code"], "error.escrow.unavailable");
}

#[tokio::test]
async fn a_sealed_identity_is_scoped_to_its_account_and_refused_like_an_escrow() {
    let fixture = Fixture::working();
    let mine = fixture.bearer().await;
    let theirs = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;

    let held = wrapped(0xB1, 128);
    store_identity(&fixture, &mine, held.clone(), StatusCode::OK).await;
    fetch_identity(&fixture, &theirs, StatusCode::NOT_FOUND).await;

    // Refused exactly as an escrow is, and a refusal is not a deletion.
    let problem: Value = store_identity(&fixture, &mine, Vec::new(), StatusCode::BAD_REQUEST)
        .await
        .json();
    assert_eq!(problem["code"], "error.escrow.malformed");
    let huge = wrapped(0xB2, capsule_server::escrow::MAX_ESCROW_BYTES + 1);
    store_identity(&fixture, &mine, huge, StatusCode::BAD_REQUEST).await;
    let fetched = fetch_identity(&fixture, &mine, StatusCode::OK).await;
    assert_eq!(fetched.bytes().as_ref(), held.as_slice());

    fixture
        .client
        .get(IDENTITY)
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

// ===========================================================================================
// The album key escrows beside it (#741)
// ===========================================================================================

/// Where the album key escrows are listed, and under which each album's is stored.
const ALBUM_KEYS: &str = "/v1/auth/escrow/albums";

/// A canonical album id.
const ALBUM_B: &str = "0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e35";

/// A canonical album id ordering before [`ALBUM_B`].
const ALBUM_A: &str = "0198f3c2-0000-7000-8000-000000000001";

/// Store `blob` as `album`'s key escrow and assert the status.
async fn store_album_keys(
    fixture: &Fixture,
    bearer: &str,
    album: &str,
    blob: Vec<u8>,
    expect: StatusCode,
) -> kynos::test::TestResponse {
    let response = fixture
        .client
        .put(&format!("{ALBUM_KEYS}/{album}"))
        .header("authorization", bearer)
        .header("accept", "application/json")
        .body("application/octet-stream", blob)
        .send()
        .await;
    response.assert_status(expect);
    response
}

/// The caller's album key escrows as `(album_id, blob)`, in the order served.
async fn list_album_keys(fixture: &Fixture, bearer: &str) -> Vec<(String, Vec<u8>)> {
    use base64::Engine as _;
    let response = fixture
        .client
        .get(ALBUM_KEYS)
        .header("authorization", bearer)
        .header("accept", "application/json")
        .send()
        .await;
    response.assert_status(StatusCode::OK);
    let body: Value = response.json();
    body["albums"]
        .as_array()
        .expect("a listing")
        .iter()
        .map(|entry| {
            assert!(entry["stored_at"].as_str().is_some(), "{entry}");
            (
                entry["album_id"].as_str().expect("an album id").to_owned(),
                base64::engine::general_purpose::STANDARD
                    .decode(entry["blob"].as_str().expect("a blob"))
                    .expect("standard base64"),
            )
        })
        .collect()
}

#[tokio::test]
async fn album_key_escrows_list_byte_for_byte_one_per_album_in_album_order() {
    // A recovered device opens each blob under the master key it recovered, so each comes back
    // exactly as stored; a second store for one album replaces only that album's.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    assert!(
        list_album_keys(&fixture, &bearer).await.is_empty(),
        "no album escrowed is an empty listing, not a 404"
    );

    let body: Value = store_album_keys(
        &fixture,
        &bearer,
        ALBUM_B,
        wrapped(0xC1, 200),
        StatusCode::OK,
    )
    .await
    .json();
    assert_eq!(body["album_id"], ALBUM_B);
    assert_eq!(body["replaced"], false);
    let rotated = wrapped(0xC2, 260);
    let body: Value = store_album_keys(&fixture, &bearer, ALBUM_B, rotated.clone(), StatusCode::OK)
        .await
        .json();
    assert_eq!(body["replaced"], true, "one slot per album");
    let other = wrapped(0xC3, 64);
    store_album_keys(&fixture, &bearer, ALBUM_A, other.clone(), StatusCode::OK).await;

    assert_eq!(
        list_album_keys(&fixture, &bearer).await,
        vec![(ALBUM_A.to_owned(), other), (ALBUM_B.to_owned(), rotated)]
    );
    fetch(&fixture, &bearer, StatusCode::NOT_FOUND).await;
    fetch_identity(&fixture, &bearer, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn album_key_escrows_are_scoped_to_their_account_and_refused_with_codes() {
    let fixture = Fixture::working();
    let mine = fixture.bearer().await;
    let theirs = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;
    let held = wrapped(0xD1, 128);
    store_album_keys(&fixture, &mine, ALBUM_A, held.clone(), StatusCode::OK).await;
    assert!(list_album_keys(&fixture, &theirs).await.is_empty());

    // Past the escrow's ceiling is still an album's keys; past their own is refused.
    let large = wrapped(0xD2, capsule_server::escrow::MAX_ESCROW_BYTES + 1);
    store_album_keys(&fixture, &mine, ALBUM_B, large.clone(), StatusCode::OK).await;
    let huge = wrapped(0xD3, capsule_server::escrow::MAX_ALBUM_KEY_ESCROW_BYTES + 1);
    let problem: Value = store_album_keys(&fixture, &mine, ALBUM_A, huge, StatusCode::BAD_REQUEST)
        .await
        .json();
    assert_eq!(problem["code"], "error.escrow.malformed");
    let problem: Value = store_album_keys(
        &fixture,
        &mine,
        ALBUM_A,
        Vec::new(),
        StatusCode::BAD_REQUEST,
    )
    .await
    .json();
    assert_eq!(problem["code"], "error.escrow.malformed");
    for album in ["not-an-album", "0198F3C2-0000-7000-8000-000000000001"] {
        let problem: Value = store_album_keys(
            &fixture,
            &mine,
            album,
            held.clone(),
            StatusCode::BAD_REQUEST,
        )
        .await
        .json();
        assert_eq!(problem["code"], "error.album.invalid_id", "{album}");
    }
    assert_eq!(
        list_album_keys(&fixture, &mine).await,
        vec![(ALBUM_A.to_owned(), held), (ALBUM_B.to_owned(), large)],
        "a refusal is not a deletion"
    );

    fixture.escrows.set_unavailable(true);
    let problem: Value = store_album_keys(
        &fixture,
        &mine,
        ALBUM_A,
        wrapped(0xD4, 8),
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await
    .json();
    assert_eq!(problem["code"], "error.escrow.unavailable");
}
