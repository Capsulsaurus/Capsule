//! `PUT /v1/albums/{album_id}/roster` — publishing an album's membership roster (slice `S-C51`).
//!
//! The one way the key-free server learns who may read and write a shared album. What these
//! cases pin is the trust anchor and the disclosure boundary: only the album owner's account
//! publishes, only a live device in the owner's published directory attests, a member who tries
//! learns nothing, and a roster that does not supersede the held one changes nothing.

mod support;

use capsule_core::crypto::keys::HybridSigningKey;
use capsule_server::membership::{MemberRole, Membership, MembershipStore as _, Revocation};
use capsule_server::routes::roster::MAX_ROSTER_BYTES;
use capsule_server::store::{AlbumId, UserId};
use kynos::http::StatusCode;
use serde_json::{Value, json};
use support::{
    Fixture, album, device, identity_header, identity_key, second_album,
    signed_directory_with_device, signed_roster,
};

/// The member every roster here names.
const BOB: &str = "01937b7c-0000-7000-8000-0000000000b0";

/// Publish a directory holding the seeded device with `dsk` as its signing key.
async fn anchor(fixture: &Fixture, bearer: &str, ik: &HybridSigningKey, dsk: &HybridSigningKey) {
    fixture
        .client
        .post("/v1/auth/devices/directory")
        .header("authorization", bearer)
        .header("x-capsule-identity-key", &identity_header(ik))
        .body(
            "application/cbor",
            signed_directory_with_device(ik, 1, device(), dsk, "1970-01-01T00:00:00Z"),
        )
        .send()
        .await
        .assert_status(StatusCode::OK);
}

/// Provision `id` for the seeded account.
async fn provision(fixture: &Fixture, bearer: &str, id: &AlbumId) {
    fixture
        .client
        .post("/v1/albums")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .json(&json!({ "album_id": id.as_str() }))
        .send()
        .await
        .assert_status(StatusCode::CREATED);
}

/// A fixture with the seeded account anchored on `dsk` and the seeded album provisioned.
async fn ready(dsk: &HybridSigningKey) -> (Fixture, String) {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    anchor(&fixture, &bearer, &identity_key(), dsk).await;
    provision(&fixture, &bearer, &album()).await;
    (fixture, bearer)
}

/// PUT `roster_cbor` for `id` as `bearer`.
async fn publish(
    fixture: &Fixture,
    bearer: &str,
    id: &AlbumId,
    roster_cbor: &str,
) -> kynos::test::TestResponse {
    fixture
        .client
        .put(&format!("/v1/albums/{id}/roster"))
        .header("authorization", bearer)
        .header("accept", "application/json")
        .json(&json!({ "roster_cbor": roster_cbor }))
        .send()
        .await
}

/// What the store says `user` is to the seeded album.
async fn membership_of(fixture: &Fixture, user: &str) -> Membership {
    fixture
        .members
        .membership(&album(), &UserId::new(user))
        .await
        .expect("the store answers")
}

// ===========================================================================================

#[tokio::test]
async fn the_owner_publishes_a_roster_and_its_members_become_members() {
    let dsk = identity_key();
    let (fixture, bearer) = ready(&dsk).await;

    let body: Value = publish(
        &fixture,
        &bearer,
        &album(),
        &signed_roster(&dsk, device(), &album(), 1, 1, &[(BOB, MemberRole::Writer)]),
    )
    .await
    .assert_status(StatusCode::OK)
    .json();
    assert_eq!(body["album_id"], album().as_str());
    assert_eq!(body["roster_version"], 1);
    assert_eq!(body["amk_epoch"], 1);
    assert_eq!(body["member_count"], 1);
    assert_eq!(body["replayed"], false);
    assert_eq!(
        membership_of(&fixture, BOB).await,
        Membership::Member {
            role: MemberRole::Writer,
            granted_epoch: 1,
        }
    );
}

#[tokio::test]
async fn the_same_roster_again_is_a_replay_and_a_lower_version_is_stale() {
    let dsk = identity_key();
    let (fixture, bearer) = ready(&dsk).await;
    let first = signed_roster(&dsk, device(), &album(), 2, 1, &[(BOB, MemberRole::Reader)]);
    publish(&fixture, &bearer, &album(), &first)
        .await
        .assert_status(StatusCode::OK);

    // Identical bytes: idempotent under `(album_id, roster_version)`.
    let replayed: Value = publish(&fixture, &bearer, &album(), &first)
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(replayed["replayed"], true);
    assert_eq!(replayed["roster_version"], 2);

    // Same version, different bytes — the loser of a concurrent publish — and a lower version.
    for stale in [
        signed_roster(&dsk, device(), &album(), 2, 1, &[]),
        signed_roster(&dsk, device(), &album(), 1, 1, &[]),
    ] {
        let problem: Value = publish(&fixture, &bearer, &album(), &stale)
            .await
            .assert_status(StatusCode::CONFLICT)
            .json();
        assert_eq!(problem["code"], "error.album.roster_stale");
        assert_eq!(problem["current_version"], 2);
    }
    assert_eq!(
        membership_of(&fixture, BOB).await,
        Membership::Member {
            role: MemberRole::Reader,
            granted_epoch: 1,
        },
        "a refused roster changes nothing"
    );
}

#[tokio::test]
async fn an_epoch_that_regresses_is_stale_too() {
    let dsk = identity_key();
    let (fixture, bearer) = ready(&dsk).await;
    publish(
        &fixture,
        &bearer,
        &album(),
        &signed_roster(&dsk, device(), &album(), 1, 3, &[(BOB, MemberRole::Writer)]),
    )
    .await
    .assert_status(StatusCode::OK);

    let problem: Value = publish(
        &fixture,
        &bearer,
        &album(),
        &signed_roster(&dsk, device(), &album(), 2, 2, &[]),
    )
    .await
    .assert_status(StatusCode::CONFLICT)
    .json();
    assert_eq!(problem["code"], "error.album.roster_stale");
    assert_eq!(
        problem["current_version"], 1,
        "the held version, so the client's action is the same re-sync a stale version asks for"
    );
    assert!(matches!(
        membership_of(&fixture, BOB).await,
        Membership::Member { .. }
    ));
}

#[tokio::test]
async fn a_member_omitted_from_a_later_roster_is_revoked_at_its_version_and_epoch() {
    let dsk = identity_key();
    let (fixture, bearer) = ready(&dsk).await;
    publish(
        &fixture,
        &bearer,
        &album(),
        &signed_roster(&dsk, device(), &album(), 1, 1, &[(BOB, MemberRole::Writer)]),
    )
    .await
    .assert_status(StatusCode::OK);

    // Removal is a new roster that omits the member; the MLS `Remove`'s epoch bump rides along.
    let body: Value = publish(
        &fixture,
        &bearer,
        &album(),
        &signed_roster(&dsk, device(), &album(), 2, 2, &[]),
    )
    .await
    .assert_status(StatusCode::OK)
    .json();
    assert_eq!(body["member_count"], 0);
    assert_eq!(
        membership_of(&fixture, BOB).await,
        Membership::Revoked(Revocation {
            at_version: 2,
            at_epoch: 2,
        })
    );
}

#[tokio::test]
async fn a_roster_not_attested_by_a_live_owner_device_is_refused() {
    let dsk = identity_key();
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    provision(&fixture, &bearer, &album()).await;
    let roster = signed_roster(&dsk, device(), &album(), 1, 1, &[(BOB, MemberRole::Writer)]);

    // No published directory: nothing can vouch for the attester.
    let problem: Value = publish(&fixture, &bearer, &album(), &roster)
        .await
        .assert_status(StatusCode::FORBIDDEN)
        .json();
    assert_eq!(problem["code"], "error.album.roster_attester");

    // A directory that holds the device under a *different* key: the signature does not verify.
    anchor(&fixture, &bearer, &identity_key(), &identity_key()).await;
    let problem: Value = publish(&fixture, &bearer, &album(), &roster)
        .await
        .assert_status(StatusCode::FORBIDDEN)
        .json();
    assert_eq!(problem["code"], "error.album.roster_attester");
    assert_eq!(membership_of(&fixture, BOB).await, Membership::Never);
}

#[tokio::test]
async fn a_roster_attested_for_another_account_is_refused_even_by_the_owner() {
    // The owner's token, the owner's album, the owner's device key — but the document says it
    // was attested by somebody else. The anchor is the *caller's* directory, so the document
    // must name the caller.
    let dsk = identity_key();
    let (fixture, bearer) = ready(&dsk).await;
    let forged = {
        use capsule_core::crypto::keys::AmkVersion;
        use capsule_core::crypto::membership::AlbumRoster;
        AlbumRoster {
            album_id: uuid::Uuid::parse_str(album().as_str()).expect("a uuid"),
            roster_version: 1,
            amk_epoch: AmkVersion(1),
            attested_by_user: uuid::Uuid::parse_str(BOB).expect("a uuid"),
            attested_by_device: device(),
            attested_at: "2026-09-02T00:00:00Z".to_owned(),
            members: vec![],
        }
    };
    let signed =
        capsule_core::crypto::membership::SignedAlbumRoster::sign(forged, &dsk).expect("signs");
    let encoded = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .encode(capsule_core::cbor::to_canonical_vec(&signed).expect("encodes"))
    };
    let problem: Value = publish(&fixture, &bearer, &album(), &encoded)
        .await
        .assert_status(StatusCode::FORBIDDEN)
        .json();
    assert_eq!(problem["code"], "error.album.roster_attester");
}

#[tokio::test]
async fn a_member_cannot_publish_the_owners_roster_and_learns_nothing_trying() {
    let dsk = identity_key();
    let (fixture, bearer) = ready(&dsk).await;
    publish(
        &fixture,
        &bearer,
        &album(),
        &signed_roster(&dsk, device(), &album(), 1, 1, &[(BOB, MemberRole::Writer)]),
    )
    .await
    .assert_status(StatusCode::OK);

    // Bob is a writer member, and gets the album ceremonies' answer: not yours is not found —
    // the same body an album nobody provisioned gets.
    let bob = fixture.other_bearer(BOB).await;
    let roster = signed_roster(&dsk, device(), &album(), 2, 1, &[]);
    let as_member: Value = publish(&fixture, &bob, &album(), &roster)
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .json();
    assert_eq!(as_member["code"], "error.album.roster_not_found");
    let unprovisioned: Value = publish(
        &fixture,
        &bob,
        &second_album(),
        &signed_roster(&dsk, device(), &second_album(), 2, 1, &[]),
    )
    .await
    .assert_status(StatusCode::NOT_FOUND)
    .json();
    assert_eq!(
        as_member, unprovisioned,
        "one answer for not-yours and not-there"
    );
    assert!(matches!(
        membership_of(&fixture, BOB).await,
        Membership::Member { .. }
    ));
}

#[tokio::test]
async fn a_malformed_roster_is_refused_before_anything_is_read() {
    let dsk = identity_key();
    let (fixture, bearer) = ready(&dsk).await;

    let malformed = [
        // Not base64 at all.
        "not base64!".to_owned(),
        // Base64 of something that is not a signed roster.
        {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(b"not a roster")
        },
        // Past the size ceiling, refused before decoding.
        {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(vec![0u8; MAX_ROSTER_BYTES + 1])
        },
        // A valid signed roster with a trailing byte: verifiable, but not the canonical bytes.
        {
            use base64::Engine as _;
            let mut bytes = base64::engine::general_purpose::STANDARD
                .decode(signed_roster(&dsk, device(), &album(), 1, 1, &[]))
                .expect("the helper emits base64");
            bytes.push(0);
            base64::engine::general_purpose::STANDARD.encode(bytes)
        },
        // For a different album than the path.
        signed_roster(&dsk, device(), &second_album(), 1, 1, &[]),
        // Lists the owner.
        signed_roster(
            &dsk,
            device(),
            &album(),
            1,
            1,
            &[(support::user().as_str(), MemberRole::Reader)],
        ),
        // Lists an account twice.
        signed_roster(
            &dsk,
            device(),
            &album(),
            1,
            1,
            &[(BOB, MemberRole::Reader), (BOB, MemberRole::Writer)],
        ),
    ];
    for roster in malformed {
        let problem: Value = publish(&fixture, &bearer, &album(), &roster)
            .await
            .assert_status(StatusCode::BAD_REQUEST)
            .json();
        assert_eq!(problem["code"], "error.album.roster_malformed", "{problem}");
    }
    assert!(
        fixture
            .members
            .current_roster(&album())
            .await
            .expect("the store answers")
            .is_none(),
        "nothing was written"
    );
}

#[tokio::test]
async fn a_store_that_cannot_answer_is_an_outage_never_a_refusal() {
    let dsk = identity_key();
    let (fixture, bearer) = ready(&dsk).await;
    let roster = signed_roster(&dsk, device(), &album(), 1, 1, &[(BOB, MemberRole::Writer)]);

    for (down, up) in [
        (
            Box::new(|| fixture.members.set_unavailable(true)) as Box<dyn Fn()>,
            Box::new(|| fixture.members.set_unavailable(false)) as Box<dyn Fn()>,
        ),
        (
            Box::new(|| fixture.albums.set_unavailable(true)),
            Box::new(|| fixture.albums.set_unavailable(false)),
        ),
        (
            Box::new(|| fixture.directories.set_unavailable(true)),
            Box::new(|| fixture.directories.set_unavailable(false)),
        ),
    ] {
        down();
        let problem: Value = publish(&fixture, &bearer, &album(), &roster)
            .await
            .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
            .json();
        assert_eq!(problem["code"], "error.album.unavailable");
        up();
    }
    assert_eq!(membership_of(&fixture, BOB).await, Membership::Never);
}

#[tokio::test]
async fn a_publish_without_the_protocol_handshake_is_refused_by_the_gate() {
    // The roster route is a write inside the gated group: a client that does not say which
    // protocol it speaks is turned away before the body is read.
    let dsk = identity_key();
    let (fixture, bearer) = ready(&dsk).await;
    fixture
        .client
        .raw()
        .put(&format!("/v1/albums/{}/roster", album()))
        .header("authorization", &bearer)
        .json(&json!({
            "roster_cbor": signed_roster(&dsk, device(), &album(), 1, 1, &[]),
        }))
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    assert!(
        fixture
            .members
            .current_roster(&album())
            .await
            .expect("the store answers")
            .is_none()
    );
}
