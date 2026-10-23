//! The **SDK's own client**, over a socket, against the real router.
//!
//! Every other case in this suite drives the router in-process through
//! `kynos::test::TestClient` — no socket, no port, nothing to flake — and that is the right
//! shape for asserting what the server decides. It is the wrong shape for asserting that a
//! *client library* can talk to it: an in-process client shares the server's types and never
//! serializes anything, so it cannot catch a generated client that lowers a query parameter
//! wrongly, misreads a status, or decodes a cursor into something the server will refuse.
//!
//! So this one case binds the assembled context to an ephemeral port and points
//! `capsule_sdk::sync::SyncConsumer` at it. What it proves is the round trip nothing else can:
//!
//! - the opaque, server-MAC'd cursor survives the trip out through JSON, through the client's
//!   own storage, and back in — the client never interprets it and the server accepts what it
//!   handed over;
//! - a page decodes into the SDK's own shape, including the base64 manifest bytes, which are
//!   the exact signed bytes `verify_asset` needs and therefore the one field where a
//!   re-encoding would be silent and fatal;
//! - the client-held **anti-rewind** high-water mark refuses a replayed page that the *real*
//!   server authenticated and re-served — the half of cursor authenticity a cursor MAC cannot
//!   provide, because a hostile server can always hand back one of its own older, validly-MAC'd
//!   cursors;
//! - and a forged cursor is refused with the stable `error.*` code the client localizes, which
//!   is the whole `S-C36`/`S-C38` contract observed from the far end of a wire.
//!
//! It replaces `capsule-api/sync/src/tests/sdk_client.rs`, which proved the same properties
//! against the gRPC feed and the crate that served it. Both are gone.

mod support;

use capsule_sdk::auth::AuthClient;
use capsule_sdk::sync::{ChangeKind, SyncConsumer, SyncCursor, SyncError, SyncState};
use capsule_server::App;
use capsule_server::app::Modules;
use capsule_server::blob::{BlobStore, ContentAddress};
use capsule_server::index::{AssetIndex, BlobRecord, PendingAsset};
use capsule_server::store::{AssetId, BlobRole, Clock};
use capsule_server::upload::{UploadContext, UploadPolicy};
use jiff::Timestamp;
use support::{EMAIL, Fixture, PASSWORD, PROTOCOL_VERSION, album, owner};

/// A client ceiling above every seeded entry's pin, so nothing is forward-version refused.
const CLIENT_MAX_PROTOCOL: &str = "2099-12-31";

/// Bind the fixture's own context to an ephemeral port and return its base URL.
///
/// The listener serves the **same** context the fixture holds handles on, so an asset seeded
/// through `fixture.index` is an asset this server serves.
async fn serve(fixture: &Fixture) -> String {
    serve_app(fixture.app()).await
}

/// Bind `app` to an ephemeral port and return its base URL.
async fn serve_app(app: App) -> String {
    let service = capsule_server::service(app).expect("the router builds");
    let bound = kynos::server::Server::new(service)
        .bind(("127.0.0.1", 0))
        .prepare()
        .await
        .expect("an ephemeral port binds");
    let address = *bound
        .local_addrs()
        .first()
        .expect("a bound server has an address");
    tokio::spawn(async move {
        // The task lives as long as the test; a serve error after the test has finished is not
        // this test's to report.
        let _ = bound.serve().await;
    });
    format!("http://{address}")
}

/// Publish `asset` with a real provenance blob behind it, and return the manifest bytes.
async fn publish(fixture: &Fixture, asset: &str) -> Vec<u8> {
    let id = AssetId::new(asset);
    fixture
        .index
        .reserve(PendingAsset {
            asset_id: id.clone(),
            owner_id: owner(),
            album_id: album(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            crypto_suite_id: 1,
            created_at: fixture.clock.now(),
        })
        .await
        .expect("the index reserves");

    // Deliberately not valid CBOR, and deliberately carrying a byte that is not printable
    // ASCII: the feed serves these bytes verbatim, so the only thing worth asserting is that
    // what arrives is what was stored — and base64 of arbitrary bytes is where a re-encoding
    // would show up.
    let manifest = format!("signed-manifest-{asset}\u{0}\u{ff}").into_bytes();
    let provenance = store(fixture, &manifest).await;
    record(fixture, &id, BlobRole::Provenance, &provenance).await;
    let metadata = store(fixture, format!("metadata-{asset}").as_bytes()).await;
    record(fixture, &id, BlobRole::Metadata, &metadata).await;
    manifest
}

async fn store(fixture: &Fixture, bytes: &[u8]) -> ContentAddress {
    let address = ContentAddress::parse(&support::checksum(bytes)).expect("a content address");
    fixture.blobs.put(&address, bytes).await.expect("stored");
    address
}

async fn record(fixture: &Fixture, asset: &AssetId, role: BlobRole, address: &ContentAddress) {
    fixture
        .index
        .record_blob(
            asset,
            BlobRecord {
                role,
                address: address.clone(),
                size: 32,
                manifest_sha256: None,
                amk_version: None,
                finalized_at: Timestamp::UNIX_EPOCH,
            },
        )
        .await
        .expect("the index records");
}

/// A live SDK session against the served base URL.
///
/// The SDK's `AuthClient` takes the *auth* base — it appends `/login` — while the generated
/// REST client takes the API root. Two arguments for one origin, which is a seam worth noticing
/// and not this test's to change.
async fn session(base_url: &str) -> capsule_sdk::auth::Session {
    let client = AuthClient::new(&format!("{base_url}/v1/auth")).expect("a base url");
    client
        .login(EMAIL, PASSWORD)
        .await
        .expect("the seeded account signs in")
        .into_session()
        .expect("the seeded account has no second factor")
}

// ===========================================================================================

#[tokio::test]
async fn the_generated_client_round_trips_the_feed_over_a_socket() {
    let fixture = Fixture::working();
    let first = publish(&fixture, "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e01").await;
    let second = publish(&fixture, "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e02").await;
    let base_url = serve(&fixture).await;

    let consumer =
        SyncConsumer::with_session(&base_url, session(&base_url).await).expect("a consumer builds");
    let mut state = SyncState::new(CLIENT_MAX_PROTOCOL);

    // One entry at a time, so the cursor is exercised rather than skipped.
    let page = consumer.pull_into(&mut state, 1).await.expect("page one");
    assert_eq!(page.entries.len(), 1);
    assert!(page.has_more, "the server said there is more, and there is");
    assert_eq!(
        page.entries[0].manifest_cbor, first,
        "the signed bytes survive base64 and JSON exactly, which is the one field where a \
         re-encoding would be silent and would break every signature"
    );
    assert_eq!(page.entries[0].kind, ChangeKind::Created);
    assert!(!page.next_cursor.is_start(), "the cursor advanced");

    let after_first = state.cursor().clone();

    let page = consumer.pull_into(&mut state, 1).await.expect("page two");
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].manifest_cbor, second);
    assert!(!page.has_more, "and now the client is caught up");

    // The anti-rewind half. The cursor below is one the *real* server minted and will happily
    // authenticate — replaying it is exactly the move a hostile server makes, and only the
    // client-held high-water mark refuses it.
    let replayed = consumer
        .pull(&after_first, 1)
        .await
        .expect("the server re-serves an older page for a cursor it authenticated");
    assert_eq!(replayed.entries.len(), 1);
    let error = state
        .apply_page(&replayed)
        .expect_err("a page the client has already applied must not apply again");
    assert!(
        matches!(error, SyncError::Rewind { .. }),
        "expected a rewind refusal, got {error:?}"
    );
}

#[tokio::test]
async fn a_forged_cursor_is_refused_with_the_code_the_client_localizes() {
    // The `error.*` code is the client half of the i18n contract, and this is the only place it
    // is observed the way a client observes it: parsed out of a problem body that crossed a
    // socket, from a document that had to *declare* the member for the generated type to carry
    // it.
    let fixture = Fixture::working();
    publish(&fixture, "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e03").await;
    let base_url = serve(&fixture).await;
    let consumer =
        SyncConsumer::with_session(&base_url, session(&base_url).await).expect("a consumer builds");

    let forged = SyncCursor::from_bytes(b"not a cursor this server minted".to_vec());
    let error = consumer
        .pull(&forged, 10)
        .await
        .expect_err("a forged cursor is refused");
    assert_eq!(
        error.error_code(),
        Some("error.sync.cursor_invalid"),
        "got {error:?}"
    );
}

#[tokio::test]
async fn after_an_epoch_bump_the_sdk_sees_a_stale_cursor_and_resyncs_from_zero() {
    // The client half of #697 over a socket: the 409 crosses as the code the SDK recognises,
    // and the reset it prescribes yields the whole library again under the new epoch.
    let fixture = Fixture::working();
    let held = publish(&fixture, "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e09").await;
    let base_url = serve(&fixture).await;
    let consumer =
        SyncConsumer::with_session(&base_url, session(&base_url).await).expect("a consumer builds");
    let mut state = SyncState::new(CLIENT_MAX_PROTOCOL);
    consumer
        .pull_into(&mut state, 10)
        .await
        .expect("the first drain");

    fixture
        .index
        .bump_sync_epoch(&owner())
        .await
        .expect("the index bumps");

    let error = consumer
        .pull_into(&mut state, 10)
        .await
        .expect_err("a cursor from the old epoch is refused");
    assert!(error.is_stale_cursor(), "got {error:?}");

    state.reset_for_resync();
    let page = consumer
        .pull_into(&mut state, 10)
        .await
        .expect("a resync from zero");
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].manifest_cbor, held);
}

/// Store `bytes` under their content address, as a rebuild leaves a lost row's blobs behind.
async fn hold(fixture: &Fixture, bytes: &[u8]) {
    store(fixture, bytes).await;
}

#[tokio::test]
async fn after_an_epoch_change_the_sdk_reannounces_what_the_feed_lost_and_the_index_converges() {
    // The #697 drill continued (#781), over a socket. One asset survived the rebuild; one lost
    // its row while its blobs stayed in the store; one lost its blobs too; and the client also
    // holds a manifest another account signed. After the stale cursor and the resync, the SDK
    // re-announces what the feed no longer lists: the lost row comes back, the one without
    // blobs is named as missing and nothing is restored, and the foreign one is refused.
    use std::collections::HashSet;

    use capsule_sdk::sync::reannounce::{HeldManifest, ReannounceOutcome, reannounce_unlisted};

    let fixture = Fixture::working();
    let survived = publish(&fixture, "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e09").await;
    let base_url = serve(&fixture).await;
    let consumer =
        SyncConsumer::with_session(&base_url, session(&base_url).await).expect("a consumer builds");
    let mut state = SyncState::new(CLIENT_MAX_PROTOCOL);
    consumer
        .pull_into(&mut state, 10)
        .await
        .expect("the first drain");

    let lost_id = uuid::Uuid::parse_str("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5ea2").expect("a uuid");
    let lost = support::signed_create(&fixture.clock, lost_id, b"lost-original", b"lost-metadata");
    hold(&fixture, b"lost-original").await;
    hold(&fixture, b"lost-metadata").await;
    hold(&fixture, &lost).await;

    let gone_id = uuid::Uuid::parse_str("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5ea3").expect("a uuid");
    let gone = support::signed_create(&fixture.clock, gone_id, b"gone-original", b"gone-metadata");

    let foreign_id = uuid::Uuid::parse_str("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5ea4").expect("a uuid");
    let foreign = support::signed_create_by(
        &fixture.clock,
        uuid::Uuid::from_u128(42),
        foreign_id,
        b"foreign-original",
        b"foreign-metadata",
    );
    hold(&fixture, b"foreign-original").await;
    hold(&fixture, b"foreign-metadata").await;
    hold(&fixture, &foreign).await;

    // The rebuild moved the epoch: the old cursor is stale, and a resync from zero lists only
    // what survived.
    fixture
        .index
        .bump_sync_epoch(&owner())
        .await
        .expect("the index bumps");
    let error = consumer
        .pull_into(&mut state, 10)
        .await
        .expect_err("a cursor from the old epoch is refused");
    assert!(error.is_stale_cursor(), "got {error:?}");
    state.reset_for_resync();
    let page = consumer
        .pull_into(&mut state, 10)
        .await
        .expect("a resync from zero");
    let listed: HashSet<String> = page
        .entries
        .iter()
        .map(|entry| String::from_utf8(entry.asset_id.clone()).expect("a utf-8 id"))
        .collect();
    assert_eq!(listed.len(), 1);

    let held = vec![
        HeldManifest {
            asset_id: "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e09".to_owned(),
            manifest: survived,
        },
        HeldManifest {
            asset_id: lost_id.to_string(),
            manifest: lost.clone(),
        },
        HeldManifest {
            asset_id: gone_id.to_string(),
            manifest: gone,
        },
        HeldManifest {
            asset_id: foreign_id.to_string(),
            manifest: foreign,
        },
    ];
    let answers = reannounce_unlisted(&consumer, &held, &listed)
        .await
        .expect("the re-announce is answered");
    let by_asset: std::collections::BTreeMap<String, ReannounceOutcome> = answers
        .into_iter()
        .map(|(asset, result)| (asset, result.outcome))
        .collect();
    assert_eq!(by_asset.len(), 3, "the survivor is not re-announced");
    assert!(
        matches!(
            by_asset[&lost_id.to_string()],
            ReannounceOutcome::Restored { sync_seq: Some(_) }
        ),
        "got {by_asset:?}"
    );
    let ReannounceOutcome::MissingBlobs(missing) = &by_asset[&gone_id.to_string()] else {
        panic!("got {by_asset:?}");
    };
    let mut missing = missing.clone();
    missing.sort();
    let mut expected = vec![
        support::checksum(b"gone-original"),
        support::checksum(b"gone-metadata"),
        support::checksum(&held[2].manifest),
    ];
    expected.sort();
    assert_eq!(missing, expected);
    assert_eq!(
        by_asset[&foreign_id.to_string()],
        ReannounceOutcome::Refused {
            code: "error.sync.reannounce_denied".to_owned()
        }
    );

    // The index converged: the feed lists the survivor and the restored row, with the restored
    // manifest byte for byte, and nothing else.
    let page = consumer
        .pull_into(&mut state, 10)
        .await
        .expect("the next page");
    assert_eq!(page.entries.len(), 1);
    assert_eq!(
        String::from_utf8(page.entries[0].asset_id.clone()).expect("a utf-8 id"),
        lost_id.to_string()
    );
    assert_eq!(page.entries[0].manifest_cbor, lost);
    assert!(
        fixture
            .index
            .read(&AssetId::new(gone_id.to_string()))
            .await
            .expect("read")
            .is_none()
    );
    assert!(
        fixture
            .index
            .read(&AssetId::new(foreign_id.to_string()))
            .await
            .expect("read")
            .is_none()
    );
}

#[tokio::test]
async fn a_refresh_rotates_the_pair_and_closes_the_one_it_replaced() {
    // The server behaviour the CLI's session-persistence bug tripped over. A refresh mints a new
    // pair and **closes the old session**, so a client that refreshed and then threw the rotated
    // pair away was left holding a refresh token the server had already invalidated — which is
    // why every command demanded an interactive login roughly fifteen minutes after signing in
    // while the stored token still had seven days on it.
    //
    // Asserted here, over a socket, through the SDK's own session: the CLI's half is the
    // write-back, and this is the half that makes the write-back necessary.
    let fixture = Fixture::working();
    let base_url = serve(&fixture).await;
    let session = session(&base_url).await;

    let before = session.export().await.expect("a live session exports");
    session.refresh().await.expect("the pair rotates");
    let after = session.export().await.expect("and still exports");

    use secrecy::ExposeSecret as _;
    assert_ne!(
        before.refresh_token.expose_secret(),
        after.refresh_token.expose_secret(),
        "a refresh mints a new pair; a client that stored the old one stored a dead one"
    );

    // And the pair it replaced is genuinely gone: resuming from it cannot refresh again.
    let stale = AuthClient::new(&format!("{base_url}/v1/auth"))
        .expect("a base url")
        .resume(before)
        .expect("a session resumes from any pair");
    stale
        .refresh()
        .await
        .expect_err("the session the rotation closed is closed");
}

/// The SDK reads a real `202` from a real server, and a real code finishes the sign-in.
///
/// The unit tests cover this against a mock; this covers it against the router that decides,
/// over a socket, which is the one thing a mock cannot rule out — that the two ends disagree
/// about which status carries the challenge.
#[tokio::test]
async fn the_sdk_completes_a_real_second_factor_over_a_socket() {
    let fixture = Fixture::working();
    let base = serve(&fixture).await;
    let bearer = fixture.bearer().await;

    // Switch a second factor on through the served surface, exactly as a client would.
    fixture
        .client
        .post("/v1/auth/totp/enroll")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(kynos::http::StatusCode::OK);
    fixture
        .client
        .post("/v1/auth/totp/verify-enrollment")
        .header("authorization", &bearer)
        .json(&serde_json::json!({
            "totp_code": support::totp_code(&fixture, &support::user()),
        }))
        .send()
        .await
        .assert_status(kynos::http::StatusCode::NO_CONTENT);

    let client = AuthClient::new(&format!("{base}/v1/auth")).expect("a base url");
    let capsule_sdk::auth::LoginOutcome::SecondFactorRequired { mfa_token, .. } = client
        .login(EMAIL, PASSWORD)
        .await
        .expect("the password verifies")
    else {
        panic!("an account with a confirmed second factor must not answer a token pair");
    };

    // A step on, so the confirming code is spent and gone.
    fixture.clock.advance(jiff::SignedDuration::from_secs(
        i64::try_from(capsule_server::auth::totp::STEP_SECONDS).expect("in range"),
    ));
    let session = client
        .verify_second_factor(&mfa_token, &support::totp_code(&fixture, &support::user()))
        .await
        .expect("the code completes the sign-in");
    assert!(session.is_authenticated().await);
}

/// The escrow round trip, over a socket, against the router that actually serves it.
///
/// This is the case the slice was missing. `capsule_sdk::recovery` used to build
/// `{api_root}/backup/escrow` by hand — the Salvo document's path — and its own in-module mock
/// answered whatever path it was handed, so every escrow test passed while no real server had
/// that route. Only a client pointed at the router can tell the difference, and the bytes are
/// the ones a KDF runs against: a wrap that comes back re-encoded is a lost master key.
///
/// **The route is asserted, not inferred.** Both halves are cross-checked against
/// `/v1/auth/escrow` through the fixture's own in-process client: what the SDK stored is read
/// back at that path, and what was seeded at that path is what the SDK fetches. A client
/// talking to some other path could satisfy neither, so this does not depend on how the
/// router happens to render a `404` for a path it does not serve.
#[tokio::test]
async fn the_sdk_stores_and_fetches_an_escrow_over_a_socket() {
    use capsule_core::crypto::primitives::Argon2Params;
    use capsule_core::crypto::pwkdf;
    use capsule_sdk::recovery::{RecoveryClient, RecoveryError};

    // Fast Argon2id params: the crypto is `capsule-core`'s and proven there; what is under test
    // is the wire.
    let params = Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    };
    const SECRET: &[u8] = b"correct horse battery staple";

    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let base_url = serve(&fixture).await;
    let client =
        RecoveryClient::new(session(&base_url).await, &base_url).expect("an API root parses");

    // Nothing stored yet: the typed refusal a cadence reads as "enroll first".
    let missing = client
        .fetch_escrow()
        .await
        .expect_err("a fresh account has escrowed nothing");
    assert!(
        matches!(missing, RecoveryError::NotEnrolled),
        "got {missing:?}"
    );

    // ── The SDK writes; the contract's route is where it landed ───────────────────────────
    let master = [0x5Au8; 32];
    let blob = pwkdf::wrap_with(&master, SECRET, params).expect("the master key wraps");
    client.store_escrow(&blob).await.expect("the escrow stores");

    let response = fixture
        .client
        .get("/v1/auth/escrow")
        .header("authorization", &bearer)
        .send()
        .await;
    let seen = response.assert_status(kynos::http::StatusCode::OK).bytes();
    assert_eq!(
        seen.as_ref(),
        capsule_core::cbor::to_canonical_vec(&blob)
            .expect("the wrap encodes")
            .as_slice(),
        "the bytes the SDK stored must be readable at `/v1/auth/escrow` — the path the \
         committed document declares, which is the assertion the old tests could not make"
    );

    // ── The SDK reads back what that route holds, byte for byte ───────────────────────────
    let cache = client.fetch_escrow().await.expect("and comes back");
    assert_eq!(
        cache.blob(),
        &blob,
        "the escrow is ciphertext served verbatim; a re-encoded wrap no longer opens"
    );
    assert_eq!(
        capsule_core::backup::recover_master_key(cache.blob(), SECRET)
            .expect("the fetched wrap opens"),
        master,
    );

    // A rotation seeded at the contract's route is the one the SDK sees next — the read half
    // pinned to the same path, without relying on a refusal to prove it.
    let rotated = pwkdf::wrap_with(&master, b"a different secret entirely", params)
        .expect("the master key re-wraps");
    fixture
        .client
        .put("/v1/auth/escrow")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .body(
            "application/octet-stream",
            capsule_core::cbor::to_canonical_vec(&rotated).expect("the wrap encodes"),
        )
        .send()
        .await
        .assert_status(kynos::http::StatusCode::OK);
    assert_eq!(
        client
            .fetch_escrow()
            .await
            .expect("the rotated escrow comes back")
            .blob(),
        &rotated,
        "the SDK reads the resource `/v1/auth/escrow` addresses, not some other path that \
         happens to answer"
    );
}

/// The sealed identity round trip (#544), over a socket, pinned to the contract's route the same
/// way the escrow case above is: what the SDK stored is read back at
/// `/v1/auth/escrow/identity`, and what was seeded there is what the SDK fetches.
#[tokio::test]
async fn the_sdk_stores_and_fetches_a_sealed_identity_over_a_socket() {
    use capsule_sdk::recovery::{RecoveryClient, RecoveryError};

    const PATH: &str = "/v1/auth/escrow/identity";
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let base_url = serve(&fixture).await;
    let client =
        RecoveryClient::new(session(&base_url).await, &base_url).expect("an API root parses");

    // Nothing stored yet: its own typed state and its own code, not the escrow's.
    let missing = client
        .fetch_sealed_identity()
        .await
        .expect_err("a fresh account has stored no sealed identity");
    assert!(
        matches!(missing, RecoveryError::IdentityNotStored),
        "got {missing:?}"
    );
    assert_eq!(
        missing.error_code(),
        Some(capsule_i18n::error_codes::ESCROW_IDENTITY_NOT_STORED)
    );

    // The SDK writes; the contract's route is where it landed.
    let sealed = b"\x00 sealed identity \xff".to_vec();
    let stored = client
        .store_sealed_identity(&sealed)
        .await
        .expect("the sealed identity stores");
    assert!(!stored.replaced, "the account's first");
    let response = fixture
        .client
        .get(PATH)
        .header("authorization", &bearer)
        .send()
        .await;
    let seen = response.assert_status(kynos::http::StatusCode::OK).bytes();
    assert_eq!(seen.as_ref(), sealed.as_slice());
    assert_eq!(
        client
            .fetch_sealed_identity()
            .await
            .expect("and comes back"),
        sealed
    );

    // A reseal seeded at the contract's route is the one the SDK sees next, and storing one
    // more through the SDK reports the replacement.
    let resealed = b"resealed under a rotated master key".to_vec();
    fixture
        .client
        .put(PATH)
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .body("application/octet-stream", resealed.clone())
        .send()
        .await
        .assert_status(kynos::http::StatusCode::OK);
    assert_eq!(
        client
            .fetch_sealed_identity()
            .await
            .expect("the reseal comes back"),
        resealed
    );
    assert!(
        client
            .store_sealed_identity(&sealed)
            .await
            .expect("stores again")
            .replaced
    );

    // An empty seal is refused like an empty escrow, with the server's code.
    let refused = client
        .store_sealed_identity(&[])
        .await
        .expect_err("an empty body cannot be a sealed identity");
    assert!(
        matches!(&refused, RecoveryError::Malformed { .. }),
        "got {refused:?}"
    );
    assert_eq!(
        refused.error_code(),
        Some(capsule_i18n::error_codes::ESCROW_MALFORMED)
    );

    // The escrow slot was never touched.
    assert!(matches!(
        client.fetch_escrow().await,
        Err(RecoveryError::NotEnrolled)
    ));
}

/// The album key escrows (#741), over a socket and pinned to the contract's routes: what the SDK
/// stores lands under `/v1/auth/escrow/albums/{album_id}`, what was seeded there is what the SDK
/// lists, byte for byte, and the server's refusals arrive typed with their codes.
#[tokio::test]
async fn the_sdk_stores_and_lists_album_key_escrows_over_a_socket() {
    use capsule_sdk::recovery::{RecoveryClient, RecoveryError};
    use uuid::Uuid;

    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let base_url = serve(&fixture).await;
    let client =
        RecoveryClient::new(session(&base_url).await, &base_url).expect("an API root parses");

    assert!(
        client
            .list_album_keys()
            .await
            .expect("an account with no album escrowed lists none")
            .is_empty()
    );

    let first = Uuid::from_u128(0x0198_f3c2_0000_7000_8000_0000_0000_00b2);
    let second = Uuid::from_u128(0x0198_f3c2_0000_7000_8000_0000_0000_00a1);
    let sealed = b"\x00 album keys \xff".to_vec();
    let stored = client
        .store_album_keys(first, &sealed)
        .await
        .expect("an album's keys store");
    assert!(!stored.replaced, "the album's first");
    assert!(
        client
            .store_album_keys(first, &sealed)
            .await
            .expect("and store again")
            .replaced
    );

    // Seeded at the contract's route, under the second album: the SDK lists both in album order.
    let seeded = b"seeded at the route".to_vec();
    fixture
        .client
        .put(&format!("/v1/auth/escrow/albums/{second}"))
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .body("application/octet-stream", seeded.clone())
        .send()
        .await
        .assert_status(kynos::http::StatusCode::OK);
    let listed = client.list_album_keys().await.expect("the listing reads");
    let listed: Vec<(Uuid, Vec<u8>)> = listed
        .into_iter()
        .map(|entry| (entry.album_id, entry.blob))
        .collect();
    assert_eq!(listed, vec![(second, seeded), (first, sealed)]);

    let refused = client
        .store_album_keys(first, &[])
        .await
        .expect_err("an empty body cannot be an album's keys");
    assert!(
        matches!(&refused, RecoveryError::Malformed { .. }),
        "got {refused:?}"
    );
    assert_eq!(
        refused.error_code(),
        Some(capsule_i18n::error_codes::ESCROW_MALFORMED)
    );

    fixture.escrows.set_unavailable(true);
    let down = client
        .list_album_keys()
        .await
        .expect_err("a store that cannot answer");
    assert!(
        matches!(&down, RecoveryError::Unavailable { .. }),
        "got {down:?}"
    );
    assert_eq!(
        down.error_code(),
        Some(capsule_i18n::error_codes::ESCROW_UNAVAILABLE)
    );
}

/// **`S-D17`'s Done-when, against the server that decides.** A token the client still believes
/// in and the server has stopped honouring is refreshed once and the call replayed once.
///
/// The unit tests cover the layer against a mock; this covers the one thing a mock cannot rule
/// out — that the two ends disagree about when an access token dies. The server validates `exp`
/// against its **injected** clock (`capsule_server::auth::tokens`, deliberately, so a test can
/// walk over an expiry), so advancing the fixture's clock past `ACCESS_TOKEN_TTL` revokes the
/// access token for real while the session and its refresh token remain live.
///
/// The client is then handed the same token pair with a far-future expiry, which is exactly the
/// state a client is in whenever it trusted a server-supplied deadline and the server changed
/// its mind first — a revocation, a clock skew, a rotated signing key. Because the client sees
/// no reason to refresh, the pre-flight half cannot fire, so a call that succeeds here succeeded
/// through the reactive layer and nothing else.
#[tokio::test]
async fn a_token_the_server_stopped_honouring_is_refreshed_and_the_call_replayed() {
    use capsule_sdk::auth::PersistedSession;
    use capsule_sdk::client::AuthenticatedClient;
    use secrecy::ExposeSecret as _;

    let fixture = Fixture::working();
    let base_url = serve(&fixture).await;
    let signed_in = session(&base_url).await;
    let pair = signed_in.export().await.expect("a live session exports");
    let stale_access = pair.access_token.expose_secret().to_owned();

    // Past the access token's life, well inside the session's. The refresh token still works;
    // the access token does not.
    fixture.clock.advance(
        capsule_server::auth::ACCESS_TOKEN_TTL
            .checked_add(jiff::SignedDuration::from_secs(60))
            .expect("a representable instant"),
    );

    // The same pair, with a deadline the client has no reason to doubt.
    let session = AuthClient::new(&format!("{base_url}/v1/auth"))
        .expect("a base url")
        .resume(PersistedSession {
            access_token: stale_access.clone().into(),
            refresh_token: pair.refresh_token,
            access_expires_at_unix: Timestamp::now().as_second() + 3600,
        })
        .expect("a session resumes from any pair");
    let client = AuthenticatedClient::new(&base_url, session).expect("an API root parses");

    // A generated operation, called straight through the Deref — nothing about this call site
    // knows a retry layer exists, which is the point of putting it at the transport seam.
    let quota = client
        .get_quota(capsule_core::crypto::primitives::PROTOCOL_VERSION, None)
        .await
        .expect("the 401 is recovered and the call replayed")
        .into_inner();
    assert_eq!(quota.state.as_str(), "ok");

    let after = client
        .session()
        .export()
        .await
        .expect("the session is still live");
    assert_ne!(
        after.access_token.expose_secret(),
        stale_access.as_str(),
        "the replay must have ridden a rotated token; an unchanged one would mean the server \
         accepted a token it had already stopped honouring"
    );
}

/// The album-upgrade proposal, over a socket, against the server that verifies the signature.
///
/// This one cannot be proven against a mock at all. The intent is signed with the proposing
/// device's DSK and verified against the account's **published** device directory, so a mock
/// that answered `200` would prove only that the client can post bytes. Here the directory is
/// anchored, the album provisioned, and the intent signed by `capsule-core` with the *same*
/// types the server verifies with — so what is asserted is that the bytes the SDK put on the
/// wire are the bytes that verify.
///
/// The `409` half matters as much: only one ceremony may hold an album, and a client that read
/// that refusal as "malformed, re-sign" would have an admin re-signing intents forever.
#[tokio::test]
async fn the_sdk_proposes_an_album_upgrade_over_a_socket() {
    use capsule_sdk::upgrade::{UpgradeClient, UpgradeError};
    use support::{
        device, identity_header, identity_key, signed_directory_with_device, signed_upgrade_intent,
    };
    use uuid::Uuid;

    let intent_id = Uuid::parse_str("019a0000-0000-7000-8000-00000000cafe").expect("a uuid");
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let identity = identity_key();
    let device_key = identity_key();

    // Anchor the directory holding the proposing device, then provision the album. Without the
    // first, every proposal is `403` however well signed — the directory *is* the trust anchor.
    fixture
        .client
        .post("/v1/auth/devices/directory")
        .header("authorization", &bearer)
        .header("x-capsule-identity-key", &identity_header(&identity))
        .body(
            "application/cbor",
            signed_directory_with_device(
                &identity,
                1,
                device(),
                &device_key,
                "1970-01-01T00:00:00Z",
            ),
        )
        .send()
        .await
        .assert_status(kynos::http::StatusCode::OK);
    fixture
        .client
        .post("/v1/albums")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&serde_json::json!({ "album_id": album().as_str() }))
        .send()
        .await
        .assert_status(kynos::http::StatusCode::CREATED);

    let base_url = serve(&fixture).await;
    let client = UpgradeClient::new(session(&base_url).await, &base_url);
    let album_id = Uuid::parse_str(album().as_str()).expect("the seeded album id is a uuid");
    let intent = signed_upgrade_intent(&device_key, device(), intent_id, "2030-01-01", 300);

    let phase = client
        .begin(album_id, &intent)
        .await
        .expect("a signed proposal from an anchored device is accepted");
    assert_eq!(phase.album_id, album_id);
    assert_eq!(
        phase.intent_id,
        Some(intent_id),
        "the ceremony the server now holds is the one the client proposed"
    );
    assert_eq!(phase.to_protocol_version.as_deref(), Some("2030-01-01"));
    assert_eq!(phase.in_flight, 0, "nothing is draining on a fresh album");
    assert!(
        phase.expires_at.is_some(),
        "the deadline is the server's to set, and it must reach the client as an instant"
    );

    // A second ceremony under a different id is refused with the live one — and with the code
    // a client localizes, parsed out of a problem body that crossed a socket.
    let second = Uuid::parse_str("019a0000-0000-7000-8000-00000000beef").expect("a uuid");
    let error = client
        .begin(
            album_id,
            &signed_upgrade_intent(&device_key, device(), second, "2030-01-01", 300),
        )
        .await
        .expect_err("only one ceremony may hold an album");
    let UpgradeError::InFlight {
        intent_id: live, ..
    } = &error
    else {
        panic!("expected an in-flight refusal, got {error:?}");
    };
    assert_eq!(live.as_deref(), Some(intent_id.to_string().as_str()));
    assert_eq!(
        error.error_code(),
        Some("error.album.upgrade_in_flight"),
        "got {error:?}"
    );
}

/// **Issue #404 meets the escrow route.** A write from outside the server's protocol window is
/// the gate's `426`, and the SDK reports it as [`RecoveryError::Unexpected`] carrying the
/// gate's own code — never as a verdict on the blob.
///
/// The window is the one knob a second `App` over the fixture's stores turns: every context
/// but `upload` is the fixture's own (so the session minted on the default-window listener is
/// live on the windowed one), and `upload` carries a policy whose window this build's protocol
/// date falls outside. Two listeners, one account, one sessions store.
#[tokio::test]
async fn an_escrow_write_outside_the_servers_window_is_a_426_the_sdk_reports_with_its_code() {
    use capsule_core::crypto::primitives::Argon2Params;
    use capsule_core::crypto::pwkdf;
    use capsule_sdk::recovery::{RecoveryClient, RecoveryError};
    use kynos::di::Provides as _;

    const VERSION_UNSUPPORTED: &str = "error.protocol.version_unsupported";

    let fixture = Fixture::working();
    let app = fixture.app();
    let windowed = App::new(Modules {
        auth: app.provide(),
        totp: app.provide(),
        upload: UploadContext::new(
            fixture.uploads.clone(),
            fixture.blobs.clone(),
            fixture.index.clone(),
            fixture.authority.clone(),
            fixture.clock.clone(),
            UploadPolicy::default().with_protocol_window("2000-01-01", "2000-01-01"),
        ),
        sync: app.provide(),
        serve: app.provide(),
        verify: app.provide(),
        directories: app.provide(),
        albums: app.provide(),
        quota: app.provide(),
        attestation: app.provide(),
        discovery: app.provide(),
        escrow: app.provide(),
        enrollment: app.provide(),
        moderation: app.provide(),
        share: app.provide(),
        drops: app.provide(),
        counters: app.provide(),
        // The three contexts that landed after this case was written (#405, #407, #406). They
        // are the fixture's own, exactly as every other context above is: the second `App`
        // exists to turn one knob on `upload`, and anything else it built for itself would be a
        // second server the session was not minted against.
        membership: app.provide(),
        oidc: app.provide(),
        federation: app.provide(),
    });

    // Sign in where the window admits this build; write where it does not.
    let admitting = serve(&fixture).await;
    let session = session(&admitting).await;
    let refusing = serve_app(windowed).await;
    let client = RecoveryClient::new(session.clone(), &refusing).expect("an API root parses");

    let blob = pwkdf::wrap_with(
        &[0x5Au8; 32],
        b"correct horse battery staple",
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        },
    )
    .expect("the master key wraps");
    let refused = client
        .store_escrow(&blob)
        .await
        .expect_err("a write from outside the window is refused");
    assert!(
        matches!(refused, RecoveryError::UpgradeRequired(_)),
        "expected the gate's 426 as the typed upgrade refusal, got {refused:?}"
    );
    assert_eq!(
        refused.error_code(),
        Some(VERSION_UNSUPPORTED),
        "the code a client localizes is the server's, not one this client minted"
    );

    // The window rides the refusing listener's responses, so the client can say which build
    // would be admitted.
    let response = session
        .execute(|http| http.get(format!("{refusing}/v1/version")))
        .await
        .expect("the exempt read answers");
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    assert_eq!(
        header("x-capsule-protocol-min").as_deref(),
        Some("2000-01-01")
    );
    assert_eq!(
        header("x-capsule-protocol-max").as_deref(),
        Some("2000-01-01")
    );

    // The same write on the admitting listener lands: the window was the only difference.
    RecoveryClient::new(session, &admitting)
        .expect("an API root parses")
        .store_escrow(&blob)
        .await
        .expect("this build's protocol date is inside the default window");
}

/// **A real push reaching a real feed, decoded and verified** — the two halves of issues #464
/// and #465, which only meet over a socket.
///
/// A library seals an asset, the SDK's ladder pushes it to this router, and the feed is pulled
/// back. Three things had to be true at once and two of them were not:
///
/// - the ladder must upload the **provenance** blob, because the server publishes an asset only
///   once it holds both index-tier roles (`upload::visibility::INDEX_TIER_ROLES`). Without it
///   the push succeeded, every blob landed, and the asset was invisible on the feed to every
///   device including the pusher's — a silent failure with no error anywhere;
/// - the bytes of that blob must be the canonical CBOR of the chain head `ProvenanceRecord`,
///   because the server's chain head is their SHA-256 while the client's next
///   `prior_provenance_hash` is `record_hash()`. Any other encoding and no lifecycle op could
///   chain onto the asset;
/// - `apply_remote_entry` must decode what the feed actually serves. It decoded an
///   `AssetManifest`, so a correctly pushed asset was quarantined as malformed by every
///   receiving device.
///
/// A mock proves none of this: the publish gate, the chain head and the served bytes are all
/// the server's, and each of the three defects is invisible from either end alone.
#[tokio::test]
async fn a_pushed_asset_reaches_the_feed_and_the_entry_decodes_and_verifies() {
    use std::collections::HashSet;

    use capsule_core::crypto::primitives::Argon2Params;
    use capsule_library::lifecycle::{RemoteEntry, SyncApplyOutcome, Workspace};
    use capsule_sdk::albums::{AlbumClient, AlbumTransport};
    use capsule_sdk::net::ConnectionClass;
    use capsule_sdk::push::{bundle_blobs, ensure_album, push_bundle};
    use capsule_sdk::staged::StagedScheduler;
    use capsule_sdk::upload::{BlobRole, UploadClient, UploadTransport};
    use capsule_server::store::{AlbumId, UserId};

    // Fast Argon2id: the KDF is proven in core, and the wire is what is under test.
    const FAST: Argon2Params = Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    };

    // ── A real library with one sealed asset ──────────────────────────────────────────────
    let dir = tempfile::TempDir::new().expect("a temp dir");
    let lib = dir.path().join("library");
    std::fs::create_dir_all(&lib).expect("the library root");
    let src = dir.path().join("photo.jpg");
    std::fs::write(&src, b"\xFF\xD8\xFF the bytes a push puts on the wire").expect("a source");

    let mut workspace =
        Workspace::create_with_params(&lib, b"passphrase", FAST).expect("a library");
    let album_id = workspace.default_album_id();
    workspace
        .ensure_album(album_id, "Imports")
        .expect("an album");
    let asset_id = workspace.import_asset(album_id, &src).expect("an import");
    let mut bundle = workspace.upload_bundle(&asset_id).expect("a bundle");
    // This fixture earns no derivative tier, so it carries a derivative manifest by hand (#556):
    // the server stores the role's bytes without parsing them, so what is under test is the
    // rung — the gate admitting the SDK's request, the feed handing the bytes back unchanged.
    assert!(bundle.derivative_manifests.is_empty());
    bundle
        .derivative_manifests
        .push(b"a signed derivative manifest, as canonical CBOR".to_vec());

    // ── A server that admits this library's album and device ──────────────────────────────
    let fixture = Fixture::working();
    // The library signs with the wall clock; the fixture's starts at the Unix epoch. Walk the
    // server's clock up to the manifest's own instant so the timestamp window admits it — the
    // window is a real check and the point here is the ladder, not the calendar.
    fixture.clock.advance(jiff::SignedDuration::from_secs(
        Timestamp::now().as_second(),
    ));
    // #405 refuses an upload whose `created_by_user` is not the authenticated caller — a writer
    // member must not file an asset into somebody else's album attributed to a third account.
    // This case pushes from a *real* `Workspace`, which mints its own account id at creation and
    // exposes no setter, so the identity that has to move is the server's: the seeded
    // credentials are re-pointed at the library's own user, and the album owner and the
    // directory device follow it. Using `support::user()` here would assert that the ladder
    // works while signing as somebody else, which is the thing #405 exists to refuse.
    let library_user = UserId::new(workspace.user_id().to_string());
    fixture.accounts.insert(EMAIL, PASSWORD, &library_user);
    fixture.authority.allow_album(
        &capsule_server::store::OwnerId::new(library_user.as_str()),
        &AlbumId::new(album_id.to_string()),
        PROTOCOL_VERSION,
    );
    // Admitted at the epoch, so the device predates every manifest this library writes.
    fixture.authority.add_device(
        &library_user,
        bundle.created_by_device,
        Timestamp::UNIX_EPOCH,
    );
    let base_url = serve(&fixture).await;
    let live = session(&base_url).await;
    // The library's own directory, published as a client does at enrollment: the ledger records
    // it, so the rebuild below verifies the pushed manifest's device signature against it (#782).
    capsule_sdk::directory::DirectoryClient::new(live.clone(), &format!("{base_url}/v1/auth"))
        .publish(workspace.device_directory(), &workspace.user_ik_public())
        .await
        .expect("the library's directory publishes");

    // ── The SDK's own ladder, over the socket ─────────────────────────────────────────────
    let albums = AlbumClient::new(AlbumTransport::with_session(
        live.clone(),
        format!("{base_url}/v1/albums"),
    ));
    ensure_album(&albums, album_id)
        .await
        .expect("the album provisions");

    let uploads = UploadClient::new(UploadTransport::with_session(
        live.clone(),
        format!("{base_url}/v1/upload"),
        PROTOCOL_VERSION,
    ));
    let scheduler = StagedScheduler::new(
        capsule_library::import::UploadPolicy::Full,
        ConnectionClass::Unmetered,
    );
    let report = push_bundle(&uploads, &scheduler, &bundle, &HashSet::new(), false)
        .await
        .expect("the ladder pushes every rung");

    // The rung that used to be missing, named in the report the ladder returns.
    let roles: Vec<BlobRole> = bundle_blobs(&bundle)
        .into_iter()
        .map(|(blob, _)| blob.role)
        .collect();
    assert_eq!(
        roles[0],
        BlobRole::Provenance,
        "the index tier leads with the provenance blob"
    );
    assert_eq!(
        report.pushed.len(),
        roles.len(),
        "every rung opened a session"
    );

    // ── The feed the push was supposed to reach ───────────────────────────────────────────
    let consumer = SyncConsumer::with_session(&base_url, live).expect("a consumer builds");
    let mut state = SyncState::new(CLIENT_MAX_PROTOCOL);
    let page = consumer.pull_into(&mut state, 10).await.expect("a page");
    let entry = page
        .entries
        .iter()
        // The feed's `asset_id` is the id's UTF-8 bytes, not its 16 raw ones.
        .find(|e| e.asset_id == asset_id.to_string().into_bytes())
        .unwrap_or_else(|| {
            panic!(
                "the pushed asset must be on the feed; without the provenance rung the server \
                 holds only one index-tier role and publishes nothing, silently. Got {} entries",
                page.entries.len()
            )
        });

    assert_eq!(
        entry.manifest_cbor, bundle.provenance_blob,
        "the feed serves the provenance blob's bytes back unchanged"
    );
    assert_eq!(
        entry.derivative_manifests, bundle.derivative_manifests,
        "the feed serves each derivative manifest's bytes back unchanged"
    );
    let held = capsule_sdk::staged::held_from_feed(entry);
    let replan = capsule_sdk::push::plan(&scheduler, &bundle, &held, false);
    assert!(
        replan
            .blobs
            .iter()
            .all(|blob| blob.tier != capsule_library::import::UploadTier::Preview),
        "a re-push reads the manifest as held from the feed and opens no session for it"
    );

    // ── …and a receiving device can decode and verify them ────────────────────────────────
    let outcome = workspace
        .apply_remote_entry(RemoteEntry {
            album_id,
            manifest_cbor: &entry.manifest_cbor,
            metadata_blob: &bundle.metadata_blob,
            original_ciphertext: &bundle.original.to_vec().unwrap(),
            local_chain_head: None,
        })
        .expect("applying is not a workspace failure");
    let SyncApplyOutcome::Applied(facts) = outcome else {
        panic!("the entry the server served must verify, got {outcome:?}");
    };
    assert_eq!(facts.asset_id, asset_id);
    assert_eq!(facts.album_id, album_id);
    assert!(
        facts.sidecar.is_some(),
        "a create carries its decrypted sidecar, which means the metadata blob really opened"
    );

    // ── Drop the index: the ledger the real ladder wrote rebuilds the row (#697) ───────────
    rebuild_matches(&fixture, &library_user, &asset_id.to_string()).await;
}

/// Rebuild the fixture's ledger and blobs into an empty index and compare `asset`'s row with
/// the one the server built while the ladder ran.
///
/// The ledger here was written by the real finalization path, so this is what proves the
/// entries a server writes are the entries a rebuild can replay: the role tokens, the instants,
/// the sequence numbers and the manifest the provenance entry names.
async fn rebuild_matches(fixture: &Fixture, owner: &capsule_server::store::UserId, asset: &str) {
    use capsule_server::attestation::{PublishedKey, ReceiptSigner as _};
    use capsule_server::gc::Mode;
    use capsule_server::index::memory::InMemoryAssetIndex;
    use capsule_server::ledger::rebuild::{RebuildContext, rebuild};
    use capsule_server::ledger::replay::LedgerKeys;

    let empty = std::sync::Arc::new(InMemoryAssetIndex::new());
    let keys = LedgerKeys::new(vec![PublishedKey {
        key_id: fixture.attestation_key.key_id(),
        public: fixture.attestation_key.verifying_key(),
        active_from: Timestamp::UNIX_EPOCH,
        active_to: None,
    }]);
    let context = RebuildContext::new(
        empty.clone(),
        fixture.blobs.clone(),
        fixture.ledger.clone(),
        std::sync::Arc::new(capsule_server::quota::InMemoryQuota::new()),
        keys,
        fixture.clock.clone(),
    );
    let report = rebuild(&context, Mode::Apply)
        .await
        .expect("a rebuild pass");
    let owner = capsule_server::store::OwnerId::new(owner.as_str());
    let owner_report = report
        .owners
        .iter()
        .find(|report| report.owner == owner)
        .expect("the owner's ledger was replayed");
    assert_eq!(owner_report.chain_fault, None);
    assert!(
        !owner_report.epoch_bump_needed,
        "a ledger the real path wrote proves its sequence: {owner_report:?}"
    );

    let asset = AssetId::new(asset);
    let original = fixture
        .index
        .read(&asset)
        .await
        .expect("the index answers")
        .expect("the pushed row");
    let rebuilt = empty
        .read(&asset)
        .await
        .expect("the index answers")
        .unwrap_or_else(|| panic!("the rebuild restored the row: {owner_report:?}"));
    assert_eq!(rebuilt.blobs, original.blobs);
    assert_eq!(
        (rebuilt.sync_seq, rebuilt.first_seq),
        (original.sync_seq, original.first_seq)
    );
    assert_eq!(rebuilt.album_id, original.album_id);
    assert_eq!(rebuilt.chain_head, original.chain_head);
    assert_eq!(rebuilt.state, original.state);
}

/// E2E case 4 (server half, SDK client): a peer pulls a shared album over a socket.
///
/// What only a socket can prove for the federated path: the capability is presentable under the
/// **one** `bearer` component the generated client attaches credentials by — the whole reason
/// the second scheme registers under the same name — the album-scoped page and its opaque
/// cursor round-trip through JSON, the blob comes back through the generated byte-serving
/// operation and verifies against its own address, and a revocation the home server publishes
/// stops the pull on the client's own fail-closed rule rather than on a refusal it stumbles
/// into.
#[tokio::test]
async fn e2e_case_4_a_peer_pulls_a_shared_album_through_the_sdk_over_a_socket() {
    use capsule_sdk::federation::{FederationError, FederationPull};
    use capsule_server::federation::{
        CapabilityRecord, CapabilityStore as _, MintRequest, PeerId, Scope,
    };
    use capsule_server::membership::{MemberRole, MembershipStore as _, RosterRecord};
    use capsule_server::store::UserId;

    const PEER: &str = "other.test";
    const BOB: &str = "01937b7c-0000-7000-8000-0000000000b0";

    use capsule_server::album::{AlbumRecord, AlbumStore as _};

    let fixture = Fixture::working();
    // The album row itself: a peer's page is bound to the album's *owner*, which is a fact of
    // the album store rather than of the index.
    fixture
        .albums
        .provision(AlbumRecord {
            album_id: album(),
            owner_id: owner(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            upgrade: None,
            created_at: Timestamp::UNIX_EPOCH,
        })
        .await
        .expect("the album store provisions");
    publish(&fixture, "case-4-one").await;
    let manifest = publish(&fixture, "case-4-two").await;
    let address = support::checksum(&manifest);
    fixture
        .members
        .apply_roster(
            RosterRecord {
                album_id: album(),
                roster_version: 1,
                amk_epoch: 1,
                attested_by_device: support::device(),
                received_at: Timestamp::UNIX_EPOCH,
                document: b"sdk-case-4".to_vec(),
            },
            vec![(UserId::new(BOB), MemberRole::Reader)],
        )
        .await
        .expect("the store applies");

    // A grant, recorded exactly as the mint route records one.
    let minted = fixture
        .codec
        .mint(&MintRequest {
            peer: PeerId::new(PEER),
            album: album(),
            scope: Scope::Read,
            min_protocol_version: PROTOCOL_VERSION.to_owned(),
            ttl: jiff::SignedDuration::from_hours(6),
        })
        .expect("it mints");
    fixture
        .revocations
        .issue(CapabilityRecord {
            jti: minted.grant.jti.clone(),
            album_id: album(),
            peer_id: PeerId::new(PEER),
            member: UserId::new(BOB),
            scope: Scope::Read,
            granted_epoch: 1,
            min_protocol_version: PROTOCOL_VERSION.to_owned(),
            issued_at: minted.grant.issued_at,
            expires_at: minted.grant.expires_at,
            not_after: minted.grant.expires_at,
            revoked_at: None,
            refreshed_to: None,
        })
        .await
        .expect("the store records");

    let base_url = serve(&fixture).await;
    let pull = FederationPull::new(
        &base_url,
        album().as_str(),
        minted.token.clone(),
        minted.grant.jti.clone(),
    )
    .expect("the base url is a url");

    // The page: the album's entries, in order, with a cursor that resumes.
    let page = pull
        .page(&SyncCursor::start(), 10)
        .await
        .expect("the peer's page is served");
    assert_eq!(page.entries.len(), 2);
    assert!(
        page.entries
            .iter()
            .all(|entry| entry.album_id == album().as_str().as_bytes())
    );
    let resumed = pull
        .page(&page.next_cursor, 10)
        .await
        .expect("the cursor resumes");
    assert!(resumed.entries.is_empty());

    // The bytes, through the generated byte-serving operation, verified against the address.
    let fetched = pull
        .blob(&address, manifest.len() as u64)
        .await
        .expect("the blob is served");
    assert_eq!(fetched, manifest);

    // A revocation the home server publishes stops the pull on the client's own rule: the
    // snapshot is re-read because the fixture's list publishes a zero staleness bound only when
    // it has nothing to say, so the poll is forced here to make the check the client's.
    fixture
        .revocations
        .revoke_issued(&minted.grant.jti, fixture.clock.now())
        .await
        .expect("the issuer revokes");
    pull.poll_revocations()
        .await
        .expect("the list is published");
    assert!(
        matches!(pull.admit().await, Err(FederationError::Revoked)),
        "a revoked jti stops the pull before anything is presented"
    );
    assert!(matches!(
        pull.page(&SyncCursor::start(), 10).await,
        Err(FederationError::Revoked)
    ));
    assert!(matches!(
        pull.blob(&address, manifest.len() as u64).await,
        Err(FederationError::Revoked)
    ));
}

/// **The release gate's receipt fetch, against the real route** (issue #441).
///
/// `GET /v1/upload/{id}/receipt` serves the signed receipt as raw `application/cbor`. The SDK's
/// `StorageVerifyClient::fetch_receipt` used to decode it as base64-in-JSON — and its unit mock
/// served JSON, so the mock encoded the same wrong belief as the client and nothing caught it.
/// Only a socket shows the two ends agree: an upload is finalized through the router, the SDK
/// fetches its receipt over the wire, and the receipt verifies under the server's own
/// attestation key with the fields the uploader sent — the key itself fetched through the SDK
/// from the origin's `.well-known` record, not pinned from the fixture (issue #547).
#[tokio::test]
async fn the_sdk_fetches_a_custody_receipt_and_verifies_it_under_the_attestation_key() {
    use capsule_core::crypto::hash::hash_bytes;
    use capsule_core::crypto::keys::HybridSigningKey;
    use capsule_library::library::{BlobRole, ReceiptExpectations, verify_receipt};
    use capsule_sdk::verify::{StorageVerifyClient, VerifyError, VerifyTransport};

    let fixture = Fixture::working();
    let bearer = format!("Bearer {}", fixture.login().await.access_token);

    // ── Finalize one upload through the router ────────────────────────────────────────────
    let first = support::payload(b'a', 4096);
    let second = support::payload(b'b', 4096);
    let whole: Vec<u8> = first.iter().chain(second.iter()).copied().collect();
    let id = fixture.open_session(&whole, "original", &bearer).await;
    fixture
        .chunk(&id, 0, &first, &bearer)
        .send()
        .await
        .assert_status(kynos::http::StatusCode::NO_CONTENT);
    fixture
        .chunk(&id, 4096, &second, &bearer)
        .send()
        .await
        .assert_status(kynos::http::StatusCode::NO_CONTENT);

    // The bytes the log holds, read in-process — what the wire must carry verbatim.
    let held = fixture
        .client
        .get(&format!("/v1/upload/{id}/receipt"))
        .header("authorization", &bearer)
        .send()
        .await;
    held.assert_status(kynos::http::StatusCode::OK);
    let held = held.bytes().to_vec();

    // ── The SDK's own client, over the socket ─────────────────────────────────────────────
    let base_url = serve(&fixture).await;
    let live = session(&base_url).await;
    let client = StorageVerifyClient::new(VerifyTransport::with_session(
        live,
        format!("{base_url}/v1"),
    ));
    let upload_id = uuid::Uuid::parse_str(&id).expect("an upload id is a UUID");
    let receipt = client
        .fetch_receipt(upload_id)
        .await
        .expect("the SDK decodes the receipt the route serves");

    assert_eq!(
        receipt.to_canonical_cbor(),
        held,
        "the SDK holds the exact bytes the server signed"
    );
    assert_eq!(receipt.core.upload_id, id);

    let expectations = ReceiptExpectations {
        ciphertext_hash: hash_bytes(&whole),
        size: whole.len() as u64,
        role: BlobRole::Original,
        envelope_hash: receipt.core.envelope_hash,
    };
    let at = receipt
        .core
        .received_at
        .parse::<Timestamp>()
        .expect("an instant")
        .as_second();

    // The pin comes over the wire too (issue #547): the same client, built on the `/v1` API
    // root, fetches the attestation-key record from the origin's `.well-known`, and what it
    // pins is the key the server signs with.
    let pinned = client
        .fetch_attestation_keys()
        .await
        .expect("the SDK fetches the attestation keys from the origin");
    let signing = fixture.attestation_key.verifying_key().to_bytes();
    assert!(
        pinned.iter().any(|key| key.to_bytes() == signing),
        "the published history holds the server's signing key"
    );
    assert_eq!(
        verify_receipt(&receipt, &pinned, &expectations, at),
        Ok(()),
        "the fetched receipt verifies under the fetched attestation keys"
    );

    // The pin is load-bearing: the same receipt under any other key does not verify.
    let stranger = HybridSigningKey::generate().verifying_key();
    assert!(
        verify_receipt(&receipt, &[stranger], &expectations, at).is_err(),
        "a receipt verifies only under the key that signed it"
    );

    // The signed durability verdict for the same write verifies under the same fetched pin, and
    // binds the nonce the SDK sent (transfer/durability.md, Signed Storage Attestation).
    let query = capsule_sdk::verify::AssetQuery {
        asset_id: uuid::Uuid::parse_str(&receipt.core.asset_id).expect("an asset id is a UUID"),
        blob_hashes: vec![hash_bytes(&whole)],
    };
    let nonce = [0x6E; 32];
    let signed = client
        .verify_signed(std::slice::from_ref(&query), false, &nonce, &pinned)
        .await
        .expect("the server's signed verdict verifies in the SDK");
    assert_eq!(signed.attestation.core.nonce, nonce);
    assert_eq!(signed.verdicts.len(), 1);
    assert_eq!(signed.verdicts[0].asset_id, query.asset_id);
    assert_eq!(signed.verdicts[0].blobs[0].hash, hash_bytes(&whole));
    let err = client
        .verify_signed(
            std::slice::from_ref(&query),
            false,
            &nonce,
            &[HybridSigningKey::generate().verifying_key()],
        )
        .await
        .expect_err("a signed verdict verifies only under the key that signed it");
    assert!(matches!(err, VerifyError::Attestation(_)), "got {err:?}");

    // A session that never finalized has no receipt, and the SDK reports the stable code.
    let pending = fixture
        .open_session(&support::payload(b'c', 4096), "original", &bearer)
        .await;
    let err = client
        .fetch_receipt(uuid::Uuid::parse_str(&pending).expect("a UUID"))
        .await
        .expect_err("an unfinalized session has no receipt");
    let VerifyError::Status { status, code } = err else {
        panic!("expected a status refusal, got {err:?}");
    };
    assert_eq!(status, 409);
    assert_eq!(code.as_deref(), Some("error.upload.receipt_not_available"));
}

/// The SDK's device-directory publish, over a socket, against the server that anchors and
/// verifies it (`S-P1` over `S-C9`/`S-C42`).
///
/// A mock cannot prove this: the server requires the user identity verifying key beside the
/// document (`X-Capsule-Identity-Key`), anchors it on first publish, and verifies the document
/// under it — invariant 23's second clause. So what is asserted is that the SDK's publish
/// names the key the way the server reads it, that what it stored the SDK's fetch verifies
/// under the same key, and that the server's two refusals arrive as their typed variants.
#[tokio::test]
async fn the_sdk_publishes_and_fetches_a_device_directory_over_a_socket() {
    use capsule_core::crypto::keys::DeviceDirectory;
    use capsule_sdk::directory::{DirectoryClient, DirectoryError};
    use support::{device, identity_key, signed_directory_with_device, user};
    use uuid::Uuid;

    let fixture = Fixture::working();
    let base_url = serve(&fixture).await;
    let client = DirectoryClient::new(session(&base_url).await, &format!("{base_url}/v1/auth"));
    let identity = identity_key();
    let device_key = identity_key();
    let directory = |ik: &capsule_core::crypto::keys::HybridSigningKey, version: u64| {
        capsule_core::cbor::from_slice::<DeviceDirectory>(&signed_directory_with_device(
            ik,
            version,
            device(),
            &device_key,
            "2026-01-01T00:00:00Z",
        ))
        .expect("the fixture directory decodes")
    };

    let published = directory(&identity, 1);
    let version = client
        .publish(&published, &identity.verifying_key())
        .await
        .expect("a directory signed under the presented identity key is accepted");
    assert_eq!(version, 1);

    // What the server stored is the signed document, and it verifies under the anchored key.
    let user_id = Uuid::parse_str(user().as_str()).expect("the seeded account id is a uuid");
    let fetched = client
        .fetch(user_id, &identity.verifying_key())
        .await
        .expect("the published directory is served and verifies");
    assert_eq!(fetched, published, "the document round-trips verbatim");

    // Invariant 23's first clause: a version that does not advance is the typed conflict.
    let err = client
        .publish(&published, &identity.verifying_key())
        .await
        .expect_err("an unchanged version does not advance");
    assert!(matches!(err, DirectoryError::VersionConflict), "{err:?}");

    // Its second clause: once anchored, a document under any other key is refused, however
    // well it verifies under that key.
    let stranger = identity_key();
    let err = client
        .publish(&directory(&stranger, 2), &stranger.verifying_key())
        .await
        .expect_err("a key other than the anchored one is refused");
    assert!(matches!(err, DirectoryError::IdentityMismatch), "{err:?}");
    assert_eq!(err.error_code(), Some("error.directory.identity_mismatch"));

    // And a document that does not verify under the key presented beside it is malformed.
    let err = client
        .publish(&directory(&stranger, 2), &identity.verifying_key())
        .await
        .expect_err("a document signed under another key does not verify");
    assert!(matches!(err, DirectoryError::Malformed), "{err:?}");
}
