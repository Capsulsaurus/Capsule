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

/// The address `bytes` would be stored under, without storing them.
fn address_of(bytes: &[u8]) -> ContentAddress {
    ContentAddress::parse(&support::checksum(bytes)).expect("a content address")
}

/// A link over one metadata record and one original.
///
/// The original is put straight into the store, standing in for an asset upload. The record
/// travels in the issue request, as a client sends it (#653).
async fn live_link(
    fixture: &Fixture,
    bearer: &str,
    tag: u8,
) -> (String, ContentAddress, ContentAddress) {
    let record = payload(b'm', 64);
    let metadata = address_of(&record);
    let original = store(fixture, &payload(b'o', 512)).await;
    let id = opaque(tag);
    issue(
        fixture,
        bearer,
        &json!({
            "opaque_id": id,
            "metadata_hash": metadata.as_str(),
            "serves": [metadata.as_str(), original.as_str()],
            "metadata_blob": BASE64.encode(&record),
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

/// Lowercase hex, the encoding the browser surface takes its URL components in.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// #614, end to end: a link built by the issuer's own `publish_share`, registered through
/// `POST /v1/shares`, served by the three public routes, and opened through the very
/// `capsule-wasm` entry points the browser viewer calls — the metadata record, then the scope
/// (from the record for a link-only link, from `wrapped-secret` for a passphrase link), then every
/// covered asset's ciphertext. Before #614 the viewer read a shape no server produced, so this
/// sequence could not complete for any link.
#[tokio::test]
async fn a_published_link_opens_through_the_browser_surface_from_what_the_server_serves() {
    use capsule_core::crypto::encryption::{
        NONCE_PREFIX_LEN, encrypt_asset_vec_with_prefix, seal_file_key,
    };
    use capsule_core::crypto::hash::hash_bytes;
    use capsule_core::crypto::keys::Amk;
    use capsule_core::crypto::primitives::Argon2Params;
    use capsule_core::crypto::provenance::{KeyMode, WrappedFileKey};
    use capsule_core::sharing::{
        LINK_SECRET_LEN, ScopeMaterial, SharedAsset, encapsulate_scope, generate_opaque_id,
        publish_share,
    };

    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let fast = Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    };
    let plaintext = payload(b'p', 5000);

    // An album holding a derived asset and an adopted drop, shared without a passphrase; and a
    // single asset shared behind one. Between them: both scope kinds, both key modes, and both
    // places a scope can travel.
    let amk_bytes = [0x3C; 32];
    let amk = Amk::from_bytes(amk_bytes);
    let derived_id = uuid::Uuid::now_v7();
    let derived_prefix = [0x0D; NONCE_PREFIX_LEN];
    let derived_key = amk.derive_file_key(&derived_id, &derived_prefix);
    let dropped_id = uuid::Uuid::now_v7();
    let dropped_prefix = [0x0F; NONCE_PREFIX_LEN];
    let guest_key = [0x6B; 32];
    let single_id = uuid::Uuid::now_v7();
    let single_prefix = [0x07; NONCE_PREFIX_LEN];
    let single_key = [0x42; 32];

    let cases = [
        (
            ScopeMaterial::Album {
                album_id: uuid::Uuid::now_v7(),
                amks: std::collections::BTreeMap::from([(3, amk_bytes)]),
            },
            None,
            vec![
                (derived_id, derived_prefix, derived_key, 3, None),
                (
                    dropped_id,
                    dropped_prefix,
                    guest_key,
                    3,
                    Some(seal_file_key(&amk, &dropped_id, &guest_key)),
                ),
            ],
        ),
        (
            ScopeMaterial::Asset {
                file_id: single_id,
                file_key: single_key,
            },
            Some("correct horse battery staple"),
            vec![(single_id, single_prefix, single_key, 0, None)],
        ),
    ];

    for (material, passphrase, assets) in cases {
        let mut shared = Vec::new();
        for (asset_id, nonce_prefix, key, amk_version, wrapped_file_key) in assets {
            let (_, ciphertext) = encrypt_asset_vec_with_prefix(&key, nonce_prefix, &plaintext);
            store(&fixture, &ciphertext).await;
            shared.push(SharedAsset {
                asset_id,
                content_hash: hash_bytes(&ciphertext),
                content_type: "image/png".to_owned(),
                size: plaintext.len() as u64,
                amk_version,
                nonce_prefix,
                key_mode: if wrapped_file_key.is_some() {
                    KeyMode::Wrapped
                } else {
                    KeyMode::Derived
                },
                wrapped_file_key: wrapped_file_key.map(WrappedFileKey),
            });
        }

        // The issuer's side: encapsulate, publish, upload the record, register the link.
        let opaque_id = generate_opaque_id();
        let fragment = capsule_core::crypto::rng::random_array::<LINK_SECRET_LEN>();
        let wrapped = encapsulate_scope(&material, &fragment, &opaque_id, passphrase, fast)
            .expect("encapsulate");
        let publication =
            publish_share(&wrapped, &fragment, &opaque_id, None, shared.clone()).expect("publish");
        let id = hex(&opaque_id);
        // The record crosses the wire in the issue request; nothing puts it in the store but
        // the server (#653).
        let mut body = json!({
            "opaque_id": id,
            "metadata_hash": publication.metadata_hash.to_hex(),
            "serves": publication.serves.iter().map(capsule_core::crypto::hash::Hash32::to_hex).collect::<Vec<_>>(),
            "metadata_blob": BASE64.encode(&publication.metadata_blob),
        });
        if let Some(secret) = &publication.wrapped_secret {
            body["wrapped_secret"] = json!(BASE64.encode(secret));
        }
        issue(&fixture, &bearer, &body, StatusCode::CREATED).await;

        // The viewer's side, with nothing but the URL.
        let served: Value = fetch(&fixture, &format!("/s/{id}"), StatusCode::OK)
            .await
            .json();
        assert_eq!(served["passphrase_protected"], passphrase.is_some());
        let metadata_hash = served["metadata_hash"].as_str().expect("a hash");
        let record = fetch(
            &fixture,
            &format!("/s/{id}/blob/{metadata_hash}"),
            StatusCode::OK,
        )
        .await;
        let view = capsule_wasm::open_share_metadata_wasm(record.bytes(), &id, &hex(&fragment))
            .expect("the served record opens with the URL alone");

        let scope = if passphrase.is_some() {
            assert_eq!(
                view.link_scope_b64().expect("projects"),
                None,
                "a passphrase link's scope must not be in the fragment-sealed record"
            );
            let secret = fetch(&fixture, &format!("/s/{id}/wrapped-secret"), StatusCode::OK).await;
            BASE64.encode(secret.bytes())
        } else {
            fetch(
                &fixture,
                &format!("/s/{id}/wrapped-secret"),
                StatusCode::NOT_FOUND,
            )
            .await;
            view.link_scope_b64()
                .expect("projects")
                .expect("a link-only record carries its scope")
        };
        let scope =
            capsule_wasm::open_share(&scope, &id, &hex(&fragment), passphrase.map(str::to_owned))
                .expect("the scope opens");

        let listed = view.assets();
        assert_eq!(listed.len(), shared.len());
        for asset in listed {
            let ciphertext = fetch(
                &fixture,
                &format!("/s/{id}/blob/{}", asset.content_hash()),
                StatusCode::OK,
            )
            .await;
            let opened = scope
                .decrypt_blob(
                    &asset.asset_id(),
                    asset.amk_version(),
                    &asset.nonce_prefix_hex(),
                    ciphertext.bytes(),
                    asset.wrapped_file_key_hex(),
                )
                .expect("every covered asset decrypts");
            assert_eq!(opened, plaintext, "{} asset", asset.key_mode());
        }
    }
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
    let stranger = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;

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
async fn a_held_opaque_id_cannot_be_reissued_and_the_refusal_is_one_answer() {
    // #553: every recipient of a link knows its opaque id. Were a re-issue a replace, any
    // account could take the link over and re-point it at its own blobs.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (id, metadata, original) = live_link(&fixture, &bearer, 8).await;
    let stranger = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;
    let hostile = store(&fixture, &payload(b'h', 64)).await;
    let takeover = json!({
        "opaque_id": id,
        "metadata_hash": hostile.as_str(),
        "serves": [hostile.as_str()],
    });

    let from_stranger = issue(&fixture, &stranger, &takeover, StatusCode::BAD_REQUEST).await;
    let from_owner = issue(&fixture, &bearer, &takeover, StatusCode::BAD_REQUEST).await;
    assert_eq!(
        from_stranger, from_owner,
        "the refusal must not say whose the id is"
    );
    assert_eq!(from_stranger["code"], "error.share.malformed");

    let body: Value = fetch(&fixture, &format!("/s/{id}"), StatusCode::OK)
        .await
        .json();
    assert_eq!(body["metadata_hash"], metadata.as_str());
    fetch(
        &fixture,
        &format!("/s/{id}/blob/{original}"),
        StatusCode::OK,
    )
    .await;
    fetch(
        &fixture,
        &format!("/s/{id}/blob/{hostile}"),
        StatusCode::NOT_FOUND,
    )
    .await;
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

/// #653: the server stores only the record `metadata_hash` names, within its bound.
#[tokio::test]
async fn a_metadata_record_that_is_not_the_named_one_is_refused_and_nothing_is_stored() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let record = payload(b'm', 64);
    let metadata = address_of(&record);
    let oversized = vec![0x5A; capsule_server::share::MAX_SHARE_METADATA_BYTES + 1];
    let oversized_address = address_of(&oversized);

    for (tag, hash, blob) in [
        // Another record's bytes under this one's address.
        (1, &metadata, BASE64.encode(payload(b'x', 64))),
        // Not base64 at all.
        (2, &metadata, "not base64!".to_owned()),
        // The right hash, and one byte past the bound.
        (3, &oversized_address, BASE64.encode(&oversized)),
    ] {
        let problem = issue(
            &fixture,
            &bearer,
            &json!({
                "opaque_id": opaque(tag),
                "metadata_hash": hash.as_str(),
                "serves": [hash.as_str()],
                "metadata_blob": blob,
            }),
            StatusCode::BAD_REQUEST,
        )
        .await;
        assert_eq!(problem["code"], "error.share.malformed", "{problem}");
        assert!(
            fixture.blobs.stat(hash).await.expect("stat").is_none(),
            "a refused record must not reach the store"
        );
        fetch(
            &fixture,
            &format!("/s/{}", opaque(tag)),
            StatusCode::NOT_FOUND,
        )
        .await;
    }
}

/// #653: the record is metadata growth, refused past the quota grace window like any other.
#[tokio::test]
async fn a_metadata_record_from_an_account_past_its_grace_window_is_refused() {
    use capsule_server::quota::{DEFAULT_GRACE_WINDOW, QuotaLimits, QuotaStore as _};
    use capsule_server::store::Clock as _;

    let limits = QuotaLimits::new(4096, 12_288, DEFAULT_GRACE_WINDOW);
    let fixture = Fixture::with_quota(limits);
    fixture
        .quotas
        .charge(
            &capsule_server::store::UserId::new(support::user().as_str()),
            &address_of(b"pre-existing storage"),
            20_000,
            fixture.clock.now(),
            limits,
        )
        .await
        .expect("the ledger accepts");
    fixture
        .clock
        .advance(DEFAULT_GRACE_WINDOW + SignedDuration::from_hours(1));
    let bearer = fixture.bearer().await;

    let record = payload(b'm', 64);
    let metadata = address_of(&record);
    let problem = issue(
        &fixture,
        &bearer,
        &json!({
            "opaque_id": opaque(1),
            "metadata_hash": metadata.as_str(),
            "serves": [metadata.as_str()],
            "metadata_blob": BASE64.encode(&record),
        }),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(problem["code"], "error.quota.grace_locked");
    assert!(fixture.blobs.stat(&metadata).await.expect("stat").is_none());
    fetch(
        &fixture,
        &format!("/s/{}", opaque(1)),
        StatusCode::NOT_FOUND,
    )
    .await;

    // A link that carries no record is refused too: past the grace window every new share
    // link is (quota.md, "Grace expired"), not only one that brings its bytes along.
    let held = store(&fixture, &payload(b'h', 64)).await;
    let problem = issue(
        &fixture,
        &bearer,
        &json!({
            "opaque_id": opaque(2),
            "metadata_hash": held.as_str(),
            "serves": [held.as_str()],
        }),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(problem["code"], "error.quota.grace_locked");
    fetch(
        &fixture,
        &format!("/s/{}", opaque(2)),
        StatusCode::NOT_FOUND,
    )
    .await;
}

/// #653: a record whose new bytes would reach the hard limit is refused, like an upload of them.
///
/// The bound on the grace window: without it an account over its limit could store any number
/// of distinct records until the window closed. Bytes the ledger already attributes add
/// nothing, so a second link over the same record is still admitted.
#[tokio::test]
async fn a_metadata_record_that_would_cross_the_hard_limit_is_refused() {
    use capsule_server::quota::{DEFAULT_GRACE_WINDOW, QuotaLimits, QuotaStore as _};
    use capsule_server::store::Clock as _;

    let limits = QuotaLimits::new(4096, 12_288, DEFAULT_GRACE_WINDOW);
    let fixture = Fixture::with_quota(limits);
    let owner = capsule_server::store::UserId::new(support::user().as_str());
    fixture
        .quotas
        .charge(
            &owner,
            &address_of(b"pre-existing storage"),
            12_200,
            fixture.clock.now(),
            limits,
        )
        .await
        .expect("the ledger accepts");
    let bearer = fixture.bearer().await;

    // 12 200 + 64 stays under the limit.
    let (_, first, _) = live_link(&fixture, &bearer, 1).await;
    let used = fixture.quotas.usage(&owner).await.expect("usage").used;
    assert_eq!(used, 12_264);

    // A second, distinct record would reach it.
    let record = payload(b'n', 64);
    let refused = address_of(&record);
    let problem = issue(
        &fixture,
        &bearer,
        &json!({
            "opaque_id": opaque(2),
            "metadata_hash": refused.as_str(),
            "serves": [refused.as_str()],
            "metadata_blob": BASE64.encode(&record),
        }),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(problem["code"], "error.quota.exceeded", "{problem}");
    assert_eq!(
        fixture.quotas.usage(&owner).await.expect("usage").used,
        used,
        "a refused record is not charged"
    );
    fetch(
        &fixture,
        &format!("/s/{}", opaque(2)),
        StatusCode::NOT_FOUND,
    )
    .await;

    // The first record again, under a third link: already attributed, so admitted.
    issue(
        &fixture,
        &bearer,
        &json!({
            "opaque_id": opaque(3),
            "metadata_hash": first.as_str(),
            "serves": [first.as_str()],
            "metadata_blob": BASE64.encode(payload(b'm', 64)),
        }),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        fixture.quotas.usage(&owner).await.expect("usage").used,
        used
    );
}

/// #653: the collector keeps a live link's record, and reclaims it once the link is revoked.
#[tokio::test]
async fn the_collector_keeps_a_live_links_record_and_reclaims_a_revoked_ones() {
    use capsule_server::gc::{CollectionContext, DEFAULT_GRACE_WINDOW, Mode, collect};

    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (id, metadata, _) = live_link(&fixture, &bearer, 1).await;
    let collection = CollectionContext::new(
        fixture.index.clone(),
        fixture.blobs.clone(),
        fixture.marks.clone(),
        fixture.quotas.clone(),
        fixture.shares.clone(),
        fixture.clock.clone(),
        DEFAULT_GRACE_WINDOW,
    );

    let report = collect(&collection, Mode::Apply).await.expect("a pass");
    assert!(
        !report.marked.contains(&metadata),
        "a live link's record has no asset row and must still be held: {report:?}"
    );
    fixture
        .clock
        .advance(DEFAULT_GRACE_WINDOW + SignedDuration::from_hours(1));
    let report = collect(&collection, Mode::Apply).await.expect("a pass");
    assert!(!report.swept.contains(&metadata), "{report:?}");
    let bearer = fixture.bearer().await;
    fetch(
        &fixture,
        &format!("/s/{id}/blob/{metadata}"),
        StatusCode::OK,
    )
    .await;

    fixture
        .client
        .delete(&format!("/v1/shares/{id}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    let report = collect(&collection, Mode::Apply).await.expect("a pass");
    assert!(report.marked.contains(&metadata), "{report:?}");
    fixture
        .clock
        .advance(DEFAULT_GRACE_WINDOW + SignedDuration::from_hours(1));
    let report = collect(&collection, Mode::Apply).await.expect("a pass");
    assert!(report.swept.contains(&metadata), "{report:?}");
    assert!(fixture.blobs.stat(&metadata).await.expect("stat").is_none());
    // The record was charged to the owner at issue, and the sweep gives the bytes back.
    let owner = capsule_server::store::UserId::new(support::user().as_str());
    assert!(
        report.credited.contains(&(owner, 64)),
        "the swept record's bytes go back to the account that issued it: {report:?}"
    );
}

/// #653: a stored record counts against the owner's quota, once per distinct record.
#[tokio::test]
async fn a_metadata_record_is_charged_to_the_owner_once() {
    use capsule_server::quota::QuotaStore as _;

    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let owner = capsule_server::store::UserId::new(support::user().as_str());
    let before = fixture.quotas.usage(&owner).await.expect("usage").used;

    let (_, metadata, _) = live_link(&fixture, &bearer, 1).await;
    let after = fixture.quotas.usage(&owner).await.expect("usage").used;
    assert_eq!(
        after,
        before + 64,
        "the 64-byte record is the owner's storage"
    );

    // The same record under a second link is the same bytes, and is not charged again.
    issue(
        &fixture,
        &bearer,
        &json!({
            "opaque_id": opaque(2),
            "metadata_hash": metadata.as_str(),
            "serves": [metadata.as_str()],
            "metadata_blob": BASE64.encode(payload(b'm', 64)),
        }),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        fixture.quotas.usage(&owner).await.expect("usage").used,
        after
    );
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

/// A saturated limiter partition must not impersonate an outage.
///
/// The share path charges a caller-supplied opaque id before it resolves it, so an attacker
/// minting ids fills the `ShareLink` partition. Partitioning (decision 22) stops that reaching
/// the other surfaces; it does not stop it reaching *this* one, and a first-time visitor to any
/// share link is refused while it lasts. What they are told matters: `429` with a code of its own
/// and a retry hint means a client backs off and an operator paged on `5xx` is not paged at all,
/// where the `500 error.share.unavailable` this used to render said "the server is broken".
#[tokio::test]
async fn a_saturated_limiter_partition_is_a_429_and_not_an_outage() {
    let fixture = Fixture::with_counter_ceiling(1);
    let bearer = fixture.bearer().await;
    let (id, _, _) = live_link(&fixture, &bearer, 1).await;

    // One key fills the partition.
    fetch(&fixture, &format!("/s/{id}"), StatusCode::OK).await;

    // A second, never-seen link finds no room. It is told to wait, not that the server broke.
    let (other, _, _) = live_link(&fixture, &bearer, 2).await;
    let problem: Value = fetch(
        &fixture,
        &format!("/s/{other}"),
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await
    .json();
    assert_eq!(
        problem["code"], "error.share.at_capacity",
        "a full partition has a code of its own, distinct from a spent budget and from an outage"
    );
    let retry_after = problem["retry_after"]
        .as_u64()
        .expect("a retry hint the client can act on");
    assert!(retry_after > 0);

    // And the link that already holds a window keeps being served: the flood locks out new
    // keys, never the ones already counted.
    fetch(&fixture, &format!("/s/{id}"), StatusCode::OK).await;

    // The spent-budget 429 stays its own code, so the two causes remain distinguishable.
    for _ in 0..60 {
        fixture.client.get(&format!("/s/{id}")).send().await;
    }
    let problem: Value = fetch(&fixture, &format!("/s/{id}"), StatusCode::TOO_MANY_REQUESTS)
        .await
        .json();
    assert_eq!(problem["code"], "error.share.rate_limited");
    assert!(problem["retry_after"].as_u64().is_some_and(|s| s > 0));
}
