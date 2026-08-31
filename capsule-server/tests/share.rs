//! Share links, end to end (slice `S-C4`).
//!
//! The case that carries the slice is `a_link_cannot_be_walked_into_a_blob_it_does_not_name`.
//! design/share-links.md asks the serve path to apply the boundary-crossing privacy strip, and
//! a key-free server cannot — the metadata is ciphertext sealed under material the server does
//! not hold. What it *can* enforce is the property that makes the client's strip stick: a link
//! reaches the addresses its record enumerates and nothing else. That is the assertion.
//!
//! The other load-bearing case is `every_refusal_on_the_public_path_is_one_answer`: not found,
//! revoked, expired, malformed and out-of-scope must be byte-identical, because the opaque id
//! is the entire credential and anything that distinguishes them is an enumeration oracle.

mod support;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_server::blob::{BlobStore, ContentAddress};
use jiff::SignedDuration;
use kynos::http::StatusCode;
use serde_json::{Value, json};
use support::{Fixture, payload};

/// A well-formed opaque id with letters in it.
fn opaque(tag: u8) -> String {
    format!(
        "{:032x}",
        u128::from(tag) | 0xabcd_ef01_2345_6789_abcd_ef01_2345_0000
    )
}

/// Put `bytes` in the store and return their address.
async fn store(fixture: &Fixture, bytes: &[u8]) -> ContentAddress {
    let address = ContentAddress::parse(&support::checksum(bytes)).expect("a content address");
    fixture
        .blobs
        .put(&address, bytes)
        .await
        .expect("the blob store accepts");
    address
}

/// Issue a link over the wire.
async fn issue(fixture: &Fixture, bearer: &str, body: &Value, expect: StatusCode) -> Value {
    fixture
        .client
        .post("/v1/shares")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .json(body)
        .send()
        .await
        .assert_status(expect)
        .json()
}

/// A link over one metadata blob and one original.
async fn live_link(
    fixture: &Fixture,
    bearer: &str,
    tag: u8,
) -> (String, ContentAddress, ContentAddress) {
    let metadata = store(fixture, &payload(b'm', 64)).await;
    let original = store(fixture, &payload(b'o', 512)).await;
    let id = opaque(tag);
    issue(
        fixture,
        bearer,
        &json!({
            "opaque_id": id,
            "metadata_hash": metadata.as_str(),
            "serves": [metadata.as_str(), original.as_str()],
        }),
        StatusCode::CREATED,
    )
    .await;
    (id, metadata, original)
}

/// Fetch a share record with no credential.
async fn fetch(fixture: &Fixture, path: &str, expect: StatusCode) -> kynos::test::TestResponse {
    let response = fixture
        .client
        .get(path)
        .header("accept", "application/json")
        .send()
        .await;
    response.assert_status(expect);
    response
}

#[tokio::test]
async fn a_live_link_serves_its_metadata_and_its_blobs_without_a_credential() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (id, metadata, original) = live_link(&fixture, &bearer, 1).await;

    let body: Value = fetch(&fixture, &format!("/s/{id}"), StatusCode::OK)
        .await
        .json();
    assert_eq!(body["metadata_hash"], metadata.as_str());
    assert_eq!(body["passphrase_protected"], false);
    assert!(
        body.as_object().expect("an object").len() == 2,
        "the record discloses the metadata address and whether a passphrase is needed — nothing \
         about the owner, the album, the scope or the expiry: {body}"
    );

    let bytes = fetch(
        &fixture,
        &format!("/s/{id}/blob/{original}"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(bytes.bytes().as_ref(), payload(b'o', 512).as_slice());
}

#[tokio::test]
async fn a_link_cannot_be_walked_into_a_blob_it_does_not_name() {
    // The enforceable half of the privacy strip. The server cannot redact ciphertext, so what
    // it guarantees instead is that a share reaches exactly the blobs the issuing client
    // prepared for the boundary crossing — never the album's unstripped metadata, even though
    // the same store holds it and the same server serves it elsewhere.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (id, _, _) = live_link(&fixture, &bearer, 1).await;

    let unstripped = store(&fixture, &payload(b'u', 128)).await;
    assert!(
        fixture
            .blobs
            .stat(&unstripped)
            .await
            .expect("stat")
            .is_some(),
        "the bytes are right there, which is what makes the refusal about the link rather than \
         about the store"
    );

    fetch(
        &fixture,
        &format!("/s/{id}/blob/{unstripped}"),
        StatusCode::NOT_FOUND,
    )
    .await;
}

#[tokio::test]
async fn every_refusal_on_the_public_path_is_one_answer() {
    // The opaque id is the entire credential, so anything that distinguishes these is an
    // enumeration oracle. Compared as whole bodies rather than by status, because a differing
    // `detail` would be just as much of a signal.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    // Expired.
    let metadata = store(&fixture, &payload(b'm', 64)).await;
    let expired = opaque(2);
    issue(
        &fixture,
        &bearer,
        &json!({
            "opaque_id": expired,
            "metadata_hash": metadata.as_str(),
            "serves": [metadata.as_str()],
            "expires_at": "1970-01-01T00:00:01Z",
        }),
        StatusCode::CREATED,
    )
    .await;
    fixture.clock.advance(SignedDuration::from_hours(1));
    // The clock moved past the access token's own fifteen minutes, so the owner signs in again
    // before the operations that need a credential.
    let bearer = fixture.bearer().await;

    // Revoked.
    let (revoked, _, _) = live_link(&fixture, &bearer, 3).await;
    fixture
        .client
        .delete(&format!("/v1/shares/{revoked}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // And a live link, for the out-of-scope blob case.
    let (live, _, _) = live_link(&fixture, &bearer, 4).await;
    let stranger = store(&fixture, &payload(b'x', 32)).await;

    let mut bodies = Vec::new();
    for path in [
        format!("/s/{}", opaque(9)),          // never existed
        format!("/s/{expired}"),              // expired
        format!("/s/{revoked}"),              // revoked
        "/s/not-an-opaque-id".to_owned(),     // malformed
        format!("/s/{live}/blob/{stranger}"), // a blob the link does not serve
        format!("/s/{}/wrapped-secret", opaque(9)),
    ] {
        let body: Value = fetch(&fixture, &path, StatusCode::NOT_FOUND).await.json();
        bodies.push(body);
    }

    for body in &bodies[1..] {
        assert_eq!(
            body, &bodies[0],
            "the public path's refusals must be byte-identical, and `410` must never appear: \
             {body}"
        );
    }
}

#[tokio::test]
async fn a_passphrase_protected_link_serves_the_wrapped_material_and_never_the_passphrase() {
    // The server is not in the password-trust path: it stores and returns wrapped bytes, and
    // unwrap is client-side. There is no request shape here that carries a passphrase, which is
    // what makes that structural rather than a promise.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let metadata = store(&fixture, &payload(b'm', 64)).await;
    let wrapped = payload(b'w', 96);
    let id = opaque(5);

    issue(
        &fixture,
        &bearer,
        &json!({
            "opaque_id": id,
            "metadata_hash": metadata.as_str(),
            "serves": [metadata.as_str()],
            "wrapped_secret": BASE64.encode(&wrapped),
        }),
        StatusCode::CREATED,
    )
    .await;

    let body: Value = fetch(&fixture, &format!("/s/{id}"), StatusCode::OK)
        .await
        .json();
    assert_eq!(body["passphrase_protected"], true);

    let served = fetch(&fixture, &format!("/s/{id}/wrapped-secret"), StatusCode::OK).await;
    assert_eq!(
        served.bytes().as_ref(),
        wrapped.as_slice(),
        "wrapped material is opaque and comes back byte for byte; a re-encoded wrap is one that \
         no longer opens"
    );
}

#[tokio::test]
async fn a_link_without_a_passphrase_has_no_wrapped_secret_to_serve() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (id, _, _) = live_link(&fixture, &bearer, 6).await;

    fetch(
        &fixture,
        &format!("/s/{id}/wrapped-secret"),
        StatusCode::NOT_FOUND,
    )
    .await;
}

#[tokio::test]
async fn revocation_takes_effect_immediately_and_is_only_the_owners() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (id, _, original) = live_link(&fixture, &bearer, 7).await;
    let stranger = fixture.other_bearer("01937b7c-0000-7000-8000-0000000000ff");

    // A stranger's revoke answers the same 204 — saying "there was nothing to revoke" would be
    // a lookup — and changes nothing.
    fixture
        .client
        .delete(&format!("/v1/shares/{id}"))
        .header("authorization", &stranger)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    fetch(&fixture, &format!("/s/{id}"), StatusCode::OK).await;

    fixture
        .client
        .delete(&format!("/v1/shares/{id}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    fetch(&fixture, &format!("/s/{id}"), StatusCode::NOT_FOUND).await;
    fetch(
        &fixture,
        &format!("/s/{id}/blob/{original}"),
        StatusCode::NOT_FOUND,
    )
    .await;
    assert!(
        fixture.blobs.stat(&original).await.expect("stat").is_some(),
        "revoking a link is a serving constraint; the owner's bytes stay"
    );
}

#[tokio::test]
async fn an_unreachable_store_refuses_rather_than_answering_not_found() {
    // Fail-closed, and the one place this surface distinguishes anything: answering `404` here
    // would be indistinguishable from "revoked" to a client that would then stop retrying a
    // link that is perfectly good.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (id, _, _) = live_link(&fixture, &bearer, 8).await;
    fixture.shares.set_unavailable(true);

    let problem: Value = fetch(
        &fixture,
        &format!("/s/{id}"),
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await
    .json();
    assert_eq!(problem["code"], "error.share.unavailable");
}

#[tokio::test]
async fn a_link_that_cannot_serve_its_own_metadata_is_refused_at_issue() {
    // Otherwise every viewer's first request is a 404 the owner has no way to diagnose.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let metadata = store(&fixture, &payload(b'm', 64)).await;
    let other = store(&fixture, &payload(b'o', 64)).await;

    let problem = issue(
        &fixture,
        &bearer,
        &json!({
            "opaque_id": opaque(1),
            "metadata_hash": metadata.as_str(),
            "serves": [other.as_str()],
        }),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(problem["code"], "error.share.malformed");
}

#[tokio::test]
async fn a_structured_or_short_opaque_id_is_refused_at_issue() {
    // 128 bits is the defense against enumeration *independent of rate limiting*, which this
    // port does not have. A UUIDv7 would cut real entropy to about 62 bits.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let metadata = store(&fixture, &payload(b'm', 64)).await;

    for bad in [
        "0123456789abcdef",
        "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f",
        &opaque(1).to_uppercase(),
    ] {
        let problem = issue(
            &fixture,
            &bearer,
            &json!({
                "opaque_id": bad,
                "metadata_hash": metadata.as_str(),
                "serves": [metadata.as_str()],
            }),
            StatusCode::BAD_REQUEST,
        )
        .await;
        assert_eq!(problem["code"], "error.share.malformed");
    }
}

#[tokio::test]
async fn issuing_and_revoking_need_a_credential_and_serving_does_not() {
    let fixture = Fixture::working();
    fixture
        .client
        .post("/v1/shares")
        .json(&json!({ "opaque_id": opaque(1), "metadata_hash": "x", "serves": [] }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    fixture
        .client
        .delete(&format!("/v1/shares/{}", opaque(1)))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    // And the public path answers on its own merits, with no credential in sight.
    fetch(
        &fixture,
        &format!("/s/{}", opaque(1)),
        StatusCode::NOT_FOUND,
    )
    .await;
}

#[tokio::test]
async fn the_public_path_is_rate_limited_per_link_across_all_three_operations() {
    // Enumeration does not care which of the three endpoints it probes with, so all three
    // charge the same budget. And the refusal is a `429` rather than the indistinguishable
    // `404`: a `404` that was really a throttle would teach a legitimate viewer that a live
    // link is dead.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (id, _, original) = live_link(&fixture, &bearer, 1).await;

    // Spend the budget across a mix of the three, so no single one is carrying the count.
    for step in 0..60 {
        let path = match step % 3 {
            0 => format!("/s/{id}"),
            1 => format!("/s/{id}/wrapped-secret"),
            _ => format!("/s/{id}/blob/{original}"),
        };
        fixture.client.get(&path).send().await;
    }

    for path in [
        format!("/s/{id}"),
        format!("/s/{id}/wrapped-secret"),
        format!("/s/{id}/blob/{original}"),
    ] {
        let problem: Value = fetch(&fixture, &path, StatusCode::TOO_MANY_REQUESTS)
            .await
            .json();
        assert_eq!(problem["code"], "error.share.rate_limited");
    }

    // A different link has its own budget: one probed link must not take every other share on
    // the server down with it.
    let (other, _, _) = live_link(&fixture, &bearer, 2).await;
    fetch(&fixture, &format!("/s/{other}"), StatusCode::OK).await;
}

#[tokio::test]
async fn probing_a_link_that_does_not_exist_still_costs_the_prober() {
    // Charged before the link is resolved. A limiter that only ran for real links would be a
    // free oracle for every id that is not one.
    let fixture = Fixture::working();
    let unknown = opaque(9);

    for _ in 0..60 {
        fixture.client.get(&format!("/s/{unknown}")).send().await;
    }
    fetch(
        &fixture,
        &format!("/s/{unknown}"),
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await;
}
