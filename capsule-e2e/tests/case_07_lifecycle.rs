//! **E2E case 7** — full lifecycle.
//!
//! Create → metadata update → trash → restore → re-delete → hard purge after retention. The
//! provenance chain advances through every transition, and the server refuses purge before
//! `retention_until`.
//!
//! Every transition is authored by the real library, posted as a lifecycle op through the
//! generated client, and observed by an incremental feed reader. The retention floor is the
//! one the library *signed*: a 30-day tombstone is retained by the collector, a zero-day
//! tombstone is purged, on the operator worker over the same stores the router serves. (The
//! server never purges an unsigned floor — absent is "never", not "now".)

use capsule_e2e::push::{post_lifecycle_head, push_asset};
use capsule_e2e::{Device, PAGE_SIZE, PROTOCOL_VERSION, Server, entry_for};
use capsule_sdk::fetch::{FetchError, HttpBlobSource, fetch_blob};
use capsule_sdk::sync::{ChangeKind, FeedEntry, SyncConsumer, SyncState};
use capsule_server::gc::{Mode, purge_expired};
use uuid::Uuid;

/// The next page of the incremental reader: what changed since the last pull.
async fn next(consumer: &SyncConsumer, state: &mut SyncState) -> Vec<FeedEntry> {
    consumer
        .pull_into(state, PAGE_SIZE)
        .await
        .expect("the feed answers")
        .entries
}

fn kind_of(entries: &[FeedEntry], asset: &Uuid) -> ChangeKind {
    entry_for(entries, asset)
        .unwrap_or_else(|| panic!("{asset} changed since the last pull"))
        .kind
}

#[tokio::test]
async fn e2e_case_7_the_chain_advances_through_every_transition_and_purge_honours_retention() {
    let server = Server::boot().await;
    let mut a = Device::register(&server, "owner").await;
    let kept = a.import_jpeg("kept.jpg");
    let purged = a.import_jpeg("purged.jpg");
    push_asset(&a, &server, &kept).await;
    let purged_bundle = push_asset(&a, &server, &purged).await.bundle;

    let consumer =
        SyncConsumer::with_session(server.base_url(), a.session.clone()).expect("a consumer");
    let mut state = SyncState::new(PROTOCOL_VERSION);
    let created = next(&consumer, &mut state).await;
    assert_eq!(kind_of(&created, &kept), ChangeKind::Created);
    assert_eq!(kind_of(&created, &purged), ChangeKind::Created);

    // Metadata update: the caption changes the sealed metadata blob and the chain head.
    a.workspace
        .set_caption(&kept, "the one we keep")
        .expect("the caption sets");
    let op = post_lifecycle_head(&a, &server, &kept).await;
    assert_eq!(op.action, "metadata-update");
    assert!(!op.replayed);
    assert_eq!(
        kind_of(&next(&consumer, &mut state).await, &kept),
        ChangeKind::Updated
    );

    // Trash with a 30-day signed floor.
    a.workspace
        .soft_delete(&kept, 30)
        .expect("the asset trashes");
    let op = post_lifecycle_head(&a, &server, &kept).await;
    assert_eq!(op.action, "delete");
    assert_eq!(
        kind_of(&next(&consumer, &mut state).await, &kept),
        ChangeKind::Deleted
    );

    // Restore.
    a.workspace.restore(&kept).expect("the asset restores");
    let op = post_lifecycle_head(&a, &server, &kept).await;
    assert_eq!(op.action, "trash-restore");
    assert_ne!(
        kind_of(&next(&consumer, &mut state).await, &kept),
        ChangeKind::Deleted
    );

    // Re-delete, again with the 30-day floor; the other asset with a floor that is already due.
    a.workspace
        .soft_delete(&kept, 30)
        .expect("the asset trashes again");
    assert_eq!(
        post_lifecycle_head(&a, &server, &kept).await.action,
        "delete"
    );
    a.workspace
        .soft_delete(&purged, 0)
        .expect("the asset trashes");
    assert_eq!(
        post_lifecycle_head(&a, &server, &purged).await.action,
        "delete"
    );
    let deleted = next(&consumer, &mut state).await;
    assert_eq!(kind_of(&deleted, &kept), ChangeKind::Deleted);
    assert_eq!(kind_of(&deleted, &purged), ChangeKind::Deleted);

    // The chain the library holds is the one the server applied: five records for `kept`.
    let chain = &a.workspace.asset(&kept).expect("the asset").chain;
    assert_eq!(chain.records().len(), 5);

    // A replay of the same head is idempotent, not a stale-chain refusal.
    assert!(post_lifecycle_head(&a, &server, &kept).await.replayed);

    // Purge on the operator worker: the 30-day floor is honoured, the due one is purged.
    let report = purge_expired(&server.assembled.maintenance.collection, Mode::Apply, 10)
        .await
        .expect("the purge runs");
    let names = |ids: &[capsule_server::store::AssetId]| -> Vec<String> {
        ids.iter().map(ToString::to_string).collect()
    };
    assert_eq!(
        names(&report.retained),
        vec![kept.to_string()],
        "{report:?}"
    );
    assert_eq!(
        names(&report.purged),
        vec![purged.to_string()],
        "{report:?}"
    );

    // The purged original is gone to a reader; the retained tombstone's bytes still stand.
    let source = HttpBlobSource::new(a.session.clone(), server.v1());
    let gone = fetch_blob(
        &source,
        &purged_bundle.ciphertext_hash.to_hex(),
        purged_bundle.ciphertext.len() as u64,
    )
    .await
    .expect_err("a purged original no longer serves");
    assert!(matches!(gone, FetchError::Gone), "got {gone:?}");
    assert!(
        server
            .blob_path(
                &a.workspace
                    .upload_bundle(&kept)
                    .expect("a bundle")
                    .ciphertext_hash
                    .to_hex()
            )
            .exists(),
        "the retained tombstone's bytes are untouched"
    );
}
