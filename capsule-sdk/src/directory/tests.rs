//! Tests for the device-directory client (`S-P1` over the `S-C9` surface).
//!
//! Driven against the shared in-process mock server ([`crate::testmock`]), which replays the
//! real wire. What is pinned here:
//!
//! | Case | Guarantee |
//! | --- | --- |
//! | `publish_sends_the_signed_document_verbatim_as_cbor` | the bytes on the wire are the canonical CBOR core `capsule-core` signed — never re-encoded |
//! | `publish_returns_the_stored_version` | the caller learns the version the server now holds |
//! | `a_version_that_does_not_advance_carries_its_catalog_code` | invariant 23 is switchable by code, not status |
//! | `a_malformed_document_carries_its_catalog_code` | the 400 path |
//! | `fetch_verifies_under_the_pinned_user_ik` | the happy path returns the document |
//! | `a_foreign_signed_directory_is_refused` | fail-closed: a server cannot inject a device key |
//! | `a_missing_directory_is_not_published` | the 404 path is typed, not an unexpected status |

use capsule_core::crypto::keys::{DeviceEntry, DirectoryCore, HybridSigningKey};
use uuid::Uuid;

use super::*;
use crate::auth::{AuthClient, PersistedSession};
use crate::testmock::{MockRequest, MockResponse, MockServer};

const USER: u128 = 0x05E2;

/// The account's identity key plus one enrolled device — the smallest honest directory.
struct Fixture {
    ik: HybridSigningKey,
    device: HybridSigningKey,
    device_id: Uuid,
}

impl Fixture {
    fn new() -> Self {
        Self {
            ik: HybridSigningKey::generate(),
            device: HybridSigningKey::generate(),
            device_id: Uuid::now_v7(),
        }
    }

    fn directory_signed_by(&self, version: u64, signer: &HybridSigningKey) -> DeviceDirectory {
        DirectoryCore {
            user_id: Uuid::from_u128(USER),
            directory_version: version,
            updated_at: "2026-08-22T00:00:00Z".into(),
            devices: vec![DeviceEntry {
                device_id: self.device_id,
                dsk_public: self.device.verifying_key(),
                added_at: "2026-08-01T00:00:00Z".into(),
                revoked_at: None,
            }],
        }
        .sign(signer)
    }

    fn directory(&self, version: u64) -> DeviceDirectory {
        self.directory_signed_by(version, &self.ik)
    }
}

/// A directory client over a session resumed from fixed tokens (no login round trip needed —
/// the point under test is the directory wire, not the auth ceremony).
fn client_for(server: &MockServer) -> DirectoryClient {
    let auth = AuthClient::new(&server.base_url()).expect("auth client");
    let session = auth
        .resume(PersistedSession {
            access_token: "directory-access-token".into(),
            refresh_token: "refresh-token".into(),
            access_expires_at_unix: jiff::Timestamp::now().as_second() + 3600,
        })
        .expect("resume session");
    DirectoryClient::new(session, &server.base_url())
}

/// Start a mock that records every request it serves and answers with `respond`.
async fn recording<F>(
    respond: F,
) -> (
    MockServer,
    std::sync::Arc<std::sync::Mutex<Vec<MockRequest>>>,
)
where
    F: Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
{
    let seen: std::sync::Arc<std::sync::Mutex<Vec<MockRequest>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = seen.clone();
    let server = MockServer::start(move |req| {
        if let Ok(mut guard) = sink.lock() {
            guard.push(req.clone());
        }
        respond(req)
    })
    .await;
    (server, seen)
}

fn published(version: u64) -> MockResponse {
    MockResponse::new(200, "OK").json_body(format!(r#"{{"directory_version":{version}}}"#))
}

/// The document that crosses the wire must be the **exact** canonical CBOR the signature
/// covers: re-encoding it here would detach it from that signature, and the server stores the
/// bytes verbatim.
#[tokio::test]
async fn publish_sends_the_signed_document_verbatim_as_cbor() {
    let fixture = Fixture::new();
    let directory = fixture.directory(1);
    let (server, seen) = recording(|_| published(1)).await;

    client_for(&server).publish(&directory).await.unwrap();

    let seen = seen.lock().unwrap();
    let req = seen.first().expect("one request");
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/devices/directory");
    assert_eq!(req.header("Content-Type"), Some("application/cbor"));
    assert_eq!(
        req.header("Authorization"),
        Some("Bearer directory-access-token"),
        "the session bearer rides the publish"
    );
    assert_eq!(
        req.body,
        capsule_core::cbor::to_canonical_vec(&directory).unwrap(),
        "the body is the signed document byte-for-byte"
    );
    // And what the server received still verifies — the round trip is lossless.
    let echoed: DeviceDirectory = capsule_core::cbor::from_slice(&req.body).unwrap();
    assert!(echoed.verify(&fixture.ik.verifying_key()));
}

#[tokio::test]
async fn publish_returns_the_stored_version() {
    let fixture = Fixture::new();
    let (server, _) = recording(|_| published(7)).await;
    assert_eq!(
        client_for(&server)
            .publish(&fixture.directory(7))
            .await
            .unwrap(),
        7
    );
}

/// Invariant 23 (anti-rollback): a version that does not advance is a *typed* conflict carrying
/// its stable catalog code, so a client can re-fetch and re-publish rather than guessing at a
/// bare 409.
#[tokio::test]
async fn a_version_that_does_not_advance_carries_its_catalog_code() {
    let fixture = Fixture::new();
    let (server, _) = recording(|_| MockResponse::new(409, "Conflict")).await;

    let err = client_for(&server)
        .publish(&fixture.directory(1))
        .await
        .unwrap_err();
    assert!(matches!(err, DirectoryError::VersionConflict));
    assert_eq!(
        err.error_code(),
        Some(error_codes::DIRECTORY_VERSION_CONFLICT)
    );
}

#[tokio::test]
async fn a_malformed_document_carries_its_catalog_code() {
    let fixture = Fixture::new();
    let (server, _) = recording(|_| MockResponse::new(400, "Bad Request")).await;

    let err = client_for(&server)
        .publish(&fixture.directory(1))
        .await
        .unwrap_err();
    assert!(matches!(err, DirectoryError::Malformed));
    assert_eq!(err.error_code(), Some(error_codes::DIRECTORY_MALFORMED));
}

/// The happy fetch: served bytes decode and verify under the pinned identity key.
#[tokio::test]
async fn fetch_verifies_under_the_pinned_user_ik() {
    let fixture = Fixture::new();
    let directory = fixture.directory(3);
    let body = capsule_core::cbor::to_canonical_vec(&directory).unwrap();
    let (server, _) = recording(move |_| {
        let mut resp = MockResponse::new(200, "OK").header("Content-Type", "application/cbor");
        resp.body = body.clone();
        resp
    })
    .await;

    let fetched = client_for(&server)
        .fetch(Uuid::from_u128(USER), &fixture.ik.verifying_key())
        .await
        .unwrap();
    assert_eq!(fetched, directory);
    assert_eq!(fetched.core.directory_version, 3);
    assert_eq!(fetched.core.devices.len(), 1);
}

/// A directory signed by a **foreign** identity key is refused and never returned — otherwise a
/// server could introduce a device signing key of its own choosing into the trusted set, which
/// is precisely the attack the directory's signature exists to stop.
#[tokio::test]
async fn a_foreign_signed_directory_is_refused() {
    let fixture = Fixture::new();
    let foreign = HybridSigningKey::generate();
    let body =
        capsule_core::cbor::to_canonical_vec(&fixture.directory_signed_by(1, &foreign)).unwrap();
    let (server, _) = recording(move |_| {
        let mut resp = MockResponse::new(200, "OK");
        resp.body = body.clone();
        resp
    })
    .await;

    let err = client_for(&server)
        .fetch(Uuid::from_u128(USER), &fixture.ik.verifying_key())
        .await
        .unwrap_err();
    assert!(matches!(err, DirectoryError::UntrustedSignature));
}

#[tokio::test]
async fn a_missing_directory_is_not_published() {
    let fixture = Fixture::new();
    let (server, _) = recording(|_| MockResponse::new(404, "Not Found")).await;

    let err = client_for(&server)
        .fetch(Uuid::from_u128(USER), &fixture.ik.verifying_key())
        .await
        .unwrap_err();
    assert!(matches!(err, DirectoryError::NotPublished));
}
