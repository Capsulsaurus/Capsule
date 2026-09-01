//! The **SDK's own client**, over a socket, against the real router (slice `S-D28`).
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
use capsule_server::blob::{BlobStore, ContentAddress};
use capsule_server::index::{AssetIndex, BlobRecord, PendingAsset};
use capsule_server::store::{AssetId, BlobRole, Clock};
use jiff::Timestamp;
use support::{EMAIL, Fixture, PASSWORD, PROTOCOL_VERSION, album, owner};

/// A client ceiling above every seeded entry's pin, so nothing is forward-version refused.
const CLIENT_MAX_PROTOCOL: &str = "2099-12-31";

/// Bind the fixture's own context to an ephemeral port and return its base URL.
///
/// The listener serves the **same** context the fixture holds handles on, so an asset seeded
/// through `fixture.index` is an asset this server serves.
async fn serve(fixture: &Fixture) -> String {
    let service = capsule_server::service(fixture.app()).expect("the router builds");
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
    // it (`S-C36`, `S-C38`).
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
