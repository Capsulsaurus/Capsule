//! The lifecycle op verbs against the crate's in-process mock server: what a foreign record
//! must be before it is sent, the hold on an asset the feed does not list, and a purge's
//! delivery — the re-issue with the delete it chains onto, and a retry from the kept chain
//! after the workspace is reopened.

use std::sync::{Arc, Mutex};

use capsule_core::crypto::provenance::{Action, ProvenanceRecord};

use super::*;
use crate::ffi::{FfiCapsuleClient, FfiClientBuild, FfiDeviceTier, FfiLoginOutcome};
use crate::testmock::{MockRequest, MockResponse, MockServer};

const PROTOCOL: &str = "2026-07-10";
const PASSPHRASE: &[u8] = b"test-passphrase";

fn client_build() -> FfiClientBuild {
    FfiClientBuild {
        client_id: "capsule-ffi-test".into(),
        semver: "0.0.1".into(),
    }
}

fn token_json() -> String {
    serde_json::json!({
        "access_token": "access-1",
        "refresh_token": "refresh-1",
        "token_type": "Bearer",
        "expires_by": jiff::Timestamp::now().as_second() + 3600,
    })
    .to_string()
}

/// A workspace enrolled at `root`, holding one sealed asset; returns it and the asset id.
fn sealed(root: &std::path::Path) -> (Arc<FfiWorkspace>, String) {
    let workspace = FfiWorkspace::create(
        root.to_string_lossy().into_owned(),
        PASSPHRASE.to_vec(),
        FfiDeviceTier::LowRam,
        client_build(),
    )
    .expect("enrollment succeeds");
    let album = workspace
        .ensure_album(workspace.default_album_id().unwrap(), "Camera Roll".into())
        .unwrap();
    let mut plaintext: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    plaintext[..3].copy_from_slice(b"\xFF\xD8\xFF");
    let asset = workspace
        .seal_asset(album, "IMG_0001.JPG".into(), plaintext)
        .unwrap();
    (workspace, asset)
}

fn reopen(root: &std::path::Path) -> Arc<FfiWorkspace> {
    FfiWorkspace::open(
        root.to_string_lossy().into_owned(),
        PASSPHRASE.to_vec(),
        FfiDeviceTier::LowRam,
        client_build(),
    )
    .expect("the library reopens")
}

/// A mock that logs in, refreshes, and answers every lifecycle op with `ops`, recording the
/// op requests it saw.
async fn ops_server<F>(ops: F) -> (MockServer, Arc<Mutex<Vec<MockRequest>>>)
where
    F: Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
{
    let seen: Arc<Mutex<Vec<MockRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let server = MockServer::start(move |req: &MockRequest| {
        match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/auth/login" | "/auth/refresh") => {
                MockResponse::new(200, "OK").json_body(token_json())
            }
            ("POST", path) if path.starts_with("/v1/albums/") && path.ends_with("/ops") => {
                sink.lock().unwrap().push(req.clone());
                ops(req)
            }
            _ => MockResponse::new(404, "Not Found").json_body(r#"{"error":"no route"}"#),
        }
    })
    .await;
    (server, seen)
}

async fn login(server: &MockServer) -> Arc<FfiSession> {
    let outcome = FfiCapsuleClient::new(
        format!("{}/auth", server.base_url()),
        format!("{}/upload", server.base_url()),
        PROTOCOL.to_string(),
        None,
    )
    .unwrap()
    .login("a@example.com".into(), "pw".into())
    .await
    .unwrap();
    match outcome {
        FfiLoginOutcome::Session { session } => session,
        FfiLoginOutcome::SecondFactorRequired { .. } => panic!("no second factor expected"),
    }
}

/// The success body for the op `req` carries, at `sync_seq`.
fn applied(req: &MockRequest, sync_seq: u64) -> MockResponse {
    let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
    let envelope = &body["manifest_envelope"];
    MockResponse::new(200, "OK").json_body(
        serde_json::json!({
            "asset_id": envelope["file_id"],
            "sync_seq": sync_seq,
            "action": envelope["action"],
            "replayed": false,
        })
        .to_string(),
    )
}

fn problem(status: u16, code: &str) -> MockResponse {
    MockResponse::new(status, "Refused").json_body(
        serde_json::json!({
            "type": "about:blank",
            "title": "Refused",
            "status": status,
            "detail": "refused",
            "code": code,
        })
        .to_string(),
    )
}

/// The record a sent op carries, decoded from its `manifest_cbor`.
fn sent_record(req: &MockRequest) -> ProvenanceRecord {
    use base64::Engine as _;
    let body: crate::rest::types::OpRequest = serde_json::from_slice(&req.body).unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(body.manifest_cbor)
        .unwrap();
    capsule_core::cbor::from_slice(&bytes).unwrap()
}

/// The create record the feed would list for `asset` before any lifecycle op reached the
/// server: the head of a freshly sealed asset.
fn create_head(workspace: &FfiWorkspace, asset: &str) -> Vec<u8> {
    workspace.provenance_head(asset.to_owned()).unwrap()
}

fn invalid(result: Result<ProvenanceRecord, FfiError>) -> String {
    match result {
        Err(FfiError::InvalidArgument { detail }) => detail,
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

// ─── The boundary ────────────────────────────────────────────────────────────

/// Foreign bytes are a record only when they are a delete or restore in its own canonical
/// encoding, within the bound; every other shape is refused before anything is sent.
#[test]
fn a_foreign_record_is_judged_before_it_is_sent() {
    let root = tempfile::TempDir::new().unwrap();
    let (workspace, asset) = sealed(root.path());
    let create = create_head(&workspace, &asset);
    workspace.delete_asset(asset.clone(), 30).unwrap();
    let delete = workspace.provenance_head(asset).unwrap();

    let record = decode_op_record(&delete).expect("a signed delete is a lifecycle op");
    assert_eq!(record.manifest.core.action, Action::Delete);

    assert!(invalid(decode_op_record(&create)).contains("only a delete or a trash-restore"));
    let mut trailing = delete.clone();
    trailing.push(0);
    invalid(decode_op_record(&trailing));
    invalid(decode_op_record(&[]));
    invalid(decode_op_record(b"not cbor at all"));
    assert!(
        invalid(decode_op_record(&vec![0; MAX_OP_RECORD_BYTES + 1])).contains("bound"),
        "an oversized record is refused before it is decoded"
    );
}

/// A record whose bytes decode but are not the record's canonical encoding is refused: the
/// server's chain head is the hash of the bytes, so only the canonical ones chain.
#[test]
fn a_non_canonical_encoding_of_a_record_is_refused() {
    let root = tempfile::TempDir::new().unwrap();
    let (workspace, asset) = sealed(root.path());
    workspace.delete_asset(asset.clone(), 30).unwrap();
    let delete = workspace.provenance_head(asset).unwrap();
    // The same record with its outer map or array re-headed as indefinite-length: a decoder
    // reads the same value, and canonical CBOR forbids the layout.
    let head = delete[0];
    let indefinite = match head >> 5 {
        4 if head & 0x1f < 24 => 0x9f,
        5 if head & 0x1f < 24 => 0xbf,
        _ => panic!("a record is a short array or map at the top, not {head:#x}"),
    };
    let mut other = vec![indefinite];
    other.extend_from_slice(&delete[1..]);
    other.push(0xff);
    let decoded: ProvenanceRecord = capsule_core::cbor::from_slice(&other)
        .expect("an indefinite-length encoding still decodes");
    assert_eq!(decoded, capsule_core::cbor::from_slice(&delete).unwrap());
    assert!(invalid(decode_op_record(&other)).contains("canonical"));
}

/// Copy the library at `from` to `to`, file by file: a second library of the same account and
/// device, whose chains diverge from the first from here on.
fn copy_library(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_library(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn refused(result: Result<FfiLifecycleOpOutcome, FfiError>) -> String {
    match result {
        Err(FfiError::InvalidArgument { detail }) => detail,
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

/// A record whose device signature does not verify is refused before it is sent, though it
/// is canonical, a delete, and otherwise the workspace's own: the server cannot verify it, and
/// once it were the chain head every reader's `verify_asset` would fail.
#[tokio::test]
async fn a_mis_signed_record_is_refused_before_it_is_sent() {
    let (server, seen) = ops_server(|req| applied(req, 1)).await;
    let session = login(&server).await;
    let root = tempfile::TempDir::new().unwrap();
    let (workspace, asset) = sealed(root.path());
    let create = create_head(&workspace, &asset);
    workspace.delete_asset(asset.clone(), 30).unwrap();
    let mut record: ProvenanceRecord =
        capsule_core::cbor::from_slice(&workspace.provenance_head(asset).unwrap()).unwrap();
    record.manifest.device_sig = record.manifest.write_sig.clone();
    let forged = capsule_core::cbor::to_canonical_vec(&record).unwrap();
    decode_op_record(&forged).expect("the bytes alone are a well-formed lifecycle op");

    let detail = refused(
        session
            .apply_lifecycle_op(server.base_url(), workspace.clone(), forged, Some(create))
            .await,
    );
    assert!(detail.contains("device signature"), "{detail}");
    assert!(
        seen.lock().unwrap().is_empty(),
        "nothing reached the server"
    );
}

/// A validly signed record that is not on this workspace's chain is refused: one of another
/// account, and one this device signed into a diverged copy of the library.
#[tokio::test]
async fn a_record_not_on_the_workspaces_chain_is_refused() {
    let (server, seen) = ops_server(|req| applied(req, 1)).await;
    let session = login(&server).await;
    let root = tempfile::TempDir::new().unwrap();
    let (own, copied) = (root.path().join("own"), root.path().join("copy"));
    let (workspace, asset) = sealed(&own);
    let create = create_head(&workspace, &asset);

    let (other, other_asset) = sealed(&root.path().join("other"));
    other.delete_asset(other_asset.clone(), 30).unwrap();
    let foreign = other.provenance_head(other_asset).unwrap();
    let detail = refused(
        session
            .apply_lifecycle_op(
                server.base_url(),
                workspace.clone(),
                foreign,
                Some(Vec::new()),
            )
            .await,
    );
    assert!(detail.contains("device signature"), "{detail}");

    drop(workspace);
    copy_library(&own, &copied);
    let workspace = reopen(&own);
    let copy = reopen(&copied);
    copy.delete_asset(asset.clone(), 30).unwrap();
    let diverged = copy.provenance_head(asset).unwrap();
    let detail = refused(
        session
            .apply_lifecycle_op(server.base_url(), workspace.clone(), diverged, Some(create))
            .await,
    );
    assert!(
        detail.contains("not a record of this workspace's chain"),
        "{detail}"
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "nothing reached the server"
    );
}

// ─── The session verbs ───────────────────────────────────────────────────────

/// A record of an asset the feed does not list is held: nothing reaches the server.
#[tokio::test]
async fn an_op_for_an_asset_the_feed_does_not_list_is_held() {
    let (server, seen) = ops_server(|req| applied(req, 1)).await;
    let session = login(&server).await;
    let root = tempfile::TempDir::new().unwrap();
    let (workspace, asset) = sealed(root.path());
    workspace.delete_asset(asset.clone(), 30).unwrap();
    let delete = workspace.provenance_head(asset.clone()).unwrap();

    let outcome = session
        .apply_lifecycle_op(server.base_url(), workspace.clone(), delete.clone(), None)
        .await
        .unwrap();
    assert_eq!(outcome, FfiLifecycleOpOutcome::NotIndexed);
    workspace.purge_asset(asset.clone()).unwrap();
    let outcome = session
        .deliver_purge(server.base_url(), workspace.clone(), asset, None)
        .await
        .unwrap();
    assert_eq!(outcome, FfiLifecycleOpOutcome::NotIndexed);
    assert!(seen.lock().unwrap().is_empty(), "no op reached the server");
}

/// A delete of an indexed asset reaches its album's ops route as it was signed.
#[tokio::test]
async fn an_op_for_an_indexed_asset_reaches_its_albums_ops_route() {
    let (server, seen) = ops_server(|req| applied(req, 7)).await;
    let session = login(&server).await;
    let root = tempfile::TempDir::new().unwrap();
    let (workspace, asset) = sealed(root.path());
    let create = create_head(&workspace, &asset);
    workspace.delete_asset(asset.clone(), 30).unwrap();
    let delete = workspace.provenance_head(asset.clone()).unwrap();

    let outcome = session
        .apply_lifecycle_op(
            format!("{}/", server.base_url()),
            workspace.clone(),
            delete.clone(),
            Some(create),
        )
        .await
        .unwrap();
    assert_eq!(
        outcome,
        FfiLifecycleOpOutcome::Applied {
            asset_id: asset.clone(),
            action: "delete".into(),
            sync_seq: 7,
            replayed: false,
        }
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let album = workspace.default_album_id().unwrap();
    assert_eq!(seen[0].path, format!("/v1/albums/{album}/ops"));
    let record: ProvenanceRecord = capsule_core::cbor::from_slice(&delete).unwrap();
    assert_eq!(sent_record(&seen[0]), record);
}

/// **The issue's first case.** An early purge's re-issue reaches the server applied, sent with
/// the delete it chains onto that the server had not seen, in chain order.
#[tokio::test]
async fn an_early_purge_delivers_its_reissued_delete() {
    let (server, seen) = ops_server(|req| applied(req, 10)).await;
    let session = login(&server).await;
    let root = tempfile::TempDir::new().unwrap();
    let (workspace, asset) = sealed(root.path());
    let create = create_head(&workspace, &asset);
    workspace.delete_asset(asset.clone(), 30).unwrap();
    let purged = workspace.purge_asset(asset.clone()).unwrap();
    let reissue: ProvenanceRecord =
        capsule_core::cbor::from_slice(&purged.reissue_record.expect("an early purge re-issues"))
            .unwrap();

    let outcome = session
        .deliver_purge(
            server.base_url(),
            workspace.clone(),
            asset.clone(),
            Some(create),
        )
        .await
        .unwrap();
    assert!(
        matches!(&outcome, FfiLifecycleOpOutcome::Applied { action, .. } if action == "delete"),
        "{outcome:?}"
    );
    let seen = seen.lock().unwrap();
    let sent: Vec<ProvenanceRecord> = seen.iter().map(sent_record).collect();
    assert_eq!(sent.len(), 2, "the held delete, then the re-issue");
    assert_eq!(sent[0].manifest.core.action, Action::Delete);
    assert_eq!(
        sent[1].manifest.core.prior_provenance_hash,
        Some(sent[0].record_hash()),
        "the re-issue chains onto the delete sent before it"
    );
    assert_eq!(
        sent[1], reissue,
        "the re-issue is the record the purge handed back"
    );
}

/// **The issue's second case.** A delivery that failed is retried from the kept chain after
/// the workspace is reopened, and the server then applies the re-issue.
#[tokio::test]
async fn a_failed_delivery_is_retried_from_the_kept_chain_after_a_reopen() {
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let failing = fail.clone();
    let (server, seen) = ops_server(move |req| {
        if failing.load(std::sync::atomic::Ordering::SeqCst) {
            problem(500, "error.internal")
        } else {
            applied(req, 11)
        }
    })
    .await;
    let session = login(&server).await;
    let root = tempfile::TempDir::new().unwrap();
    let (workspace, asset) = sealed(root.path());
    workspace.delete_asset(asset.clone(), 30).unwrap();
    // The server already holds the delete, so the re-issue alone is the stretch.
    let delete = workspace.provenance_head(asset.clone()).unwrap();
    let purged = workspace.purge_asset(asset.clone()).unwrap();
    let reissue = purged.reissue_record.expect("an early purge re-issues");

    let outcome = session
        .deliver_purge(
            server.base_url(),
            workspace.clone(),
            asset.clone(),
            Some(delete.clone()),
        )
        .await
        .unwrap();
    match outcome {
        FfiLifecycleOpOutcome::Failed {
            kind, permanent, ..
        } => {
            assert_eq!(kind, "unavailable");
            assert!(!permanent, "a server fault is retried");
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    drop(workspace);

    fail.store(false, std::sync::atomic::Ordering::SeqCst);
    let reopened = reopen(root.path());
    assert_eq!(reopened.purged_asset_ids().unwrap(), vec![asset.clone()]);
    assert_eq!(
        reopened.purged_record(asset.clone()).unwrap().as_deref(),
        Some(reissue.as_slice()),
        "the kept chain still holds the re-issue"
    );
    let outcome = session
        .deliver_purge(
            server.base_url(),
            reopened.clone(),
            asset.clone(),
            Some(delete),
        )
        .await
        .unwrap();
    assert!(
        matches!(outcome, FfiLifecycleOpOutcome::Applied { sync_seq: 11, .. }),
        "{outcome:?}"
    );
    // The single-record verb sends the same bytes from `purged_record`.
    let resent = session
        .apply_lifecycle_op(
            server.base_url(),
            reopened.clone(),
            reissue.clone(),
            Some(Vec::new()),
        )
        .await
        .unwrap();
    assert!(matches!(resent, FfiLifecycleOpOutcome::Applied { .. }));

    let seen = seen.lock().unwrap();
    let expected: ProvenanceRecord = capsule_core::cbor::from_slice(&reissue).unwrap();
    assert_eq!(seen.len(), 3, "the failed send, the retry, and the resend");
    assert!(seen.iter().all(|req| sent_record(req) == expected));
}

/// A refusal the server judged final stops the stretch and says so; a session the server will
/// not accept is an auth error, so the app signs in again.
#[tokio::test]
async fn refusals_are_typed() {
    let (server, seen) = ops_server(|_| {
        MockResponse::new(409, "Conflict").json_body(
            serde_json::json!({
                "type": "about:blank",
                "title": "Stale",
                "status": 409,
                "detail": "stale",
                "code": "error.upload.stale_revival",
                "chain_head": "ab".repeat(32),
            })
            .to_string(),
        )
    })
    .await;
    let session = login(&server).await;
    let root = tempfile::TempDir::new().unwrap();
    let (workspace, asset) = sealed(root.path());
    let create = create_head(&workspace, &asset);
    workspace.delete_asset(asset.clone(), 30).unwrap();
    workspace.purge_asset(asset.clone()).unwrap();
    let outcome = session
        .deliver_purge(
            server.base_url(),
            workspace.clone(),
            asset.clone(),
            Some(create),
        )
        .await
        .unwrap();
    assert_eq!(
        outcome,
        FfiLifecycleOpOutcome::Failed {
            kind: "stale_revival".into(),
            code: Some("error.upload.stale_revival".into()),
            permanent: true,
            chain_head: Some("ab".repeat(32)),
            retention_until: None,
            detail: "lifecycle op refused with status 409 (error.upload.stale_revival)".into(),
        }
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "the stretch stops at the refusal"
    );

    let (server, _) = ops_server(|_| problem(401, "error.auth.unauthorized")).await;
    let session = login(&server).await;
    let record = workspace.purged_record(asset).unwrap().unwrap();
    match session
        .apply_lifecycle_op(
            server.base_url(),
            workspace.clone(),
            record,
            Some(Vec::new()),
        )
        .await
    {
        Err(FfiError::Auth { .. }) => {}
        other => panic!("expected an auth error, got {other:?}"),
    }
}

#[tokio::test]
async fn bad_arguments_are_refused_before_anything_is_sent() {
    let (server, seen) = ops_server(|req| applied(req, 1)).await;
    let session = login(&server).await;
    let root = tempfile::TempDir::new().unwrap();
    let (workspace, asset) = sealed(root.path());

    let not_purged = session
        .deliver_purge(
            server.base_url(),
            workspace.clone(),
            asset.clone(),
            Some(Vec::new()),
        )
        .await;
    assert!(
        matches!(
            not_purged,
            Err(FfiError::Trash {
                refusal: crate::ffi::FfiTrashRefusal::NotFound,
                ..
            })
        ),
        "{not_purged:?}"
    );
    for (url, id) in [
        ("not a url".to_owned(), asset.clone()),
        ("ftp://example.com".to_owned(), asset.clone()),
        (server.base_url(), "not-a-uuid".to_owned()),
    ] {
        let result = session
            .deliver_purge(url, workspace.clone(), id, Some(Vec::new()))
            .await;
        assert!(
            matches!(result, Err(FfiError::InvalidArgument { .. })),
            "{result:?}"
        );
    }
    let create = create_head(&workspace, &asset);
    let result = session
        .apply_lifecycle_op(
            server.base_url(),
            workspace.clone(),
            create.clone(),
            Some(create),
        )
        .await;
    assert!(matches!(result, Err(FfiError::InvalidArgument { .. })));
    assert!(seen.lock().unwrap().is_empty());
}
