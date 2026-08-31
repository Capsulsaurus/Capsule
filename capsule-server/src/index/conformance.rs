//! The one suite every [`AssetIndex`] adapter must pass.
//!
//! It lives in `src/` for the reason [`crate::store::conformance`] does: it is part of the
//! contract, not a test of one implementation, so it has to be runnable against an adapter
//! written elsewhere. Everything is generic over `&dyn AssetIndex`, so a Postgres-backed smoke
//! test is one [`run_all`] call and costs nothing in a binary that never makes one.
//!
//! # What this suite can prove about `S-C21`, and what it cannot
//!
//! The port's central claim is that a sequence number is allocated inside the same critical
//! section that makes its row readable, so a reader can never page past a number that is still
//! uncommitted. **A single-process suite cannot exhibit that race**, because the window it
//! would have to observe exists only between two concurrent transactions. Saying otherwise
//! would repeat the `S-C35` mistake in a new place: a conformance suite proves the operations
//! that exist behave consistently, never that a property holds under conditions it cannot
//! create.
//!
//! So the suite asserts the property's **observable consequences** — every minted number is
//! reachable through the feed, numbers are strictly increasing per owner, and paging over any
//! page size sees every published asset exactly once — and the structural guarantee stays where
//! it belongs: in the adapter, as one mutex here and as a row lock held to commit in Postgres.
//! An adapter that mints outside its critical section passes this suite and is still wrong,
//! which is worth knowing rather than papering over.
//!
//! # Reusing an index
//!
//! Every case scopes its owners and assets to itself, so cases may share one index and
//! [`run_all`] does.

use jiff::Timestamp;

use super::{
    AssetIndex, AssetState, BlobOutcome, BlobRecord, ChangeKind, PendingAsset, Reservation,
};
use crate::blob::ContentAddress;
use crate::blob::address::CONTENT_ADDRESS_LEN;
use crate::store::{AlbumId, AssetId, BlobRole, OwnerId};

/// Unwrap an index result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, crate::store::StoreError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("the index could not {doing}: {error}"),
    }
}

/// A deterministic content address from a seed, so a case can name blobs readably.
fn address(seed: &str) -> ContentAddress {
    let mut hex = String::with_capacity(CONTENT_ADDRESS_LEN);
    for byte in seed.bytes().cycle() {
        if hex.len() == CONTENT_ADDRESS_LEN {
            break;
        }
        hex.push(char::from_digit(u32::from(byte % 16), 16).unwrap_or('0'));
    }
    match ContentAddress::parse(&hex) {
        Ok(parsed) => parsed,
        Err(error) => panic!("the suite built a malformed address from {seed:?}: {error}"),
    }
}

/// A pending row for `case`'s asset `n`, under `case`'s own owner and album.
fn pending(case: &str, n: u32) -> PendingAsset {
    PendingAsset {
        asset_id: AssetId::new(format!("{case}-asset-{n}")),
        owner_id: OwnerId::new(format!("{case}-owner")),
        album_id: AlbumId::new(format!("{case}-album")),
        protocol_version: "2026-01-01".to_owned(),
        crypto_suite_id: 1,
        created_at: Timestamp::UNIX_EPOCH,
    }
}

/// A finalized blob of `role`, addressed from `seed`.
fn blob(role: BlobRole, seed: &str) -> BlobRecord {
    BlobRecord {
        role,
        address: address(seed),
        size: 1024,
        finalized_at: Timestamp::UNIX_EPOCH,
    }
}

/// Record `blob` and return the sequence number it minted, failing on any non-`Recorded`
/// outcome — the shape almost every case wants.
async fn record(index: &dyn AssetIndex, asset: &AssetId, record: BlobRecord) -> Option<u64> {
    let role = record.role;
    match ok(index.record_blob(asset, record).await, "record a blob") {
        BlobOutcome::Recorded { minted, .. } => minted,
        other => panic!("recording a {role:?} blob on {asset} answered {other:?}"),
    }
}

/// Publish `case`'s asset `n`: reserve it, then land its index tier.
///
/// Returns the asset id and the sequence number publication minted.
async fn publish(index: &dyn AssetIndex, case: &str, n: u32) -> (AssetId, u64) {
    let row = pending(case, n);
    let asset = row.asset_id.clone();
    ok(index.reserve(row).await, "reserve a row");
    record(
        index,
        &asset,
        blob(BlobRole::Provenance, &format!("{case}p{n}")),
    )
    .await;
    let seq = record(
        index,
        &asset,
        blob(BlobRole::Metadata, &format!("{case}m{n}")),
    )
    .await;
    let Some(seq) = seq else {
        panic!("landing the index tier of {asset} published nothing")
    };
    (asset, seq)
}

// ---------------------------------------------------------------------------------------
// Reservation
// ---------------------------------------------------------------------------------------

/// Every session of a bundle reserves unconditionally; the second one joins.
pub async fn reserving_twice_joins_the_same_row(index: &dyn AssetIndex) {
    let row = pending("join", 1);
    let asset = row.asset_id.clone();

    let Reservation::Created(created) = ok(index.reserve(row.clone()).await, "reserve") else {
        panic!("the first reservation of {asset} did not create a row")
    };
    assert_eq!(created.state, AssetState::Pending);
    assert!(
        created.sync_seq.is_none(),
        "a pending row must not hold a sequence number: nothing can see it"
    );

    let Reservation::Joined(joined) = ok(index.reserve(row).await, "reserve again") else {
        panic!("a sibling session of {asset}'s bundle did not join the existing row")
    };
    assert_eq!(*joined, *created);
}

/// A reservation that disagrees with the existing row is refused, and says nothing about it.
pub async fn a_disagreeing_reservation_is_refused_without_disclosure(index: &dyn AssetIndex) {
    let mine = pending("conflict", 1);
    let asset = mine.asset_id.clone();
    ok(index.reserve(mine).await, "reserve");

    let mut theirs = pending("conflict", 1);
    theirs.owner_id = OwnerId::new("conflict-other-owner");

    let outcome = ok(index.reserve(theirs).await, "reserve under another owner");
    assert_eq!(
        outcome,
        Reservation::Conflict,
        "a guessed asset id must not join another owner's bundle"
    );

    // The disclosure property is structural — `Conflict` has no payload — and this is what
    // states it as a test rather than as a comment.
    let rendered = format!("{outcome:?}");
    assert!(
        !rendered.contains("conflict-album") && !rendered.contains(asset.as_str()),
        "the refusal leaked something about the existing row: {rendered}"
    );
}

// ---------------------------------------------------------------------------------------
// Publication
// ---------------------------------------------------------------------------------------

/// The index tier is a conjunction: neither blob publishes on its own.
pub async fn publication_needs_both_index_tier_blobs(index: &dyn AssetIndex) {
    let row = pending("tier", 1);
    let asset = row.asset_id.clone();
    ok(index.reserve(row).await, "reserve");

    assert_eq!(
        record(index, &asset, blob(BlobRole::Metadata, "tier-m")).await,
        None,
        "a metadata blob alone published an asset whose manifest no client can verify"
    );
    let after_metadata = ok(index.read(&asset).await, "read").expect("the row exists");
    assert_eq!(after_metadata.state, AssetState::Pending);

    assert_eq!(
        record(index, &asset, blob(BlobRole::Derivative, "tier-d")).await,
        None,
        "a derivative must never publish an asset"
    );

    let seq = record(index, &asset, blob(BlobRole::Provenance, "tier-p"))
        .await
        .expect("the manifest completes the index tier");
    let published = ok(index.read(&asset).await, "read").expect("the row exists");
    assert_eq!(published.state, AssetState::Visible);
    assert_eq!(published.sync_seq, Some(seq));
    assert_eq!(
        published.first_seq,
        Some(seq),
        "the first publication is the row's first sequence number"
    );
}

/// The bundle has no wire ordering, so the gate must not depend on which blob arrives last.
pub async fn either_arrival_order_publishes(index: &dyn AssetIndex) {
    let forward = pending("order", 1);
    let a = forward.asset_id.clone();
    ok(index.reserve(forward).await, "reserve");
    assert_eq!(
        record(index, &a, blob(BlobRole::Provenance, "order-p1")).await,
        None
    );
    assert!(
        record(index, &a, blob(BlobRole::Metadata, "order-m1"))
            .await
            .is_some()
    );

    let reverse = pending("order", 2);
    let b = reverse.asset_id.clone();
    ok(index.reserve(reverse).await, "reserve");
    assert_eq!(
        record(index, &b, blob(BlobRole::Metadata, "order-m2")).await,
        None
    );
    assert!(
        record(index, &b, blob(BlobRole::Provenance, "order-p2"))
            .await
            .is_some(),
        "the reverse arrival order left the asset unpublished"
    );
}

/// Re-recording the same blob is a retry, not a change.
pub async fn re_recording_a_blob_mints_nothing(index: &dyn AssetIndex) {
    let (asset, seq) = publish(index, "retry", 1).await;

    let outcome = ok(
        index
            .record_blob(&asset, blob(BlobRole::Metadata, "retrym1"))
            .await,
        "re-record",
    );
    let BlobOutcome::AlreadyHeld(row) = outcome else {
        panic!("a retried finalization was not recognised as one: {outcome:?}")
    };
    assert_eq!(
        row.sync_seq,
        Some(seq),
        "a retry minted a second sequence number and would publish the asset twice"
    );
}

/// A singular role cannot be re-pointed at different bytes.
pub async fn a_singular_role_cannot_be_repointed(index: &dyn AssetIndex) {
    let (asset, _) = publish(index, "repoint", 1).await;

    for role in [BlobRole::Metadata, BlobRole::Provenance] {
        let outcome = ok(
            index
                .record_blob(&asset, blob(role, "repoint-different"))
                .await,
            "re-point a singular role",
        );
        assert_eq!(
            outcome,
            BlobOutcome::Conflict,
            "{role:?} was silently re-pointed, which lets a later session swap the bytes a \
             signature already covers"
        );
    }

    // Derivatives are plural: an asset has a thumbnail *and* a preview.
    assert!(
        record(index, &asset, blob(BlobRole::Derivative, "repoint-d1"))
            .await
            .is_some()
    );
    assert!(
        record(index, &asset, blob(BlobRole::Derivative, "repoint-d2"))
            .await
            .is_some(),
        "a second derivative was refused; derivatives are not singular"
    );
}

/// Recording against a row nobody reserved is a miss, not an implicit create.
pub async fn recording_against_an_unreserved_asset_is_not_found(index: &dyn AssetIndex) {
    let outcome = ok(
        index
            .record_blob(
                &AssetId::new("unreserved-asset"),
                blob(BlobRole::Metadata, "orphan"),
            )
            .await,
        "record against nothing",
    );
    assert_eq!(outcome, BlobOutcome::NotFound);
}

// ---------------------------------------------------------------------------------------
// The feed
// ---------------------------------------------------------------------------------------

/// `original_held` follows the original blob, through the row and through the feed.
pub async fn original_held_follows_the_original_blob(index: &dyn AssetIndex) {
    let owner = OwnerId::new("held-owner");
    let (asset, seq) = publish(index, "held", 1).await;

    let page = ok(index.feed_page(&owner, seq - 1, 10).await, "page the feed");
    let entry = page.first().expect("the published asset is in the feed");
    assert_eq!(entry.asset_id, asset);
    assert!(
        !entry.original_held,
        "an asset published from its index tier alone is awaiting-original"
    );

    let flipped = record(index, &asset, blob(BlobRole::Original, "held-o"))
        .await
        .expect("the original landing is a publishable change");
    assert!(flipped > seq);

    let page = ok(index.feed_page(&owner, seq, 10).await, "page the feed");
    let entry = page.first().expect("the flip reached the feed");
    assert!(
        entry.original_held,
        "the original landed and the feed still says awaiting-original"
    );
    assert_eq!(
        entry.change,
        ChangeKind::Updated,
        "a reader that had already seen the asset saw it created a second time"
    );
}

/// The change kind is a fact about the reader, not about the asset.
pub async fn the_change_kind_is_relative_to_the_reader(index: &dyn AssetIndex) {
    let owner = OwnerId::new("kind-owner");
    let (_, first) = publish(index, "kind", 1).await;
    let asset = AssetId::new("kind-asset-1");
    let second = record(index, &asset, blob(BlobRole::Original, "kind-o"))
        .await
        .expect("the original landing publishes an update");

    // A reader who has never looked: the asset is new to them, whatever has happened since.
    let fresh = ok(index.feed_page(&owner, 0, 10).await, "page from the start");
    assert_eq!(fresh.len(), 1, "a latest-state feed shows each asset once");
    assert_eq!(fresh[0].change, ChangeKind::Created);
    assert_eq!(fresh[0].sync_seq, second);

    // A reader who saw the creation: the same row is an update.
    let resuming = ok(index.feed_page(&owner, first, 10).await, "resume");
    assert_eq!(resuming.len(), 1);
    assert_eq!(resuming[0].change, ChangeKind::Updated);

    // And a reader who is caught up sees nothing at all.
    let caught_up = ok(index.feed_page(&owner, second, 10).await, "resume at head");
    assert!(caught_up.is_empty());
}

/// Paging is ordered, bounded and resumable, at any page size.
pub async fn paging_is_ordered_bounded_and_resumable(index: &dyn AssetIndex) {
    let owner = OwnerId::new("page-owner");
    let mut published = Vec::new();
    for n in 0..5 {
        published.push(publish(index, "page", n).await);
    }

    for page_size in [1_usize, 2, 5, 50] {
        let mut cursor = 0_u64;
        let mut seen = Vec::new();
        loop {
            let page = ok(
                index.feed_page(&owner, cursor, page_size).await,
                "page the feed",
            );
            if page.is_empty() {
                break;
            }
            assert!(
                page.len() <= page_size,
                "a page of {} exceeded the requested {page_size}",
                page.len()
            );
            for entry in &page {
                assert!(
                    entry.sync_seq > cursor,
                    "an entry at {} came back for a cursor already past it",
                    entry.sync_seq
                );
                cursor = entry.sync_seq;
                seen.push(entry.asset_id.clone());
            }
        }
        let expected: Vec<_> = published.iter().map(|(asset, _)| asset.clone()).collect();
        assert_eq!(
            seen, expected,
            "paging at {page_size} did not see every asset exactly once, in sequence order"
        );
    }
}

/// Every number the index mints is reachable through the feed.
///
/// The observable half of `S-C21` — see the module docs on what this can and cannot prove.
pub async fn every_minted_number_is_reachable(index: &dyn AssetIndex) {
    let owner = OwnerId::new("reach-owner");
    let mut minted = Vec::new();
    for n in 0..4 {
        let (asset, seq) = publish(index, "reach", n).await;
        minted.push(seq);
        // Interleave a second publishable change, so the owner's numbers are not simply the
        // order the assets were created in.
        if n % 2 == 0 {
            let flipped = record(
                index,
                &asset,
                blob(BlobRole::Original, &format!("reacho{n}")),
            )
            .await
            .expect("the original landing publishes an update");
            minted.push(flipped);
        }
    }

    let mut previous = 0;
    for seq in &minted {
        assert!(
            *seq > previous,
            "sequence numbers must be strictly increasing per owner; {seq} followed {previous}"
        );
        previous = *seq;
    }

    let head = ok(index.head_seq(&owner).await, "read the head");
    assert_eq!(
        head, previous,
        "the head must be the highest number the owner has minted"
    );

    // Each row's *latest* number is in the feed. Superseded numbers are not, by design: this
    // is a latest-state feed, so a reader is never handed an entry it would immediately
    // overwrite.
    let live: Vec<u64> = ok(index.feed_page(&owner, 0, 100).await, "page the feed")
        .iter()
        .map(|entry| entry.sync_seq)
        .collect();
    for seq in live {
        assert!(
            minted.contains(&seq),
            "the feed served {seq}, which was never minted"
        );
    }
}

/// An album sees a monotonic subsequence of its owner's numbers.
pub async fn an_albums_numbers_are_monotonic_with_gaps(index: &dyn AssetIndex) {
    let owner = OwnerId::new("album-owner");
    // Two albums under one owner, publications interleaved.
    for n in 0..4 {
        let mut row = pending("album", n);
        row.owner_id = owner.clone();
        row.album_id = AlbumId::new(if n % 2 == 0 { "album-a" } else { "album-b" });
        let asset = row.asset_id.clone();
        ok(index.reserve(row).await, "reserve");
        record(
            index,
            &asset,
            blob(BlobRole::Provenance, &format!("albp{n}")),
        )
        .await;
        record(index, &asset, blob(BlobRole::Metadata, &format!("albm{n}"))).await;
    }

    let page = ok(index.feed_page(&owner, 0, 100).await, "page the feed");
    for album in ["album-a", "album-b"] {
        let seqs: Vec<u64> = page
            .iter()
            .filter(|entry| entry.album_id.as_str() == album)
            .map(|entry| entry.sync_seq)
            .collect();
        assert!(seqs.len() >= 2, "{album} should have published twice");
        assert!(
            seqs.windows(2).all(|pair| pair[0] < pair[1]),
            "{album}'s numbers are not strictly increasing: {seqs:?}"
        );
    }
    // Gaps are the point: the design contracts monotonicity, never contiguity, and a client
    // that required contiguity would break the moment a sibling album wrote.
    let a_seqs: Vec<u64> = page
        .iter()
        .filter(|entry| entry.album_id.as_str() == "album-a")
        .map(|entry| entry.sync_seq)
        .collect();
    assert!(
        a_seqs.windows(2).any(|pair| pair[1] - pair[0] > 1),
        "the interleaved albums produced no gap, so this case is no longer testing anything"
    );
}

// ---------------------------------------------------------------------------------------
// Tombstones
// ---------------------------------------------------------------------------------------

/// A deletion reaches a reader that never saw the asset.
pub async fn a_tombstone_reaches_every_reader(index: &dyn AssetIndex) {
    let owner = OwnerId::new("tomb-owner");
    let (asset, published) = publish(index, "tomb", 1).await;

    let row = ok(
        index.tombstone(&asset, Timestamp::UNIX_EPOCH).await,
        "tombstone",
    )
    .expect("the row exists");
    assert_eq!(row.state, AssetState::Tombstoned);
    let deleted = row.sync_seq.expect("a retraction is a publishable change");
    assert!(deleted > published);

    let fresh = ok(index.feed_page(&owner, 0, 10).await, "page from the start");
    assert_eq!(fresh.len(), 1);
    assert_eq!(
        fresh[0].change,
        ChangeKind::Deleted,
        "a reader that never saw the asset must still be told it is gone"
    );

    // Tombstoning twice is idempotent: the asset's final word does not get a second number.
    let again = ok(
        index.tombstone(&asset, Timestamp::UNIX_EPOCH).await,
        "tombstone again",
    )
    .expect("the row exists");
    assert_eq!(again.sync_seq, Some(deleted));
}

/// A row nobody could see needs no retraction.
pub async fn tombstoning_a_pending_row_publishes_nothing(index: &dyn AssetIndex) {
    let owner = OwnerId::new("pendtomb-owner");
    let row = pending("pendtomb", 1);
    let asset = row.asset_id.clone();
    ok(index.reserve(row).await, "reserve");

    let tombstoned = ok(
        index.tombstone(&asset, Timestamp::UNIX_EPOCH).await,
        "tombstone",
    )
    .expect("the row exists");
    assert_eq!(tombstoned.state, AssetState::Tombstoned);
    assert_eq!(
        tombstoned.sync_seq, None,
        "an abandoned half-bundle consumed a sequence number nobody can use"
    );
    assert!(
        ok(index.feed_page(&owner, 0, 10).await, "page the feed").is_empty(),
        "a row no device ever saw was retracted to every device"
    );

    // The id is spent: a caller cannot reserve a tombstoned asset back into life.
    assert_eq!(
        ok(index.reserve(pending("pendtomb", 1)).await, "re-reserve"),
        Reservation::Joined(Box::new(tombstoned)),
        "re-reserving must return the tombstone rather than a fresh pending row"
    );
}

/// A blob that lands after the tombstone is held but publishes nothing.
pub async fn a_late_blob_does_not_revive_a_tombstone(index: &dyn AssetIndex) {
    let (asset, _) = publish(index, "late", 1).await;
    let deleted = ok(
        index.tombstone(&asset, Timestamp::UNIX_EPOCH).await,
        "tombstone",
    )
    .expect("the row exists")
    .sync_seq
    .expect("a retraction is publishable");

    assert_eq!(
        record(index, &asset, blob(BlobRole::Original, "late-o")).await,
        None,
        "a late original republished a deleted asset"
    );
    let row = ok(index.read(&asset).await, "read").expect("the row exists");
    assert_eq!(row.state, AssetState::Tombstoned);
    assert_eq!(row.sync_seq, Some(deleted));
    assert!(
        row.address_for(BlobRole::Original).is_some(),
        "the reference must still be held; GC decides what to do with the bytes, not this port"
    );
}

// ---------------------------------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------------------------------

/// The duplicate lookup is scoped to owner *and* album, and both scopes carry weight.
pub async fn the_duplicate_lookup_is_scoped_to_owner_and_album(index: &dyn AssetIndex) {
    let (asset, _) = publish(index, "find", 1).await;
    let mine = OwnerId::new("find-owner");
    let theirs = OwnerId::new("find-other-owner");
    let album = AlbumId::new("find-album");
    let elsewhere = AlbumId::new("find-second-album");
    let held = address("findm1");

    assert_eq!(
        ok(
            index.find_by_address(&mine, &album, &held).await,
            "look up my blob"
        ),
        Some(asset.clone()),
    );
    assert_eq!(
        ok(
            index.find_by_address(&theirs, &album, &held).await,
            "look up another owner's blob"
        ),
        None,
        "answering across owners tells one account what another holds",
    );
    assert_eq!(
        ok(
            index.find_by_address(&mine, &elsewhere, &held).await,
            "look up my blob from another album"
        ),
        None,
        "the same bytes in a second album are not a duplicate: there is nothing to merge, \
         so the upload proceeds and the blob store deduplicates the ciphertext instead",
    );
    assert_eq!(
        ok(
            index
                .find_by_address(&mine, &album, &address("never-stored"))
                .await,
            "look up an unheld blob"
        ),
        None,
    );

    // The second album's copy is its own asset, and *it* is what a repeat upload there finds.
    let second = AssetId::new("find-second-asset");
    ok(
        index
            .reserve(PendingAsset {
                asset_id: second.clone(),
                owner_id: mine.clone(),
                album_id: elsewhere.clone(),
                protocol_version: "2026-01-01".to_owned(),
                crypto_suite_id: 1,
                created_at: Timestamp::UNIX_EPOCH,
            })
            .await,
        "reserve the second album's row",
    );
    record(
        index,
        &second,
        BlobRecord {
            role: BlobRole::Metadata,
            address: held.clone(),
            size: 1024,
            finalized_at: Timestamp::UNIX_EPOCH,
        },
    )
    .await;
    assert_eq!(
        ok(
            index.find_by_address(&mine, &elsewhere, &held).await,
            "look up the second album's copy"
        ),
        Some(second),
    );
    assert_eq!(
        ok(
            index.find_by_address(&mine, &album, &held).await,
            "look up the first album's copy"
        ),
        Some(asset),
        "the second album's row must not shadow the first album's answer",
    );
}

/// The serving lookup answers about liveness, and pending rows are not live.
pub async fn the_serving_lookup_ignores_unpublished_rows(index: &dyn AssetIndex) {
    let row = pending("serve-pending", 1);
    let asset = row.asset_id.clone();
    ok(index.reserve(row).await, "reserve a row");
    let held = address("servep1");
    record(index, &asset, blob(BlobRole::Provenance, "servep1")).await;

    assert_eq!(
        ok(index.find_reference(&held).await, "look up a pending blob"),
        None,
        "an asset in nobody's feed must not be fetchable by content address",
    );

    // Publishing it makes the same address a live reference.
    record(index, &asset, blob(BlobRole::Metadata, "servepm1")).await;
    let reference = ok(
        index.find_reference(&held).await,
        "look up a published blob",
    )
    .expect("a published asset's blob is a live reference");
    assert_eq!(reference.asset_id, asset);
    assert_eq!(reference.role, BlobRole::Provenance);
    assert_eq!(reference.state, AssetState::Visible);
}

/// A tombstone makes its blobs gone, and `original_held` rides along for the pending case.
pub async fn the_serving_lookup_reports_state_and_original_holding(index: &dyn AssetIndex) {
    let (asset, _) = publish(index, "serve-state", 1).await;
    let held = address("serve-statem1");

    let reference = ok(index.find_reference(&held).await, "look up a live blob")
        .expect("a published asset's blob is a live reference");
    assert_eq!(reference.state, AssetState::Visible);
    assert!(
        !reference.original_held,
        "no original has landed, and the reference must say so rather than imply it"
    );

    record(index, &asset, blob(BlobRole::Original, "serve-stateo1")).await;
    let reference = ok(
        index.find_reference(&address("serve-stateo1")).await,
        "look up an original",
    )
    .expect("the original is a live reference");
    assert!(reference.original_held);
    assert_eq!(reference.role, BlobRole::Original);

    ok(
        index.tombstone(&asset, Timestamp::UNIX_EPOCH).await,
        "tombstone",
    );
    let reference = ok(
        index.find_reference(&held).await,
        "look up a tombstoned blob",
    )
    .expect("the reference survives the tombstone; GC owns the bytes, not this port");
    assert_eq!(
        reference.state,
        AssetState::Tombstoned,
        "a deleted asset's blob is gone, not unknown",
    );
}

/// Deleting one asset must not take a shared blob's other holder with it.
pub async fn a_live_holder_outranks_a_deleted_one(index: &dyn AssetIndex) {
    let shared = address("serve-shared");
    let thumbnail = || BlobRecord {
        role: BlobRole::Derivative,
        address: shared.clone(),
        size: 1024,
        finalized_at: Timestamp::UNIX_EPOCH,
    };

    let (first, _) = publish(index, "serve-shared", 1).await;
    let (second, _) = publish(index, "serve-shared", 2).await;
    record(index, &first, thumbnail()).await;
    record(index, &second, thumbnail()).await;

    ok(
        index.tombstone(&first, Timestamp::UNIX_EPOCH).await,
        "tombstone the first holder",
    );

    let reference = ok(index.find_reference(&shared).await, "look up a shared blob")
        .expect("a shared blob with a live holder is a live reference");
    assert_eq!(
        reference.asset_id, second,
        "deleting one asset made a blob another asset still holds unservable",
    );
    assert_eq!(reference.state, AssetState::Visible);
}

/// Run every case against `index`, in order.
pub async fn run_all(index: &dyn AssetIndex) {
    reserving_twice_joins_the_same_row(index).await;
    a_disagreeing_reservation_is_refused_without_disclosure(index).await;
    publication_needs_both_index_tier_blobs(index).await;
    either_arrival_order_publishes(index).await;
    re_recording_a_blob_mints_nothing(index).await;
    a_singular_role_cannot_be_repointed(index).await;
    recording_against_an_unreserved_asset_is_not_found(index).await;
    original_held_follows_the_original_blob(index).await;
    the_change_kind_is_relative_to_the_reader(index).await;
    paging_is_ordered_bounded_and_resumable(index).await;
    every_minted_number_is_reachable(index).await;
    an_albums_numbers_are_monotonic_with_gaps(index).await;
    a_tombstone_reaches_every_reader(index).await;
    tombstoning_a_pending_row_publishes_nothing(index).await;
    a_late_blob_does_not_revive_a_tombstone(index).await;
    the_duplicate_lookup_is_scoped_to_owner_and_album(index).await;
    the_serving_lookup_ignores_unpublished_rows(index).await;
    the_serving_lookup_reports_state_and_original_holding(index).await;
    a_live_holder_outranks_a_deleted_one(index).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::memory::InMemoryAssetIndex;

    /// The deterministic double is only a legitimate stand-in for Postgres to the extent it
    /// passes this.
    #[tokio::test]
    async fn the_in_memory_index_conforms() {
        run_all(&InMemoryAssetIndex::new()).await;
    }
}
