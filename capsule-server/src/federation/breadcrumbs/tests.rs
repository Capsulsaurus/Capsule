use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::provenance::Action;

use super::*;
use crate::federation::PeerId;
use crate::federation::pull::InMemoryPulledAlbums;
use crate::federation::pull::fixtures::{album, asset, core, record};

fn job() -> JobKey {
    JobKey::new(PeerId::new("home.test"), album())
}

fn entry(change: Change, manifest: Option<Vec<u8>>) -> AcceptedEntry {
    AcceptedEntry {
        asset_id: asset(1).to_string(),
        sync_seq: 1,
        change,
        changed_at: "2026-05-31T13:00:00Z".parse().expect("an instant"),
        head: manifest.as_deref().map(hash_bytes),
        manifest,
        metadata_blob: None,
        blobs: Vec::new(),
        derivative_manifests: Vec::new(),
        original_held: true,
        purged: false,
    }
}

#[tokio::test]
async fn the_in_memory_store_conforms() {
    conformance::run_all(&InMemoryPulledAlbums::new()).await;
}

#[test]
fn a_breadcrumb_is_read_from_the_head_record() {
    let mut written = core(asset(1), Action::Create, None, 1);
    written.plaintext_size = 4096;
    written.created_by_user = uuid::Uuid::from_u128(42);
    written.timestamp = "2026-05-30T08:30:00Z".to_owned();
    let bytes = record(written);
    let crumb = breadcrumb(&job(), &entry(Change::Created, Some(bytes.clone())))
        .expect("it reads")
        .expect("a live asset");
    assert_eq!(
        crumb,
        Breadcrumb {
            job: job(),
            asset_id: asset(1).to_string(),
            hash: hash_bytes(&bytes),
            timestamp: "2026-05-30T08:30:00Z".parse().expect("an instant"),
            author: uuid::Uuid::from_u128(42),
            size: 4096,
        },
        "the record's own time, not the feed's"
    );
}

#[test]
fn a_delete_a_purge_or_a_bare_entry_leaves_no_breadcrumb() {
    let created = record(core(asset(1), Action::Create, None, 1));
    let deleted = record(core(
        asset(1),
        Action::Delete,
        Some(hash_bytes(&created)),
        1,
    ));
    assert_eq!(
        breadcrumb(&job(), &entry(Change::Deleted, Some(deleted))).expect("reads"),
        None
    );
    let mut purged = entry(Change::Deleted, None);
    purged.purged = true;
    assert!(!leaves_a_breadcrumb(&purged));
    assert_eq!(breadcrumb(&job(), &purged).expect("reads"), None);
    let mut headless = entry(Change::Created, Some(created));
    headless.head = None;
    assert!(!leaves_a_breadcrumb(&headless));
    assert_eq!(breadcrumb(&job(), &headless).expect("reads"), None);
}

#[test]
fn a_held_record_that_does_not_parse_is_corrupt() {
    let error = breadcrumb(&job(), &entry(Change::Created, Some(b"not cbor".to_vec())))
        .expect_err("a damaged row");
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
}

#[tokio::test]
async fn a_read_never_answers_more_than_the_page_bound() {
    use crate::federation::pull::PulledAlbums as _;
    use crate::federation::pull::pulled::{Committed, PulledAlbum, ValidatedPage};

    let store = InMemoryPulledAlbums::new();
    let bytes = record(core(asset(1), Action::Create, None, 1));
    let entries: Vec<AcceptedEntry> = (0..=MAX_BREADCRUMB_PAGE)
        .map(|n| AcceptedEntry {
            asset_id: format!("asset-{n:05}"),
            sync_seq: u64::try_from(n).expect("small") + 1,
            ..entry(Change::Created, Some(bytes.clone()))
        })
        .collect();
    let page = ValidatedPage {
        entries,
        metadata_blobs: Default::default(),
        state: PulledAlbum::default(),
    };
    assert_eq!(
        store.commit(&job(), None, page).await.expect("commit"),
        Committed::Written
    );
    let crumbs = store
        .breadcrumbs(&job(), None, usize::MAX)
        .await
        .expect("read");
    assert_eq!(crumbs.len(), MAX_BREADCRUMB_PAGE);
    let rest = store
        .breadcrumbs(&job(), Some(&crumbs[MAX_BREADCRUMB_PAGE - 1].asset_id), 10)
        .await
        .expect("read");
    assert_eq!(rest.len(), 1);
}
