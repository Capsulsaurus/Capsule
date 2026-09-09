//! **E2E case 8** — the album upgrade ceremony, the server leg through the SDK.
//!
//! An admin initiates the upgrade → quiesce: a signed `UpgradeIntent` from a device in the
//! published directory is proposed through `capsule_sdk::upgrade`, the phase reads back
//! in flight, a write that does not name the ceremony is refused with the ceremony's code,
//! the abort clears it, and the same write then lands.
//!
//! The client ceremony (drain → tombstone → fork → replay, and resume-from-crash) stays in
//! `capsule-core`'s in-process suite: a `Workspace` keeps its device signing key private and
//! cannot sign an intent, so the proposer here is the harness-held device the directory names.

use capsule_core::crypto::primitives::CRYPTO_SUITE_ID;
use capsule_core::crypto::upgrade::{SignedUpgradeIntent, UpgradeIntent};
use capsule_e2e::push::push_asset;
use capsule_e2e::{Device, PROTOCOL_VERSION, Server, entry_for};
use capsule_sdk::push::{bundle_blobs, create_request};
use capsule_sdk::upgrade::UpgradeClient;
use capsule_sdk::upload::UploadError;
use uuid::Uuid;

#[tokio::test]
async fn e2e_case_8_a_proposed_upgrade_quiesces_the_album_until_it_is_aborted() {
    let server = Server::boot().await;
    let mut a = Device::register(&server, "admin").await;
    let album = a.workspace.default_album_id();
    let generated = a.generated(&server);

    let intent = UpgradeIntent {
        intent_id: Uuid::now_v7(),
        from_protocol_version: PROTOCOL_VERSION.to_owned(),
        to_protocol_version: "2030-01-01".to_owned(),
        from_suite_id: CRYPTO_SUITE_ID,
        to_suite_id: CRYPTO_SUITE_ID,
        proposer_user: a.user_id,
        proposer_device: a.proposer_id,
        deadline_secs: 300,
    };
    let intent_id = intent.intent_id;
    let proposer_sig = a
        .proposer
        .sign(&intent.signing_bytes().expect("an intent encodes"));
    let signed = capsule_core::cbor::to_canonical_vec(&SignedUpgradeIntent {
        intent,
        proposer_sig,
    })
    .expect("a signed intent serializes");

    let phase = UpgradeClient::new(a.session.clone(), server.base_url())
        .begin(album, &signed)
        .await
        .expect("a signed proposal from a directory device is accepted");
    assert_eq!(phase.album_id, album);
    assert_eq!(phase.intent_id, Some(intent_id));
    assert_eq!(
        phase.in_flight, 0,
        "nothing was mid-flight when the album quiesced"
    );
    assert!(phase.expires_at.is_some());

    let read = generated
        .album_upgrade_phase(album.to_string(), PROTOCOL_VERSION, None)
        .await
        .expect("the phase reads")
        .into_inner();
    assert_eq!(
        read.intent_id.as_deref(),
        Some(intent_id.to_string().as_str())
    );
    assert_eq!(read.to_protocol_version.as_deref(), Some("2030-01-01"));

    // A write that does not name the ceremony is refused with its code and the live intent.
    let asset = a.import_jpeg("during-quiesce.jpg");
    let bundle = a.workspace.upload_bundle(&asset).expect("a bundle");
    let blobs = bundle_blobs(&bundle);
    let (blob, hash) = blobs.first().expect("a T0 blob");
    let request = create_request(&bundle, blob, hash);
    assert!(request.intent_id.is_none());
    let refused = a
        .upload_client(&server)
        .create_session(&request)
        .await
        .expect_err("a quiescing album refuses a write that names no ceremony");
    match &refused {
        UploadError::Rejected { status, code, .. } => {
            assert_eq!(*status, 409);
            assert_eq!(code.as_deref(), Some("error.upload.album_quiescing"));
        }
        other => panic!("expected the ceremony's refusal, got {other:?}"),
    }

    // Abort: the phase clears and the same write lands.
    let aborted = generated
        .abort_album_upgrade(
            album.to_string(),
            intent_id.to_string(),
            PROTOCOL_VERSION,
            None,
        )
        .await
        .expect("the proposer aborts")
        .into_inner();
    assert_eq!(aborted.intent_id, None);
    let cleared = generated
        .album_upgrade_phase(album.to_string(), PROTOCOL_VERSION, None)
        .await
        .expect("the phase reads")
        .into_inner();
    assert_eq!(cleared.intent_id, None);

    push_asset(&a, &server, &asset).await;
    let feed = a.feed(&server).await;
    assert!(
        entry_for(&feed, &asset).is_some(),
        "the write resumed after the abort"
    );
}
