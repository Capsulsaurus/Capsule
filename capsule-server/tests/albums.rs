//! `POST /v1/albums` — album provisioning (slice `S-C25`), end to end.
//!
//! The slice exists because `capsule push` had nowhere to land: a container album's id is
//! derived from the account master key, the client already knows it, and the server had never
//! heard of it. So the case that matters most here is not any single response — it is
//! `provisioning_makes_an_album_writable`, which asserts that an upload the server refused
//! before provisioning succeeds after it, through the same authority a deployment runs on.

mod support;

use capsule_server::album::authority::ProvisionedAuthority;
use capsule_server::album::{AlbumStore, ProvisionOutcome};
use capsule_server::store::AlbumId;
use capsule_server::upload::{AlbumWriteAccess, WriteAuthority};
use kynos::http::StatusCode;
use serde_json::{Value, json};
use support::{Fixture, PROTOCOL_VERSION, owner};

/// A canonical derived album id.
const DERIVED: &str = "0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e35";

/// The bearer for the seeded account.
async fn token(fixture: &Fixture) -> String {
    format!("Bearer {}", fixture.login().await.access_token)
}

/// Provision `album_id`, asserting the status.
async fn provision(fixture: &Fixture, bearer: &str, body: &Value, expect: StatusCode) -> Value {
    fixture
        .client
        .post("/v1/albums")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .json(body)
        .send()
        .await
        .assert_status(expect)
        .json()
}

// ===========================================================================================

/// The first call creates; every later one is a success that writes nothing.
#[tokio::test]
async fn provisioning_is_idempotent() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;

    let created = provision(
        &fixture,
        &bearer,
        &json!({ "album_id": DERIVED }),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(created["album_id"], DERIVED);
    assert_eq!(created["created"], true);
    assert_eq!(
        created["protocol_version"], PROTOCOL_VERSION,
        "the pin is the server's own, so a client learns it without a second call"
    );

    // The same id arrives from a second device, and again after a recovery.
    for _ in 0..2 {
        let again = provision(
            &fixture,
            &bearer,
            &json!({ "album_id": DERIVED }),
            StatusCode::OK,
        )
        .await;
        assert_eq!(again["created"], false);
        assert_eq!(again["album_id"], created["album_id"]);
        assert_eq!(again["protocol_version"], created["protocol_version"]);
    }
}

/// The point of the slice: an album invariant 6 will actually admit a write to.
///
/// Asserted through [`ProvisionedAuthority`] rather than against the store, because the store
/// holding a row proves nothing on its own — what a write path asks is
/// `album_write_access`, and that is the answer that has to change.
#[tokio::test]
async fn provisioning_makes_an_album_writable() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    let album = AlbumId::new(DERIVED);
    let authority = ProvisionedAuthority::new(fixture.albums.clone(), fixture.directories.clone());

    assert_eq!(
        authority
            .album_write_access(&owner(), &album)
            .await
            .expect("the authority answers"),
        AlbumWriteAccess::Denied,
        "an album the server has never heard of is exactly what `capsule push` used to hit"
    );

    provision(
        &fixture,
        &bearer,
        &json!({ "album_id": DERIVED }),
        StatusCode::CREATED,
    )
    .await;

    assert_eq!(
        authority
            .album_write_access(&owner(), &album)
            .await
            .expect("the authority answers"),
        AlbumWriteAccess::Writable {
            protocol_pin: PROTOCOL_VERSION.to_owned()
        },
        "invariant 6 compares a write against this pin, and it is the server's own"
    );
    assert_eq!(
        fixture
            .albums
            .read(&album)
            .await
            .expect("the store answers")
            .expect("the album is provisioned")
            .owner_id,
        owner(),
    );
}

/// An id bound to another account is refused, and the refusal says nothing.
#[tokio::test]
async fn an_id_bound_elsewhere_is_refused_uninformatively() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    fixture
        .albums
        .provision(capsule_server::album::AlbumRecord {
            album_id: AlbumId::new(DERIVED),
            owner_id: capsule_server::store::OwnerId::new("somebody-else"),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            created_at: jiff::Timestamp::UNIX_EPOCH,
        })
        .await
        .expect("the store accepts");

    let problem = provision(
        &fixture,
        &bearer,
        &json!({ "album_id": DERIVED }),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(problem["code"], "error.album.not_available");
    assert_eq!(
        problem["detail"], "that album id is not available",
        "one fixed message whatever the reason, or the endpoint becomes an existence oracle \
         over other accounts' derived ids"
    );
}

/// Only a canonical UUID is an album id.
#[tokio::test]
async fn a_non_canonical_id_is_refused() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;

    for spelling in [
        "0198F3C2-9C4A-7B3D-8F21-4D7C9A1B2E35",
        "{0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e35}",
        "0198f3c29c4a7b3d8f214d7c9a1b2e35",
        "not-a-uuid",
        "",
    ] {
        let problem = provision(
            &fixture,
            &bearer,
            &json!({ "album_id": spelling }),
            StatusCode::BAD_REQUEST,
        )
        .await;
        assert_eq!(
            problem["code"], "error.album.invalid_id",
            "{spelling:?} round-trips to a different string, so two devices deriving the same \
             album would disagree about its name"
        );
    }
}

/// A name is refused, not silently dropped.
#[tokio::test]
async fn a_body_carrying_a_name_is_refused_rather_than_ignored() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;

    fixture
        .client
        .post("/v1/albums")
        .header("authorization", &bearer)
        .json(&json!({ "album_id": DERIVED, "name": "Holidays" }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    assert_eq!(
        fixture
            .albums
            .read(&AlbumId::new(DERIVED))
            .await
            .expect("the store answers"),
        None,
        "a refused body must not have provisioned the album either",
    );
}

/// Provisioning needs a credential.
#[tokio::test]
async fn provisioning_requires_a_credential() {
    let fixture = Fixture::working();
    fixture
        .client
        .post("/v1/albums")
        .json(&json!({ "album_id": DERIVED }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

/// The store's own answer and the surface's agree.
#[tokio::test]
async fn the_surface_and_the_store_agree_on_what_happened() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    provision(
        &fixture,
        &bearer,
        &json!({ "album_id": DERIVED }),
        StatusCode::CREATED,
    )
    .await;

    // Re-provisioning through the port directly must agree with what the surface said.
    let outcome = fixture
        .albums
        .provision(capsule_server::album::AlbumRecord {
            album_id: AlbumId::new(DERIVED),
            owner_id: owner(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            created_at: jiff::Timestamp::UNIX_EPOCH,
        })
        .await
        .expect("the store answers");
    assert!(matches!(outcome, ProvisionOutcome::AlreadyProvisioned(_)));
}
