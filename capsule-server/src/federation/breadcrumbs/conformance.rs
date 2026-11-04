//! The one suite every [`BreadcrumbIndex`] adapter must pass, over the entries its
//! [`PulledAlbums`] commits.
//!
//! # The rules the suite exists to protect
//!
//! - **One breadcrumb per live asset, from its latest entry.** An asset updated twice has one
//!   breadcrumb, at its newest head; one whose latest entry is a delete or a purged tombstone
//!   has none, and a restored one has one again.
//! - **A replayed feed adds nothing.** After a restart the feed re-serves what was accepted;
//!   the entries are written again and the breadcrumbs are the same.
//! - **Pages are bounded and ordered.** By asset id in byte order, after the cursor, at most the
//!   limit, never past [`MAX_BREADCRUMB_PAGE`].
//! - **A job is its peer and its album.** One album id under two peers is two indexes.
//!
//! # Reusing a store
//!
//! Every case scopes its jobs by a case name, so cases may share one store.

use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::provenance::Action;
use jiff::Timestamp;

use super::{Breadcrumb, BreadcrumbIndex, MAX_BREADCRUMB_PAGE};
use crate::federation::PeerId;
use crate::federation::pull::JobKey;
use crate::federation::pull::fixtures::{album, asset, core, record};
use crate::federation::pull::pulled::{
    AcceptedEntry, Change, Committed, PulledAlbum, PulledAlbums, ValidatedPage,
};

/// What the suite drives: a pulled-album store that is its own breadcrumb index.
pub(crate) trait Indexed: PulledAlbums + BreadcrumbIndex {}

impl<T: PulledAlbums + BreadcrumbIndex> Indexed for T {}

/// `case`'s job on `peer`.
fn job(case: &str, peer: &str) -> JobKey {
    JobKey::new(PeerId::new(format!("{peer}.{case}.test")), album())
}

/// The accepted entry for `record` of asset `n` at `sync_seq`.
fn accepted(n: u128, sync_seq: u64, change: Change, record: Option<Vec<u8>>) -> AcceptedEntry {
    AcceptedEntry {
        asset_id: asset(n).to_string(),
        sync_seq,
        change,
        changed_at: "2026-05-31T12:00:00Z".parse().expect("an instant"),
        head: record.as_deref().map(hash_bytes),
        manifest: record,
        metadata_blob: None,
        blobs: Vec::new(),
        derivative_manifests: Vec::new(),
        original_held: true,
        purged: false,
    }
}

/// A `create` of asset `n`.
fn create(n: u128) -> Vec<u8> {
    record(core(asset(n), Action::Create, None, 1))
}

/// `action` on asset `n` on top of `prior`.
fn next(n: u128, action: Action, prior: &[u8]) -> Vec<u8> {
    record(core(asset(n), action, Some(hash_bytes(prior)), 1))
}

/// Commit `entries` to `job` as its next page, from wherever it is.
async fn write(store: &dyn Indexed, job: &JobKey, entries: Vec<AcceptedEntry>) {
    let from = store.read(job).await.expect("read").high_water;
    let high_water = entries.iter().map(|entry| entry.sync_seq).max().or(from);
    let page = ValidatedPage {
        entries,
        metadata_blobs: Default::default(),
        state: PulledAlbum {
            cursor: high_water.map(|seq| format!("c{seq}")),
            high_water,
            ..PulledAlbum::default()
        },
    };
    assert_eq!(
        store.commit(job, from, page).await.expect("commit"),
        Committed::Written
    );
}

async fn read(
    store: &dyn Indexed,
    job: &JobKey,
    after: Option<&str>,
    limit: usize,
) -> Vec<Breadcrumb> {
    store
        .breadcrumbs(job, after, limit)
        .await
        .expect("a conforming index reads its breadcrumbs")
}

/// Asset ids and heads, for comparing pages.
fn heads(breadcrumbs: &[Breadcrumb]) -> Vec<(String, Hash32)> {
    breadcrumbs
        .iter()
        .map(|crumb| (crumb.asset_id.clone(), crumb.hash))
        .collect()
}

/// One breadcrumb per live asset, from its latest entry.
pub(crate) async fn each_live_asset_has_one_breadcrumb_at_its_head(store: &dyn Indexed) {
    let job = job("latest", "home");
    assert!(
        read(store, &job, None, 10).await.is_empty(),
        "nothing pulled"
    );
    let (one, two) = (create(1), create(2));
    let edited = next(1, Action::MetadataUpdate, &one);
    write(
        store,
        &job,
        vec![
            accepted(1, 1, Change::Created, Some(one)),
            accepted(2, 2, Change::Created, Some(two.clone())),
        ],
    )
    .await;
    write(
        store,
        &job,
        vec![accepted(1, 3, Change::Updated, Some(edited.clone()))],
    )
    .await;

    let crumbs = read(store, &job, None, 10).await;
    assert_eq!(
        crumbs,
        vec![
            Breadcrumb {
                job: job.clone(),
                asset_id: asset(1).to_string(),
                hash: hash_bytes(&edited),
                timestamp: "2026-05-31T12:00:00Z"
                    .parse::<Timestamp>()
                    .expect("an instant"),
                author: uuid::Uuid::from_u128(7),
                size: 1024,
            },
            Breadcrumb {
                job: job.clone(),
                asset_id: asset(2).to_string(),
                hash: hash_bytes(&two),
                timestamp: "2026-05-31T12:00:00Z"
                    .parse::<Timestamp>()
                    .expect("an instant"),
                author: uuid::Uuid::from_u128(7),
                size: 1024,
            },
        ]
    );
}

/// A deleted or purged asset leaves no breadcrumb; a restored one has one again.
pub(crate) async fn a_deleted_or_purged_asset_leaves_none(store: &dyn Indexed) {
    let job = job("deleted", "home");
    let (one, two, three) = (create(1), create(2), create(3));
    let deleted_one = next(1, Action::Delete, &one);
    let deleted_two = next(2, Action::Delete, &two);
    let restored = next(2, Action::TrashRestore, &deleted_two);
    let mut purged = accepted(3, 6, Change::Deleted, None);
    purged.purged = true;
    write(
        store,
        &job,
        vec![
            accepted(1, 1, Change::Created, Some(one)),
            accepted(2, 2, Change::Created, Some(two)),
            accepted(3, 3, Change::Created, Some(three)),
            accepted(1, 4, Change::Deleted, Some(deleted_one)),
            accepted(2, 5, Change::Deleted, Some(deleted_two)),
            purged,
        ],
    )
    .await;
    assert!(
        read(store, &job, None, 10).await.is_empty(),
        "a delete and a purged tombstone leave nothing"
    );

    write(
        store,
        &job,
        vec![accepted(2, 7, Change::Updated, Some(restored.clone()))],
    )
    .await;
    assert_eq!(
        heads(&read(store, &job, None, 10).await),
        vec![(asset(2).to_string(), hash_bytes(&restored))],
        "a restore brings the breadcrumb back"
    );
}

/// The feed replayed after a restart writes its entries again and changes no breadcrumb.
pub(crate) async fn a_replayed_feed_adds_nothing(store: &dyn Indexed) {
    let job = job("replay", "home");
    let (one, two) = (create(1), create(2));
    let entries = vec![
        accepted(1, 1, Change::Created, Some(one)),
        accepted(2, 2, Change::Created, Some(two)),
    ];
    write(store, &job, entries.clone()).await;
    let before = read(store, &job, None, 10).await;
    store.restart(&job).await.expect("restart");
    write(store, &job, entries).await;
    assert_eq!(read(store, &job, None, 10).await, before);
    assert_eq!(before.len(), 2);
}

/// Pages are ordered by asset id, start after the cursor, and stop at the limit.
pub(crate) async fn pages_are_bounded_and_ordered(store: &dyn Indexed) {
    let job = job("pages", "home");
    write(
        store,
        &job,
        (1..=3)
            .rev()
            .map(|n| {
                accepted(
                    n,
                    4 - u64::try_from(n).expect("small"),
                    Change::Created,
                    Some(create(n)),
                )
            })
            .collect(),
    )
    .await;
    let ids = |crumbs: Vec<Breadcrumb>| {
        crumbs
            .into_iter()
            .map(|crumb| crumb.asset_id)
            .collect::<Vec<_>>()
    };
    let first = ids(read(store, &job, None, 2).await);
    assert_eq!(first, vec![asset(1).to_string(), asset(2).to_string()]);
    let rest = ids(read(store, &job, Some(&first[1]), 2).await);
    assert_eq!(rest, vec![asset(3).to_string()]);
    assert!(read(store, &job, Some(&rest[0]), 2).await.is_empty());
    assert!(read(store, &job, None, 0).await.is_empty(), "a zero limit");
    assert_eq!(
        read(store, &job, None, usize::MAX).await.len(),
        3,
        "a limit past the page bound reads what there is"
    );
    const { assert!(MAX_BREADCRUMB_PAGE >= 3) };
}

/// One album id under two peers is two indexes.
pub(crate) async fn a_job_is_its_peer_and_its_album(store: &dyn Indexed) {
    let (here, there) = (job("jobs", "home"), job("jobs", "other"));
    write(
        store,
        &here,
        vec![accepted(1, 1, Change::Created, Some(create(1)))],
    )
    .await;
    assert_eq!(read(store, &here, None, 10).await.len(), 1);
    assert!(read(store, &there, None, 10).await.is_empty());
}

/// Every case.
pub(crate) async fn run_all(store: &dyn Indexed) {
    each_live_asset_has_one_breadcrumb_at_its_head(store).await;
    a_deleted_or_purged_asset_leaves_none(store).await;
    a_replayed_feed_adds_nothing(store).await;
    pages_are_bounded_and_ordered(store).await;
    a_job_is_its_peer_and_its_album(store).await;
}
