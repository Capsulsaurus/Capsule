//! Tests for the share-link client (#622), against the shared in-process mock server
//! ([`crate::testmock`]) replaying the `POST /v1/shares` and `DELETE /v1/shares/{opaque_id}`
//! wire, problem documents included.
//!
//! | Case | Guarantee |
//! | --- | --- |
//! | `the_request_carries_the_publication_and_never_the_fragment` | the body is the publication's fields, and the URL secret never reaches the server |
//! | `a_passphrase_link_sends_its_wrapped_secret_as_base64` | the wrapped scope rides as the bytes `/wrapped-secret` serves back |
//! | `issue_posts_the_body_with_the_bearer` | the generated operation's method, path, credential and handshake |
//! | `a_malformed_refusal_carries_its_code` / `an_unavailable_store_carries_its_code` / `a_forbidden_issue_is_unauthorized` | refusals are switchable by code |
//! | `an_echo_for_another_link_is_refused` | the server cannot confirm a link this client did not send |
//! | `revoke_deletes_the_link` / `a_refused_revoke_carries_its_code` | the revoke path |
//! | `the_url_puts_the_secret_in_the_fragment` | the recipient URL's shape |
//! | `every_declared_issue_refusal_maps_to_its_class` / `every_declared_revoke_refusal_maps_to_its_class` | each documented status lands in its `ShareError` variant with its code |
//! | `a_session_that_cannot_mint_a_bearer_is_unauthorized` | a dead session is an auth failure, and nothing reaches the share routes |
//! | `an_unreachable_server_is_a_transport_failure` | a refused connection is not read as an auth failure |
//! | `an_undeclared_status_is_unexpected` | a status the document does not declare keeps its number |

use std::sync::{Arc, Mutex};

use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::sharing::{
    LINK_SECRET_LEN, ScopeMaterial, ShareLinkId, ShareScope, encapsulate_scope, generate_opaque_id,
    publish_share,
};
use capsule_i18n::error_codes;
use uuid::Uuid;

use super::*;
use crate::auth::{AuthClient, PersistedSession};
use crate::testmock::{MockRequest, MockResponse, MockServer};

fn fast() -> Argon2Params {
    Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    }
}

/// A link over one asset, behind `passphrase` when given, and what publishing it yields.
fn published(passphrase: Option<&str>) -> (ShareLink, SharePublication) {
    let file_id = Uuid::now_v7();
    let material = ScopeMaterial::Asset {
        file_id,
        file_key: [0x42; 32],
    };
    let opaque_id = generate_opaque_id();
    let fragment_secret = capsule_core::crypto::rng::random_array::<LINK_SECRET_LEN>();
    let wrapped_scope =
        encapsulate_scope(&material, &fragment_secret, &opaque_id, passphrase, fast()).unwrap();
    let expires_at = Some("2030-01-01T00:00:00Z".to_owned());
    let asset = capsule_core::sharing::SharedAsset {
        asset_id: file_id,
        content_hash: capsule_core::crypto::hash::hash_bytes(b"ciphertext"),
        content_type: "image/jpeg".to_owned(),
        size: 10,
        amk_version: 0,
        nonce_prefix: [0x07; capsule_core::crypto::encryption::NONCE_PREFIX_LEN],
        key_mode: capsule_core::crypto::provenance::KeyMode::Derived,
        wrapped_file_key: None,
    };
    let publication = publish_share(
        &wrapped_scope,
        &fragment_secret,
        &opaque_id,
        expires_at.clone(),
        vec![asset],
    )
    .unwrap();
    let link = ShareLink {
        link_id: ShareLinkId(Uuid::now_v7()),
        opaque_id,
        scope: ShareScope::Asset(file_id),
        fragment_secret,
        wrapped_scope,
        expires_at,
    };
    (link, publication)
}

fn session_for(base: &str) -> Session {
    AuthClient::new(base)
        .unwrap()
        .resume(PersistedSession {
            access_token: "test-access".to_owned().into(),
            refresh_token: "test-refresh".to_owned().into(),
            access_expires_at_unix: jiff::Timestamp::now().as_second() + 3_600,
        })
        .unwrap()
}

/// Start a mock that records every request and answers with `respond`, and a client over it.
async fn recording<F>(respond: F) -> (MockServer, ShareClient, Arc<Mutex<Vec<MockRequest>>>)
where
    F: Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
{
    let seen: Arc<Mutex<Vec<MockRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let server = MockServer::start(move |req| {
        sink.lock().unwrap().push(req.clone());
        respond(req)
    })
    .await;
    let client = ShareClient::new(session_for(&server.base_url()), &server.base_url()).unwrap();
    (server, client, seen)
}

/// A problem document as the server renders it.
fn problem(status: u16, reason: &str, code: &str) -> MockResponse {
    let mut response =
        MockResponse::new(status, reason).header("Content-Type", "application/problem+json");
    response.body = format!(
        r#"{{"type":"about:blank","title":{reason:?},"status":{status},"detail":"refused","code":{code:?}}}"#
    )
    .into_bytes();
    response
}

/// A session whose access token has expired and whose refresh the server refuses.
fn dead_session_for(base: &str) -> Session {
    AuthClient::new(base)
        .unwrap()
        .resume(PersistedSession {
            access_token: "test-access".to_owned().into(),
            refresh_token: "test-refresh".to_owned().into(),
            access_expires_at_unix: jiff::Timestamp::now().as_second() - 3_600,
        })
        .unwrap()
}

/// A `401` whose code is not the access-token refusal, so the client neither refreshes nor
/// replays it.
const REFUSED_CREDENTIAL: &str = "error.auth.invalid_credentials";

/// A refusal as the server renders it: a coded problem, or for the transport backstop's `413`
/// (`code` is `None`) no body at all.
fn refusal_response(status: u16, reason: &str, code: Option<&str>) -> MockResponse {
    code.map_or_else(
        || MockResponse::new(status, reason),
        |code| problem(status, reason, code),
    )
}

/// One declared refusal, and the `ShareError` class it must land in.
type RefusalCase = (
    u16,
    &'static str,
    Option<&'static str>,
    fn(&ShareError) -> bool,
);

fn created(opaque_id: &str) -> MockResponse {
    MockResponse::new(201, "Created").json_body(format!(r#"{{"opaque_id":"{opaque_id}"}}"#))
}

#[test]
fn the_request_carries_the_publication_and_never_the_fragment() {
    let (link, publication) = published(None);
    let request = issue_request(&link, &publication);
    assert_eq!(request.opaque_id, link.opaque_id_hex());
    assert_eq!(request.metadata_hash, publication.metadata_hash.to_hex());
    assert_eq!(
        request.serves,
        publication
            .serves
            .iter()
            .map(capsule_core::crypto::hash::Hash32::to_hex)
            .collect::<Vec<_>>()
    );
    assert_eq!(request.serves[0], request.metadata_hash);
    assert_eq!(request.wrapped_secret, None);
    assert_eq!(request.expires_at, link.expires_at);
    let record = BASE64
        .decode(request.metadata_blob.as_deref().expect("the sealed record"))
        .unwrap();
    assert_eq!(record, publication.metadata_blob);

    let wire = serde_json::to_string(&request).unwrap();
    assert!(
        !wire.contains(&link.fragment_hex()),
        "the fragment secret must never reach the server"
    );
}

#[test]
fn a_passphrase_link_sends_its_wrapped_secret_as_base64() {
    let (link, publication) = published(Some("hunter2"));
    let request = issue_request(&link, &publication);
    let sent = BASE64
        .decode(request.wrapped_secret.expect("a wrapped secret"))
        .unwrap();
    assert_eq!(Some(sent), publication.wrapped_secret);
}

#[tokio::test]
async fn issue_posts_the_body_with_the_bearer() {
    let (link, publication) = published(None);
    let id = link.opaque_id_hex();
    let (_server, client, seen) = recording(move |_| created(&id)).await;
    client.issue(&link, &publication).await.unwrap();

    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/shares");
    assert_eq!(request.header("authorization"), Some("Bearer test-access"));
    assert_eq!(
        request.header("x-capsule-protocol"),
        Some(capsule_core::crypto::primitives::PROTOCOL_VERSION)
    );
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(
        body,
        serde_json::to_value(issue_request(&link, &publication)).unwrap()
    );
}

#[tokio::test]
async fn a_malformed_refusal_carries_its_code() {
    let (link, publication) = published(None);
    let (_server, client, _) =
        recording(|_| problem(400, "Bad Request", error_codes::SHARE_MALFORMED)).await;
    let error = client.issue(&link, &publication).await.unwrap_err();
    assert!(matches!(error, ShareError::Malformed { .. }), "{error:?}");
    assert_eq!(error.error_code(), Some(error_codes::SHARE_MALFORMED));
}

#[tokio::test]
async fn an_unavailable_store_carries_its_code() {
    let (link, publication) = published(None);
    let (_server, client, _) =
        recording(|_| problem(500, "Internal Server Error", error_codes::SHARE_UNAVAILABLE)).await;
    let error = client.issue(&link, &publication).await.unwrap_err();
    assert!(matches!(error, ShareError::Unavailable { .. }), "{error:?}");
    assert_eq!(error.error_code(), Some(error_codes::SHARE_UNAVAILABLE));
}

#[tokio::test]
async fn a_forbidden_issue_is_unauthorized() {
    let (link, publication) = published(None);
    let (_server, client, _) =
        recording(|_| problem(403, "Forbidden", "error.auth.forbidden")).await;
    let error = client.issue(&link, &publication).await.unwrap_err();
    assert!(
        matches!(error, ShareError::Unauthorized { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_echo_for_another_link_is_refused() {
    let (link, publication) = published(None);
    let (_server, client, _) = recording(|_| created(&"0".repeat(32))).await;
    let error = client.issue(&link, &publication).await.unwrap_err();
    assert!(matches!(error, ShareError::WrongEcho { .. }), "{error:?}");
}

#[tokio::test]
async fn revoke_deletes_the_link() {
    let (link, _) = published(None);
    let (_server, client, seen) = recording(|_| MockResponse::new(204, "No Content")).await;
    client.revoke(&link.opaque_id_hex()).await.unwrap();

    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(
        requests[0].path,
        format!("/v1/shares/{}", link.opaque_id_hex())
    );
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer test-access")
    );
}

#[tokio::test]
async fn a_refused_revoke_carries_its_code() {
    let (_server, client, _) =
        recording(|_| problem(500, "Internal Server Error", error_codes::SHARE_UNAVAILABLE)).await;
    let error = client.revoke(&"a".repeat(32)).await.unwrap_err();
    assert_eq!(error.error_code(), Some(error_codes::SHARE_UNAVAILABLE));
}

#[tokio::test]
async fn every_declared_issue_refusal_maps_to_its_class() {
    let cases: [RefusalCase; 5] = [
        (401, "Unauthorized", Some(REFUSED_CREDENTIAL), |e| {
            matches!(e, ShareError::Unauthorized { .. })
        }),
        (
            415,
            "Unsupported Media Type",
            Some(error_codes::REQUEST_UNSUPPORTED_MEDIA_TYPE),
            |e| matches!(e, ShareError::Malformed { .. }),
        ),
        (
            422,
            "Unprocessable Entity",
            Some(error_codes::REQUEST_UNPROCESSABLE),
            |e| matches!(e, ShareError::Malformed { .. }),
        ),
        (
            426,
            "Upgrade Required",
            Some(error_codes::PROTOCOL_VERSION_UNSUPPORTED),
            |e| matches!(e, ShareError::Unexpected { status: 426, .. }),
        ),
        (413, "Payload Too Large", None, |e| {
            matches!(
                e,
                ShareError::Unexpected {
                    status: 413,
                    code: None
                }
            )
        }),
    ];
    for (status, reason, code, expected) in cases {
        let (link, publication) = published(None);
        let (_server, client, seen) =
            recording(move |_| refusal_response(status, reason, code)).await;
        let error = client.issue(&link, &publication).await.unwrap_err();
        assert!(expected(&error), "{status}: {error:?}");
        assert_eq!(error.error_code(), code, "{status}");
        assert_eq!(seen.lock().unwrap().len(), 1, "{status} is not replayed");
    }
}

#[tokio::test]
async fn every_declared_revoke_refusal_maps_to_its_class() {
    let cases: [RefusalCase; 5] = [
        (
            400,
            "Bad Request",
            Some(error_codes::SHARE_MALFORMED),
            |e| matches!(e, ShareError::Malformed { .. }),
        ),
        (401, "Unauthorized", Some(REFUSED_CREDENTIAL), |e| {
            matches!(e, ShareError::Unauthorized { .. })
        }),
        (
            403,
            "Forbidden",
            Some(error_codes::REQUEST_FORBIDDEN),
            |e| matches!(e, ShareError::Unauthorized { .. }),
        ),
        (
            426,
            "Upgrade Required",
            Some(error_codes::PROTOCOL_VERSION_UNSUPPORTED),
            |e| matches!(e, ShareError::Unexpected { status: 426, .. }),
        ),
        (413, "Payload Too Large", None, |e| {
            matches!(
                e,
                ShareError::Unexpected {
                    status: 413,
                    code: None
                }
            )
        }),
    ];
    for (status, reason, code, expected) in cases {
        let (_server, client, seen) =
            recording(move |_| refusal_response(status, reason, code)).await;
        let error = client.revoke(&"a".repeat(32)).await.unwrap_err();
        assert!(expected(&error), "{status}: {error:?}");
        assert_eq!(error.error_code(), code, "{status}");
        assert_eq!(seen.lock().unwrap().len(), 1, "{status} is not replayed");
    }
}

#[tokio::test]
async fn a_session_that_cannot_mint_a_bearer_is_unauthorized() {
    let (link, publication) = published(None);
    let seen: Arc<Mutex<Vec<MockRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    // Every request, the session's refresh included, is refused.
    let server = MockServer::start(move |req| {
        sink.lock().unwrap().push(req.clone());
        problem(401, "Unauthorized", error_codes::REQUEST_UNAUTHENTICATED)
    })
    .await;
    let client =
        ShareClient::new(dead_session_for(&server.base_url()), &server.base_url()).unwrap();

    let error = client.issue(&link, &publication).await.unwrap_err();
    assert!(
        matches!(error, ShareError::Unauthorized { code: None }),
        "a dead session must reach the caller as an auth failure, got {error:?}"
    );
    let error = client.revoke(&link.opaque_id_hex()).await.unwrap_err();
    assert!(
        matches!(error, ShareError::Unauthorized { code: None }),
        "{error:?}"
    );
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .all(|request| !request.path.starts_with("/v1/shares")),
        "nothing reaches the share routes without a bearer"
    );
}

#[tokio::test]
async fn an_unreachable_server_is_a_transport_failure() {
    // Bind, read the port, then drop the listener: the address now refuses connections.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let base = format!("http://{addr}");
    // The access token is still valid, so the session never needs the dead origin.
    let client = ShareClient::new(session_for(&base), &base).unwrap();

    let (link, publication) = published(None);
    let error = client.issue(&link, &publication).await.unwrap_err();
    assert!(matches!(error, ShareError::Transport(_)), "{error:?}");
    assert_eq!(error.error_code(), None);
    let error = client.revoke(&link.opaque_id_hex()).await.unwrap_err();
    assert!(matches!(error, ShareError::Transport(_)), "{error:?}");
}

#[tokio::test]
async fn an_undeclared_status_is_unexpected() {
    let (link, publication) = published(None);
    let (_server, client, _) = recording(|_| MockResponse::new(502, "Bad Gateway")).await;
    let error = client.issue(&link, &publication).await.unwrap_err();
    assert!(
        matches!(
            error,
            ShareError::Unexpected {
                status: 502,
                code: None
            }
        ),
        "{error:?}"
    );
    let error = client.revoke(&link.opaque_id_hex()).await.unwrap_err();
    assert!(
        matches!(
            error,
            ShareError::Unexpected {
                status: 502,
                code: None
            }
        ),
        "{error:?}"
    );
}

#[test]
fn the_url_puts_the_secret_in_the_fragment() {
    let (link, _) = published(None);
    let url = share_url("https://capsule.example.com/", &link);
    assert_eq!(
        url,
        format!(
            "https://capsule.example.com/s/{}#{}",
            link.opaque_id_hex(),
            link.fragment_hex()
        )
    );
}
