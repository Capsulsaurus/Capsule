//! The one suite every [`PulledAlbums`] adapter must pass.
//!
//! # The rules the suite exists to protect
//!
//! - **Everything written reads back as written.** The state — cursor, high-water mark, AMK
//!   high-water mark, every asset head with its superseded heads — the accepted entries in the
//!   order they were written, and the metadata blobs a page carried.
//! - **A commit is conditional and atomic.** A page validated from a position the album has
//!   moved past writes nothing: no entry, no blob, no state.
//! - **A restart forgets the position and nothing else.** The heads, the AMK high-water mark,
//!   the entries and the held metadata blobs stay, so the replayed feed is re-validated against
//!   what was already accepted (invariant 20).
//! - **A job is its peer and its album.** One album id under two peers is two jobs, and nothing
//!   one holds is visible through the other — a held metadata blob included.
//!
//! - **A commit is bounded by every subscriber's caching budget** ([`run_budget`]). A page that
//!   would leave a subscriber of its job past [`BUDGET`] on the peer, by holding a blob that
//!   subscriber does not already hold, writes nothing; a blob counts once per subscriber across
//!   all its albums on a peer, each peer is its own budget, and every subscriber of a shared job
//!   is held to its own.
//!
//! # Reusing a harness
//!
//! Every case scopes its jobs by a case name, so cases may share one store and [`run_all`]
//! does. [`run_budget`] needs a store assembled with a budget of [`BUDGET`] over the
//! subscription store it is handed.

use std::collections::BTreeMap;

use capsule_core::crypto::hash::{Hash32, hash_bytes};
use jiff::Timestamp;

use super::{
    AcceptedEntry, AssetHead, Change, Committed, OverBudget, PulledAlbum, PulledAlbums, PulledBlob,
    ValidatedPage,
};
use crate::federation::PeerId;
use crate::federation::capability::Scope;
use crate::federation::pull::JobKey;
use crate::federation::subscriptions::{CapabilityToken, Subscription, SubscriptionStore};
use crate::store::{AlbumId, StoreError, StoreFuture, UserId};

/// What the suite reads besides the port: what a commit wrote that the port's own reads do not
/// answer.
pub trait Inspect: PulledAlbums {
    /// Every entry written for `job`, in the order written.
    fn written_entries<'a>(&'a self, job: &'a JobKey) -> StoreFuture<'a, Vec<AcceptedEntry>>;

    /// The bytes of the metadata blob at `hash`, if `job` holds it.
    fn held_metadata_blob<'a>(
        &'a self,
        job: &'a JobKey,
        hash: &'a Hash32,
    ) -> StoreFuture<'a, Option<Vec<u8>>>;
}

/// Unwrap a store result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, StoreError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming pulled-album store must succeed at {doing}: {error}"),
    }
}

/// Whether a commit wrote, failing on an error or a budget refusal.
#[track_caller]
fn wrote(result: Result<Committed, StoreError>, doing: &str) -> bool {
    match ok(result, doing) {
        Committed::Written => true,
        Committed::Stale => false,
        Committed::OverBudget(over) => {
            panic!("a conforming pulled-album store refused {doing} for a budget: {over:?}")
        }
    }
}

/// `case`'s job pulling `album` from `peer`.
fn job(case: &str, peer: &str, album: &str) -> JobKey {
    JobKey::new(
        PeerId::new(format!("{peer}.{case}.test")),
        AlbumId::new(format!("{case}-{album}")),
    )
}

/// An instant `seconds` past an epoch every case shares.
fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(1_767_225_600 + seconds).expect("a representable instant")
}

fn hash(byte: u8) -> Hash32 {
    Hash32([byte; 32])
}

/// A head at `head` that has superseded nothing yet.
fn head(head: Hash32) -> AssetHead {
    AssetHead {
        head,
        superseded: Vec::new(),
        deleted: false,
        amk_version: 1,
        metadata_blob: None,
    }
}

/// An entry for `asset` at `sync_seq` that changed nothing but its position.
fn entry(asset: &str, sync_seq: u64) -> AcceptedEntry {
    AcceptedEntry {
        asset_id: asset.to_owned(),
        sync_seq,
        change: Change::Updated,
        changed_at: at(i64::try_from(sync_seq).unwrap_or(0)),
        manifest: Some(format!("{asset}@{sync_seq}").into_bytes()),
        head: Some(hash_bytes(format!("{asset}@{sync_seq}").as_bytes())),
        metadata_blob: None,
        blobs: Vec::new(),
        derivative_manifests: Vec::new(),
        original_held: false,
        purged: false,
    }
}

/// A page moving the album to `high_water`, carrying `entries` and nothing else.
fn page(high_water: u64, entries: Vec<AcceptedEntry>) -> ValidatedPage {
    ValidatedPage {
        entries,
        metadata_blobs: BTreeMap::new(),
        state: PulledAlbum {
            cursor: Some(format!("c{high_water}")),
            high_water: Some(high_water),
            ..PulledAlbum::default()
        },
    }
}

/// Run every case against `store`.
pub async fn run_all(store: &dyn Inspect) {
    an_album_never_pulled_reads_as_the_start(store).await;
    a_page_reads_back_exactly(store).await;
    a_commit_lands_only_from_the_position_it_was_validated_at(store).await;
    entries_accumulate_in_the_order_written(store).await;
    a_restart_forgets_the_position_and_keeps_the_heads(store).await;
    a_later_state_replaces_the_earlier_one(store).await;
    jobs_are_kept_apart(store).await;
}

async fn an_album_never_pulled_reads_as_the_start(store: &dyn Inspect) {
    let job = job("fresh", "home", "a");
    assert_eq!(ok(store.read(&job).await, "a read"), PulledAlbum::default());
    assert!(ok(store.written_entries(&job).await, "listing entries").is_empty());
    ok(store.restart(&job).await, "restarting a job never pulled");
    assert_eq!(
        ok(store.read(&job).await, "a read"),
        PulledAlbum::default(),
        "a restart of nothing leaves nothing"
    );
}

async fn a_page_reads_back_exactly(store: &dyn Inspect) {
    let job = job("exact", "home", "a");
    let metadata = b"an encrypted metadata blob".to_vec();
    let other_metadata = b"another encrypted metadata blob".to_vec();
    let metadata_hash = hash_bytes(&metadata);
    let other_hash = hash_bytes(&other_metadata);

    let created = AcceptedEntry {
        change: Change::Created,
        metadata_blob: Some(metadata_hash),
        blobs: vec![
            PulledBlob {
                role: "original",
                hash: hash(1),
                size: 4_000_000_000,
            },
            PulledBlob {
                role: "metadata",
                hash: metadata_hash,
                size: 26,
            },
            PulledBlob {
                role: "derivative",
                hash: hash(2),
                size: 1,
            },
            PulledBlob {
                role: "provenance",
                hash: hash(3),
                size: 0,
            },
        ],
        derivative_manifests: vec![b"first".to_vec(), Vec::new(), b"third".to_vec()],
        original_held: true,
        ..entry("photo", 1)
    };
    let deleted = AcceptedEntry {
        change: Change::Deleted,
        metadata_blob: Some(other_hash),
        ..entry("other", 2)
    };
    let purged = AcceptedEntry {
        change: Change::Deleted,
        manifest: None,
        head: None,
        purged: true,
        ..entry("gone", 3)
    };

    let mut assets = BTreeMap::new();
    assets.insert(
        "photo".to_owned(),
        AssetHead {
            superseded: vec![hash(10), hash(11), hash(12)],
            amk_version: u32::MAX,
            metadata_blob: Some(metadata_hash),
            ..head(hash(13))
        },
    );
    assets.insert(
        "other".to_owned(),
        AssetHead {
            deleted: true,
            metadata_blob: Some(other_hash),
            ..head(hash(14))
        },
    );
    let state = PulledAlbum {
        cursor: Some("an opaque cursor".to_owned()),
        high_water: Some(i64::MAX.unsigned_abs()),
        amk_version: Some(7),
        assets,
    };
    let written = ValidatedPage {
        entries: vec![created, deleted, purged],
        metadata_blobs: BTreeMap::from([
            (metadata_hash, metadata.clone()),
            (other_hash, other_metadata.clone()),
        ]),
        state: state.clone(),
    };
    assert!(wrote(
        store.commit(&job, None, written.clone()).await,
        "a first commit"
    ));

    assert_eq!(
        ok(store.read(&job).await, "a read"),
        state,
        "the state reads back field for field"
    );
    assert_eq!(
        ok(store.written_entries(&job).await, "listing entries"),
        written.entries,
        "every entry reads back field for field, in the order written"
    );
    assert_eq!(
        ok(
            store.held_metadata_blob(&job, &metadata_hash).await,
            "reading a metadata blob"
        ),
        Some(metadata)
    );
    assert_eq!(
        ok(
            store.held_metadata_blob(&job, &other_hash).await,
            "reading a metadata blob"
        ),
        Some(other_metadata)
    );
    assert_eq!(
        ok(
            store.held_metadata_blob(&job, &hash(99)).await,
            "reading a metadata blob never written"
        ),
        None
    );
}

async fn a_commit_lands_only_from_the_position_it_was_validated_at(store: &dyn Inspect) {
    let job = job("conditional", "home", "a");
    assert!(wrote(
        store.commit(&job, None, page(3, vec![entry("a", 3)])).await,
        "a first commit"
    ));

    // Validated from the start, but the album has moved to 3 since.
    let stale_metadata = b"metadata only a stale page carried".to_vec();
    let stale_hash = hash_bytes(&stale_metadata);
    let mut stale = page(5, vec![entry("b", 5)]);
    stale.metadata_blobs.insert(stale_hash, stale_metadata);
    stale.state.assets.insert("b".to_owned(), head(hash(5)));
    assert!(
        !wrote(store.commit(&job, None, stale).await, "a stale commit"),
        "a page validated against a position the album has left is refused"
    );
    // And from a position the album has not reached.
    assert!(!wrote(
        store.commit(&job, Some(4), page(5, Vec::new())).await,
        "a commit from ahead"
    ));

    let state = ok(store.read(&job).await, "a read");
    assert_eq!(state.high_water, Some(3));
    assert_eq!(state.cursor.as_deref(), Some("c3"));
    assert!(
        state.assets.is_empty(),
        "the refused page's heads are not held"
    );
    assert_eq!(
        ok(store.written_entries(&job).await, "listing entries"),
        vec![entry("a", 3)],
        "the refused page's entries are not held"
    );
    assert_eq!(
        ok(
            store.held_metadata_blob(&job, &stale_hash).await,
            "reading a refused blob"
        ),
        None,
        "the refused page's metadata blobs are not held"
    );

    assert!(wrote(
        store
            .commit(&job, Some(3), page(5, vec![entry("b", 5)]))
            .await,
        "a commit from the held position"
    ));
    assert_eq!(ok(store.read(&job).await, "a read").high_water, Some(5));
}

async fn entries_accumulate_in_the_order_written(store: &dyn Inspect) {
    let job = job("order", "home", "a");
    assert!(wrote(
        store
            .commit(&job, None, page(2, vec![entry("a", 1), entry("b", 2)]))
            .await,
        "a first commit"
    ));
    assert!(wrote(
        store
            .commit(&job, Some(2), page(4, vec![entry("a", 4)]))
            .await,
        "a second commit"
    ));
    ok(store.restart(&job).await, "a restart");
    // The feed restarted from zero: the same positions arrive again, after the ones held.
    assert!(wrote(
        store.commit(&job, None, page(1, vec![entry("a", 1)])).await,
        "a commit after a restart"
    ));
    assert_eq!(
        ok(store.written_entries(&job).await, "listing entries"),
        vec![entry("a", 1), entry("b", 2), entry("a", 4), entry("a", 1)],
        "in the order written, not by position in the feed"
    );
}

async fn a_restart_forgets_the_position_and_keeps_the_heads(store: &dyn Inspect) {
    let job = job("restart", "home", "a");
    let metadata = b"held across a restart".to_vec();
    let metadata_hash = hash_bytes(&metadata);
    let mut first = page(4, vec![entry("asset", 4)]);
    first.state.assets.insert(
        "asset".to_owned(),
        AssetHead {
            superseded: vec![hash(0)],
            metadata_blob: Some(metadata_hash),
            ..head(hash(1))
        },
    );
    first.state.amk_version = Some(2);
    first.metadata_blobs.insert(metadata_hash, metadata.clone());
    assert!(wrote(
        store.commit(&job, None, first.clone()).await,
        "a commit"
    ));

    ok(store.restart(&job).await, "a restart");
    let state = ok(store.read(&job).await, "a read");
    assert_eq!(state.cursor, None);
    assert_eq!(state.high_water, None);
    assert_eq!(state.amk_version, Some(2), "the AMK high-water mark stays");
    assert_eq!(state.assets, first.state.assets, "every head stays");
    assert_eq!(
        ok(store.written_entries(&job).await, "listing entries"),
        first.entries,
        "the accepted entries stay"
    );
    assert_eq!(
        ok(
            store.held_metadata_blob(&job, &metadata_hash).await,
            "reading the held blob"
        ),
        Some(metadata),
        "the held metadata blob stays"
    );

    // The next pull starts from the start, and a page carrying no blob keeps the held one.
    assert!(
        !wrote(
            store.commit(&job, Some(4), page(5, Vec::new())).await,
            "a commit from the forgotten position"
        ),
        "the forgotten position is no position"
    );
    let mut replay = page(1, Vec::new());
    replay.state.assets = first.state.assets.clone();
    replay.state.amk_version = Some(2);
    assert!(wrote(
        store.commit(&job, None, replay).await,
        "a commit from the start"
    ));
    assert!(
        ok(
            store.held_metadata_blob(&job, &metadata_hash).await,
            "reading the held blob"
        )
        .is_some(),
        "a page that carries no blob does not drop the one held"
    );
}

async fn a_later_state_replaces_the_earlier_one(store: &dyn Inspect) {
    let job = job("replace", "home", "a");
    let mut first = page(1, Vec::new());
    first.state.assets.insert("kept".to_owned(), head(hash(1)));
    first.state.assets.insert("moved".to_owned(), head(hash(2)));
    first
        .state
        .assets
        .insert("dropped".to_owned(), head(hash(3)));
    first.state.amk_version = Some(1);
    assert!(wrote(
        store.commit(&job, None, first.clone()).await,
        "a commit"
    ));

    let mut second = page(2, Vec::new());
    second.state.cursor = None;
    second.state.amk_version = None;
    second
        .state
        .assets
        .insert("kept".to_owned(), first.state.assets["kept"].clone());
    second.state.assets.insert(
        "moved".to_owned(),
        first.state.assets["moved"].advanced(AssetHead {
            deleted: true,
            amk_version: 3,
            metadata_blob: Some(hash(8)),
            ..head(hash(4))
        }),
    );
    second.state.assets.insert("new".to_owned(), head(hash(5)));
    assert!(wrote(
        store.commit(&job, Some(1), second.clone()).await,
        "a second commit"
    ));
    assert_eq!(
        ok(store.read(&job).await, "a read"),
        second.state,
        "the state is the last page's, an asset it no longer names included"
    );
}

async fn jobs_are_kept_apart(store: &dyn Inspect) {
    let mine = job("apart", "home", "a");
    let other_peer = job("apart", "elsewhere", "a");
    let other_album = job("apart", "home", "b");
    assert_eq!(mine.album, other_peer.album, "one album id under two peers");

    let metadata = b"held by one job only".to_vec();
    let metadata_hash = hash_bytes(&metadata);
    let mut written = page(6, vec![entry("asset", 6)]);
    written
        .state
        .assets
        .insert("asset".to_owned(), head(hash(6)));
    written.metadata_blobs.insert(metadata_hash, metadata);
    assert!(wrote(store.commit(&mine, None, written).await, "a commit"));
    assert!(wrote(
        store.commit(&other_album, None, page(9, Vec::new())).await,
        "another album's commit"
    ));

    for other in [&other_peer, &other_album] {
        assert!(
            ok(store.read(other).await, "a read").assets.is_empty(),
            "{other} holds no head of another job"
        );
        assert!(
            ok(store.written_entries(other).await, "listing entries").is_empty(),
            "{other} holds no entry of another job"
        );
        assert_eq!(
            ok(
                store.held_metadata_blob(other, &metadata_hash).await,
                "reading another job's blob"
            ),
            None,
            "{other} does not hold another job's metadata blob"
        );
    }
    assert_eq!(
        ok(store.read(&other_peer).await, "a read"),
        PulledAlbum::default()
    );
    assert_eq!(
        ok(store.read(&mine).await, "a read").high_water,
        Some(6),
        "another album's commit moves only its own position"
    );

    ok(store.restart(&other_album).await, "restarting another job");
    assert_eq!(
        ok(store.read(&mine).await, "a read").high_water,
        Some(6),
        "another job's restart moves only its own position"
    );
}

/// The caching budget, in bytes, a store handed to [`run_budget`] is assembled with.
pub const BUDGET: u64 = 100;

/// Run every caching-budget case against `store`, which charges the subscribers
/// `subscriptions` holds against [`BUDGET`].
pub async fn run_budget(store: &dyn Inspect, subscriptions: &dyn SubscriptionStore) {
    a_page_past_a_subscribers_budget_writes_nothing(store, subscriptions).await;
    a_blob_counts_once_across_a_subscribers_albums_on_a_peer(store, subscriptions).await;
    each_peer_is_its_own_budget(store, subscriptions).await;
    every_subscriber_of_a_shared_job_is_held_to_its_own_budget(store, subscriptions).await;
    a_page_adding_nothing_new_is_written_past_the_budget(store, subscriptions).await;
}

/// `case`'s `user` subscribed to `job`.
async fn subscribe(subscriptions: &dyn SubscriptionStore, case: &str, user: &str, job: &JobKey) {
    ok(
        subscriptions
            .put(Subscription {
                subscriber: subscriber(case, user),
                peer: job.peer.clone(),
                album: job.album.clone(),
                scope: Scope::Read,
                jti: format!("{case}-{user}-{job}"),
                token: CapabilityToken::new(format!("token-{case}-{user}-{job}")),
                issued_at: at(0),
                expires_at: at(86_400),
                min_protocol_version: "2026-06-01".to_owned(),
                subscribed_at: at(0),
                updated_at: at(0),
            })
            .await,
        "subscribing",
    );
}

fn subscriber(case: &str, user: &str) -> UserId {
    UserId::new(format!("{case}-{user}"))
}

/// A metadata blob of `size` bytes, distinct per `tag`.
fn blob(tag: u8, size: usize) -> (Hash32, Vec<u8>) {
    let bytes = vec![tag; size];
    (hash_bytes(&bytes), bytes)
}

/// A page moving the album to `high_water`, holding `blobs`.
fn holding(high_water: u64, blobs: &[&(Hash32, Vec<u8>)]) -> ValidatedPage {
    let mut page = page(high_water, vec![entry("a", high_water)]);
    page.metadata_blobs = blobs
        .iter()
        .map(|(hash, bytes)| (*hash, bytes.clone()))
        .collect();
    page
}

async fn a_page_past_a_subscribers_budget_writes_nothing(
    store: &dyn Inspect,
    subscriptions: &dyn SubscriptionStore,
) {
    let case = "over";
    let job = job(case, "home", "a");
    subscribe(subscriptions, case, "u", &job).await;
    let first = blob(1, 60);
    let second = blob(2, 50);
    let third = blob(3, 40);
    assert!(wrote(
        store.commit(&job, None, holding(1, &[&first])).await,
        "a commit within the budget"
    ));

    let mut refused = holding(2, &[&first, &second]);
    refused.state.assets.insert("a".to_owned(), head(hash(7)));
    assert_eq!(
        ok(
            store.commit(&job, Some(1), refused).await,
            "a commit past the budget"
        ),
        Committed::OverBudget(OverBudget {
            subscriber: subscriber(case, "u"),
            peer: job.peer.clone(),
            used: 110,
            budget: BUDGET,
        }),
        "the page names whom it would take past the budget, and by how much"
    );
    let state = ok(store.read(&job).await, "a read");
    assert_eq!(state.high_water, Some(1), "the refused page moved nothing");
    assert!(state.assets.is_empty(), "the refused page wrote no head");
    assert_eq!(
        ok(store.written_entries(&job).await, "listing entries").len(),
        1,
        "the refused page wrote no entry, and the held one stays"
    );
    assert_eq!(
        ok(
            store.held_metadata_blob(&job, &second.0).await,
            "reading a refused blob"
        ),
        None,
        "the refused page's blob is not held"
    );
    assert_eq!(
        ok(
            store.held_metadata_blob(&job, &first.0).await,
            "reading a held blob"
        ),
        Some(first.1.clone()),
        "what was held before the refusal is still held"
    );

    // Exactly at the budget is within it, and a page holding nothing new is never refused.
    assert!(wrote(
        store
            .commit(&job, Some(1), holding(2, &[&first, &third]))
            .await,
        "a commit to exactly the budget"
    ));
    assert!(wrote(
        store
            .commit(&job, Some(2), holding(3, &[&first, &third]))
            .await,
        "a commit holding nothing new"
    ));
}

async fn a_blob_counts_once_across_a_subscribers_albums_on_a_peer(
    store: &dyn Inspect,
    subscriptions: &dyn SubscriptionStore,
) {
    let case = "dedup";
    let (one, two) = (job(case, "home", "a"), job(case, "home", "b"));
    subscribe(subscriptions, case, "u", &one).await;
    subscribe(subscriptions, case, "u", &two).await;
    let shared = blob(4, 70);
    let more = blob(5, 40);
    assert!(wrote(
        store.commit(&one, None, holding(1, &[&shared])).await,
        "the first album's commit"
    ));
    assert!(
        wrote(
            store.commit(&two, None, holding(1, &[&shared])).await,
            "the second album's commit of the same blob"
        ),
        "a blob the subscriber already holds on the peer adds nothing to its usage"
    );
    assert_eq!(
        ok(
            store.commit(&two, Some(1), holding(2, &[&more])).await,
            "a commit past the budget"
        ),
        Committed::OverBudget(OverBudget {
            subscriber: subscriber(case, "u"),
            peer: two.peer.clone(),
            used: 110,
            budget: BUDGET,
        }),
        "the shared blob is counted once, not once per album"
    );
}

async fn each_peer_is_its_own_budget(store: &dyn Inspect, subscriptions: &dyn SubscriptionStore) {
    let case = "peers";
    let (home, other) = (job(case, "home", "a"), job(case, "other", "a"));
    subscribe(subscriptions, case, "u", &home).await;
    subscribe(subscriptions, case, "u", &other).await;
    assert!(wrote(
        store.commit(&home, None, holding(1, &[&blob(6, 90)])).await,
        "one peer's commit"
    ));
    assert!(
        wrote(
            store
                .commit(&other, None, holding(1, &[&blob(7, 90)]))
                .await,
            "another peer's commit"
        ),
        "what one peer holds for a subscriber is not charged to its budget on another"
    );
}

async fn every_subscriber_of_a_shared_job_is_held_to_its_own_budget(
    store: &dyn Inspect,
    subscriptions: &dyn SubscriptionStore,
) {
    let case = "shared";
    let (shared, own) = (job(case, "home", "shared"), job(case, "home", "own"));
    subscribe(subscriptions, case, "u", &shared).await;
    subscribe(subscriptions, case, "v", &shared).await;
    subscribe(subscriptions, case, "v", &own).await;
    assert!(wrote(
        store.commit(&own, None, holding(1, &[&blob(8, 80)])).await,
        "the second subscriber's own album"
    ));
    assert_eq!(
        ok(
            store
                .commit(&shared, None, holding(1, &[&blob(9, 30)]))
                .await,
            "a shared album's commit"
        ),
        Committed::OverBudget(OverBudget {
            subscriber: subscriber(case, "v"),
            peer: shared.peer.clone(),
            used: 110,
            budget: BUDGET,
        }),
        "a blob held for a shared job is charged in full to each subscriber, and one past its \
         budget refuses the page however much room another has"
    );
    assert!(
        wrote(
            store
                .commit(&shared, None, holding(1, &[&blob(10, 20)]))
                .await,
            "a shared album's smaller commit"
        ),
        "within every subscriber's budget, the shared page is written"
    );
}

async fn a_page_adding_nothing_new_is_written_past_the_budget(
    store: &dyn Inspect,
    subscriptions: &dyn SubscriptionStore,
) {
    let case = "past";
    let (first, second, fresh) = (
        job(case, "home", "first"),
        job(case, "home", "second"),
        job(case, "home", "fresh"),
    );
    subscribe(subscriptions, case, "p", &first).await;
    subscribe(subscriptions, case, "q", &second).await;
    let held = blob(11, 60);
    let other = blob(12, 60);
    assert!(wrote(
        store.commit(&first, None, holding(1, &[&held])).await,
        "one subscriber's album"
    ));
    assert!(wrote(
        store.commit(&second, None, holding(1, &[&other])).await,
        "another subscriber's album"
    ));

    // Subscribed after the fact to both albums, the subscriber already uses 120 of its 100.
    subscribe(subscriptions, case, "u", &first).await;
    subscribe(subscriptions, case, "u", &second).await;
    subscribe(subscriptions, case, "u", &fresh).await;
    assert!(
        wrote(
            store.commit(&fresh, None, holding(1, &[&held])).await,
            "a page whose only new blob the subscriber holds through another album"
        ),
        "a page new to its job but adding nothing to a subscriber past its budget is written"
    );
    assert_eq!(
        ok(
            store.held_metadata_blob(&fresh, &held.0).await,
            "reading the admitted blob"
        ),
        Some(held.1.clone()),
        "the admitted page's blob is held for its job"
    );
    assert_eq!(
        ok(
            store
                .commit(&fresh, Some(1), holding(2, &[&held, &blob(13, 1)]))
                .await,
            "a page adding one byte past the budget"
        ),
        Committed::OverBudget(OverBudget {
            subscriber: subscriber(case, "u"),
            peer: fresh.peer.clone(),
            used: 121,
            budget: BUDGET,
        }),
        "past the budget, any growth is still refused"
    );
}
