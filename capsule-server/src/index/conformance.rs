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

use capsule_core::crypto::hash::Hash32;
use jiff::Timestamp;

use super::{
    AssetIndex, AssetState, BlobOutcome, BlobRecord, ChangeKind, HoldOutcome, LifecycleOp,
    OpAction, OpOutcome, PendingAsset, Reservation, ServingHold,
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
///
/// A provenance blob carries a `manifest_sha256`, because that is what a real finalization
/// computes over the bytes it committed and it is what the chain head is set from (`S-C31`).
/// Taken from the address here only because the double's addresses *are* SHA-256 digests; the
/// port is what refuses to make that assumption.
fn blob(role: BlobRole, seed: &str) -> BlobRecord {
    let address = address(seed);
    BlobRecord {
        manifest_sha256: (role == BlobRole::Provenance)
            .then(|| Hash32::from_hex(address.as_str()).expect("an address is a digest")),
        role,
        address,
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
            manifest_sha256: None,
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
        manifest_sha256: None,
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

// ---------------------------------------------------------------------------------------
// Lifecycle writes
// ---------------------------------------------------------------------------------------

/// A manifest hash from a seed, so a case can name one readably.
fn manifest(seed: u8) -> Hash32 {
    Hash32([seed; 32])
}

/// The chain head `asset` currently carries.
async fn head_of(index: &dyn AssetIndex, asset: &AssetId) -> Option<Hash32> {
    ok(index.read(asset).await, "read a row").and_then(|row| row.chain_head)
}

/// A lifecycle op for `case`'s asset `n`, chaining onto `prior` and producing `hash`.
fn op(case: &str, n: u32, action: OpAction, prior: Option<Hash32>, hash: Hash32) -> LifecycleOp {
    LifecycleOp {
        asset_id: AssetId::new(format!("{case}-asset-{n}")),
        owner_id: OwnerId::new(format!("{case}-owner")),
        album_id: AlbumId::new(format!("{case}-album")),
        action,
        manifest_hash: hash,
        prior_provenance_hash: prior,
        amk_version: 1,
        provenance: address(&format!("{case}prov{n}")),
        original: None,
        metadata: None,
        retention_until: None,
        at: Timestamp::UNIX_EPOCH,
    }
}

/// Publication establishes a chain head, and each op must name the one before it.
///
/// The head a `create` establishes is its **provenance blob's** content address: the manifest
/// is uploaded as that blob and never re-declared, so its address is the only handle the server
/// has on it. Every case here therefore starts from `publish`'s provenance seed rather than
/// from `None` — which is also what a real first lifecycle op does.
pub async fn a_lifecycle_write_extends_the_chain(index: &dyn AssetIndex) {
    let (asset, published) = publish(index, "op-chain", 1).await;
    let created = head_of(index, &asset).await;
    assert!(
        created.is_some(),
        "a published asset must carry the chain its first lifecycle op has to name"
    );
    let first = manifest(1);

    let OpOutcome::Applied { row, sync_seq } = ok(
        index
            .apply_op(op("op-chain", 1, OpAction::MetadataUpdate, created, first))
            .await,
        "apply the first op",
    ) else {
        panic!("a well-formed first op was not applied")
    };
    assert!(
        sync_seq > published,
        "a change must sit above the publication it changes, or a caught-up reader misses it"
    );
    assert_eq!(row.chain_head, Some(first));

    // A second op must name the first as its predecessor — naming the *create* again is the
    // shape a replayed old manifest takes.
    let second = manifest(2);
    let stale = ok(
        index
            .apply_op(op("op-chain", 1, OpAction::MetadataUpdate, created, second))
            .await,
        "apply an unchained op",
    );
    assert_eq!(
        stale,
        OpOutcome::StaleChain { head: Some(first) },
        "an op that does not name the stored head is invariant 17's stale revival",
    );

    let OpOutcome::Applied { row, .. } = ok(
        index
            .apply_op(op(
                "op-chain",
                1,
                OpAction::MetadataUpdate,
                Some(first),
                second,
            ))
            .await,
        "apply the chained op",
    ) else {
        panic!("an op chaining onto the head was not applied")
    };
    assert_eq!(row.chain_head, Some(second));
    let _ = asset;
}

/// The same manifest twice is one application and one sequence number.
pub async fn re_applying_a_manifest_is_a_replay(index: &dyn AssetIndex) {
    let (asset, _) = publish(index, "op-replay", 1).await;
    let created = head_of(index, &asset).await;
    let hash = manifest(3);
    let OpOutcome::Applied { sync_seq, .. } = ok(
        index
            .apply_op(op("op-replay", 1, OpAction::Delete, created, hash))
            .await,
        "apply",
    ) else {
        panic!("a well-formed op was not applied")
    };

    // The replay names the head it chained onto, which is *no longer* the head — which is
    // exactly why the idempotency check must run before invariant 17.
    assert_eq!(
        ok(
            index
                .apply_op(op("op-replay", 1, OpAction::Delete, created, hash))
                .await,
            "replay",
        ),
        OpOutcome::Replayed { sync_seq },
        "a lost acknowledgement must not cost a second sequence number, and must not be \
         answered as a stale chain — the client's only fault was not hearing the first answer",
    );
    assert_eq!(
        ok(
            index.head_seq(&OwnerId::new("op-replay-owner")).await,
            "head"
        ),
        sync_seq,
        "a replay minted a number"
    );
}

/// Two identical submissions racing produce one application and one replay.
///
/// The window this closes is not hypothetical: an adapter that checks its idempotency store,
/// then takes the asset's lock, has read *before* the lock and decided *after* it. Two clients
/// retrying the same manifest — or one client whose first attempt is still in flight when its
/// retry timer fires — both find nothing, and the loser then serializes behind a winner that has
/// meanwhile applied the very manifest it looked for.
///
/// Answering that loser from what it read before the lock is wrong twice over. It would fail
/// invariant 17 against a chain head the winner has just advanced, and report `StaleChain` to a
/// client whose manifest *was* applied — moments ago, by the winner — which is precisely the
/// answer `re_applying_a_manifest_is_a_replay` exists to forbid in the sequential case.
///
/// A single-process suite cannot force a particular interleaving, so this asserts the property
/// that holds under *every* interleaving: one `Applied`, one `Replayed`, the same sequence
/// number in both, and exactly one number minted.
pub async fn racing_identical_submissions_apply_once_and_replay_once(index: &dyn AssetIndex) {
    let (asset, _) = publish(index, "op-race", 1).await;
    let created = head_of(index, &asset).await;
    // A seed of its own. `applied_manifests` is keyed on the hash **globally**, not per asset —
    // which is the point of it — so a case that reused another case's manifest would be told
    // `Replayed` for a submission it had never made, and would take that for its own answer.
    let hash = manifest(41);
    let owner = OwnerId::new("op-race-owner");
    let before = ok(index.head_seq(&owner).await, "head");

    let (first, second) = tokio::join!(
        index.apply_op(op("op-race", 1, OpAction::Delete, created, hash)),
        index.apply_op(op("op-race", 1, OpAction::Delete, created, hash)),
    );
    let outcomes = [ok(first, "apply"), ok(second, "apply")];

    let applied: Vec<u64> = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            OpOutcome::Applied { sync_seq, .. } => Some(*sync_seq),
            _ => None,
        })
        .collect();
    let replayed: Vec<u64> = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            OpOutcome::Replayed { sync_seq } => Some(*sync_seq),
            _ => None,
        })
        .collect();
    assert_eq!(
        applied.len(),
        1,
        "exactly one of two identical submissions applies, got {outcomes:?}"
    );
    assert_eq!(
        replayed.len(),
        1,
        "the other is a replay and never a stale chain, got {outcomes:?}"
    );
    assert_eq!(
        applied[0], replayed[0],
        "a replay reports the number the application minted"
    );

    assert_eq!(
        ok(index.head_seq(&owner).await, "head"),
        before.saturating_add(1),
        "two identical submissions cost one sequence number, not two"
    );
}

/// A delete tombstones, a restore un-tombstones, and both reach the feed.
pub async fn delete_and_restore_are_both_publishable_changes(index: &dyn AssetIndex) {
    let (asset, _) = publish(index, "op-cycle", 1).await;
    let created = head_of(index, &asset).await;
    let owner = OwnerId::new("op-cycle-owner");
    let deleted = manifest(4);
    let restored = manifest(5);

    let OpOutcome::Applied {
        row,
        sync_seq: at_delete,
    } = ok(
        index
            .apply_op(op("op-cycle", 1, OpAction::Delete, created, deleted))
            .await,
        "delete",
    )
    else {
        panic!("a delete was not applied")
    };
    assert_eq!(row.state, AssetState::Tombstoned);

    let OpOutcome::Applied {
        row,
        sync_seq: at_restore,
    } = ok(
        index
            .apply_op(op(
                "op-cycle",
                1,
                OpAction::TrashRestore,
                Some(deleted),
                restored,
            ))
            .await,
        "restore",
    )
    else {
        panic!("a restore was not applied")
    };
    assert_eq!(row.state, AssetState::Visible);
    assert!(at_restore > at_delete);

    // A reader who saw the asset before the delete sees the restore as an update, not as a
    // resurrection it has to guess at.
    let page = ok(index.feed_page(&owner, at_delete, 10).await, "page");
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].sync_seq, at_restore);
    assert_eq!(page[0].change, ChangeKind::Updated);
}

/// An epoch below the album's high-water mark is refused, whichever asset it arrives on.
pub async fn an_epoch_that_regresses_the_album_is_refused(index: &dyn AssetIndex) {
    let (first_asset, _) = publish(index, "op-epoch", 1).await;
    let (second_asset, _) = publish(index, "op-epoch", 2).await;

    let mut forward = op(
        "op-epoch",
        1,
        OpAction::MetadataUpdate,
        head_of(index, &first_asset).await,
        manifest(6),
    );
    forward.amk_version = 9;
    let OpOutcome::Applied { .. } = ok(index.apply_op(forward).await, "advance the epoch") else {
        panic!("an advancing epoch was refused")
    };

    // A *different* asset in the same album may not re-admit the epoch the album left behind.
    let mut backward = op(
        "op-epoch",
        2,
        OpAction::MetadataUpdate,
        head_of(index, &second_asset).await,
        manifest(7),
    );
    backward.amk_version = 8;
    assert_eq!(
        ok(index.apply_op(backward).await, "regress the epoch"),
        OpOutcome::AmkRegressed { stored: 9 },
        "invariant 18 is an album rule, so a stale asset must not be a way back to a retired \
         epoch",
    );
}

/// An op against somebody else's asset, a missing one, or an unpublished one is one answer.
pub async fn an_op_on_an_asset_that_is_not_the_callers_is_not_found(index: &dyn AssetIndex) {
    let (asset, _) = publish(index, "op-scope", 1).await;
    let created = head_of(index, &asset).await;

    let mut theirs = op("op-scope", 1, OpAction::Delete, created, manifest(8));
    theirs.owner_id = OwnerId::new("op-scope-other-owner");
    assert_eq!(
        ok(index.apply_op(theirs).await, "apply as another owner"),
        OpOutcome::NotFound,
    );

    let mut elsewhere = op("op-scope", 1, OpAction::Delete, created, manifest(9));
    elsewhere.album_id = AlbumId::new("op-scope-other-album");
    assert_eq!(
        ok(
            index.apply_op(elsewhere).await,
            "apply against another album"
        ),
        OpOutcome::NotFound,
        "the album is part of the address, not decoration",
    );

    let mut absent = op("op-scope", 99, OpAction::Delete, created, manifest(10));
    absent.asset_id = AssetId::new("op-scope-no-such-asset");
    assert_eq!(
        ok(index.apply_op(absent).await, "apply against nothing"),
        OpOutcome::NotFound,
    );

    // A reserved-but-unpublished row is also not found: an op against a half-finished upload is
    // a client bug about which asset, not about which manifest.
    let pending = pending("op-scope-pending", 1);
    let pending_id = pending.asset_id.clone();
    ok(index.reserve(pending).await, "reserve");
    let mut half = op("op-scope-pending", 1, OpAction::Delete, None, manifest(11));
    half.asset_id = pending_id;
    assert_eq!(
        ok(index.apply_op(half).await, "apply against a pending row"),
        OpOutcome::NotFound,
    );
    let _ = asset;
}

/// A lifecycle write **retains** the manifest it supersedes (`S-C52`).
///
/// The provenance role moves; the manifest it moved off does not become an orphan. Two
/// documented capabilities rest on that — the scrub's chain walk, and the takedown rebuttal,
/// which answers an accusation with the user's own signed `delete` manifest — and both were
/// quietly lost while a re-point simply dropped the address.
pub async fn a_lifecycle_write_retains_the_manifest_it_supersedes(index: &dyn AssetIndex) {
    let (asset, _) = publish(index, "op-keep", 1).await;
    let first = ok(index.read(&asset).await, "read")
        .expect("the row exists")
        .address_for(BlobRole::Provenance)
        .cloned()
        .expect("publication landed a manifest");

    let mut update = op(
        "op-keep",
        1,
        OpAction::MetadataUpdate,
        head_of(index, &asset).await,
        manifest(31),
    );
    // Each link is its own object. `op` reuses one provenance address, and content addressing
    // means two identical manifests *are* one blob — so a case about a chain has to vary it, or
    // it is asserting about a single link twice.
    update.provenance = address("op-keep-prov-2");
    ok(index.apply_op(update).await, "apply");

    let row = ok(index.read(&asset).await, "read").expect("the row exists");
    assert_ne!(
        row.address_for(BlobRole::Provenance),
        Some(&first),
        "the current manifest moved"
    );
    assert_eq!(
        row.superseded,
        vec![first.clone()],
        "and the one it moved off is retained, not dropped"
    );
    assert_eq!(
        ok(index.reference_count(&first).await, "count"),
        1,
        "a retained manifest is a reference, which is what keeps the collector off it"
    );

    // A second write extends the record rather than replacing it: the chain is the whole point.
    let mut again = op(
        "op-keep",
        1,
        OpAction::MetadataUpdate,
        head_of(index, &asset).await,
        manifest(32),
    );
    again.provenance = address("op-keep-prov-3");
    ok(index.apply_op(again).await, "apply");
    assert_eq!(
        ok(index.read(&asset).await, "read")
            .expect("the row exists")
            .superseded
            .len(),
        2,
        "oldest first, and every link kept"
    );

    // And the retention ends where the user's own signed delete said it would.
    ok(
        index.tombstone(&asset, Timestamp::UNIX_EPOCH).await,
        "tombstone",
    );
    ok(index.purge(&asset).await, "purge");
    assert_eq!(
        ok(index.reference_count(&first).await, "count after a purge"),
        0,
        "a purge is the close of the retention window, so the chain goes with the bytes"
    );
}

/// A `replace` re-points the **original**, which is the role `record_blob` refuses to move
/// (`S-C43`).
///
/// The refusal is not softened by this: an *upload* that swapped the original would swap bytes
/// under a signature that still verifies against the old ones, and `BlobOutcome::Conflict` is
/// what stops it. A replace arrives with a manifest that chains onto the one it supersedes, and
/// that chain check is the difference between an authorized replacement and the defect — which
/// is why it is an `apply_op` and not a relaxed `record_blob`.
pub async fn a_replace_repoints_the_original_the_upload_path_may_not_move(index: &dyn AssetIndex) {
    let (asset, _) = publish(index, "op-repl", 1).await;
    // `publish` lands the index tier — the manifest and the metadata blob — because that is what
    // makes an asset visible. The original arrives afterwards, exactly as it does under a staged
    // upload, and it is the role this case is about.
    record(index, &asset, blob(BlobRole::Original, "op-repl-o1")).await;
    let before = ok(index.read(&asset).await, "read")
        .expect("the row exists")
        .address_for(BlobRole::Original)
        .cloned()
        .expect("the original landed");

    // The upload path cannot do this, and that is the property being contrasted.
    assert_eq!(
        ok(
            index
                .record_blob(&asset, blob(BlobRole::Original, "op-repl-newbytes"))
                .await,
            "record a second original"
        ),
        BlobOutcome::Conflict,
        "an upload may never re-point a singular role; only an authorized write may"
    );

    let mut replace = op(
        "op-repl",
        1,
        OpAction::Replace,
        head_of(index, &asset).await,
        manifest(77),
    );
    replace.original = Some(address("op-repl-newbytes"));
    replace.metadata = Some(address("op-repl-newmeta"));
    let sync_seq = match ok(index.apply_op(replace).await, "apply a replace") {
        OpOutcome::Applied { sync_seq, .. } => sync_seq,
        other => panic!("a replace answered {other:?}"),
    };

    let row = ok(index.read(&asset).await, "read").expect("the row exists");
    assert_eq!(
        row.address_for(BlobRole::Original),
        Some(&address("op-repl-newbytes")),
        "the asset holds the new bytes"
    );
    assert_ne!(row.address_for(BlobRole::Original), Some(&before));
    assert_eq!(
        row.address_for(BlobRole::Metadata),
        Some(&address("op-repl-newmeta"))
    );
    assert_eq!(row.chain_head, Some(manifest(77)));
    assert_eq!(
        row.state,
        AssetState::Visible,
        "a replace changes an asset's bytes and not its lifecycle state"
    );
    assert_eq!(row.sync_seq, Some(sync_seq));

    // And it is one write: a replace that does not chain onto the new head is refused, exactly
    // as any other lifecycle write is.
    let mut stale = op(
        "op-repl",
        1,
        OpAction::Replace,
        Some(manifest(1)),
        manifest(78),
    );
    stale.original = Some(address("op-repl-newerbytes"));
    assert_eq!(
        ok(index.apply_op(stale).await, "apply a stale replace"),
        OpOutcome::StaleChain {
            head: Some(manifest(77))
        }
    );
}

/// A lifecycle write re-points the provenance blob, which is the one authorized way a singular
/// role moves.
pub async fn a_lifecycle_write_repoints_the_provenance_blob(index: &dyn AssetIndex) {
    let (asset, _) = publish(index, "op-prov", 1).await;
    let before = ok(index.read(&asset).await, "read")
        .expect("the row exists")
        .address_for(BlobRole::Provenance)
        .cloned()
        .expect("publication landed a provenance blob");

    let mut update = op(
        "op-prov",
        1,
        OpAction::MetadataUpdate,
        head_of(index, &asset).await,
        manifest(12),
    );
    update.metadata = Some(address("op-prov-newmeta"));
    ok(index.apply_op(update).await, "apply");

    let row = ok(index.read(&asset).await, "read").expect("the row exists");
    let after = row
        .address_for(BlobRole::Provenance)
        .expect("the op landed a provenance blob");
    assert_ne!(
        after, &before,
        "the feed must serve the newest manifest, so the provenance reference moves with the \
         chain rather than being frozen at publication"
    );
    assert_eq!(
        row.address_for(BlobRole::Metadata),
        Some(&address("op-prov-newmeta")),
        "a metadata update that did not re-point the metadata blob updated nothing"
    );
    assert_eq!(
        ok(
            index.find_reference(&before).await,
            "look up the superseded manifest"
        ),
        None,
        "the superseded manifest is no longer referenced, which is what makes it collectable"
    );
}

/// Reference counting is a query over the rows, and a tombstone still references.
pub async fn references_are_counted_from_the_rows_that_name_them(index: &dyn AssetIndex) {
    let shared = address("refcount-shared");
    let (first, _) = publish(index, "refcount", 1).await;
    let (second, _) = publish(index, "refcount", 2).await;
    assert_eq!(
        ok(
            index.reference_count(&shared).await,
            "count an unheld address"
        ),
        0,
    );

    for asset in [&first, &second] {
        record(
            index,
            asset,
            BlobRecord {
                role: BlobRole::Derivative,
                address: shared.clone(),
                size: 16,
                manifest_sha256: None,
                finalized_at: Timestamp::UNIX_EPOCH,
            },
        )
        .await;
    }
    assert_eq!(
        ok(
            index.reference_count(&shared).await,
            "count a shared address"
        ),
        2,
        "content addressing means one blob serves many assets, and the count is what stops the \
         collector treating the second holder as an orphan",
    );

    // A tombstone still references: deleting is not purging.
    let head = ok(index.read(&first).await, "read")
        .expect("the row exists")
        .chain_head;
    let deleted = ok(
        index
            .apply_op(op("refcount", 1, OpAction::Delete, head, manifest(20)))
            .await,
        "delete",
    );
    assert!(matches!(deleted, OpOutcome::Applied { .. }));
    assert_eq!(
        ok(index.reference_count(&shared).await, "count after a delete"),
        2,
        "trash still occupies storage, which is what makes it recoverable",
    );
}

/// Purging drops the references and keeps the tombstone.
pub async fn purging_drops_the_references_and_keeps_the_tombstone(index: &dyn AssetIndex) {
    let (asset, _) = publish(index, "purge", 1).await;
    let head = ok(index.read(&asset).await, "read")
        .expect("the row exists")
        .chain_head;
    let mut delete = op("purge", 1, OpAction::Delete, head, manifest(21));
    delete.retention_until = Some(Timestamp::UNIX_EPOCH);
    ok(index.apply_op(delete).await, "delete");

    let listed = ok(index.tombstoned(10).await, "list tombstones");
    assert!(
        listed.iter().any(|row| row.asset_id == asset),
        "a tombstoned row is the purge worker's input and has to be findable"
    );
    assert_eq!(
        listed
            .iter()
            .find(|row| row.asset_id == asset)
            .and_then(|row| row.retention_until),
        Some(Timestamp::UNIX_EPOCH),
        "the signed retention floor rides on the row, because the purge reads it from there \
         rather than from a server policy",
    );

    let purged = ok(index.purge(&asset).await, "purge").expect("the row exists");
    assert!(purged.blobs.is_empty());
    assert_eq!(
        purged.state,
        AssetState::Tombstoned,
        "removing the row would make the deletion invisible to a client that has not synced \
         since it, rather than final",
    );
    assert_eq!(
        ok(
            index.purge(&AssetId::new("purge-no-such-asset")).await,
            "purge nothing"
        ),
        None,
    );
}

/// A restore clears the retention floor.
pub async fn a_restore_clears_the_retention_floor(index: &dyn AssetIndex) {
    let (asset, _) = publish(index, "restore-floor", 1).await;
    let head = ok(index.read(&asset).await, "read")
        .expect("the row exists")
        .chain_head;
    let mut delete = op("restore-floor", 1, OpAction::Delete, head, manifest(22));
    delete.retention_until = Some(Timestamp::UNIX_EPOCH);
    ok(index.apply_op(delete).await, "delete");

    let head = ok(index.read(&asset).await, "read")
        .expect("the row exists")
        .chain_head;
    ok(
        index
            .apply_op(op(
                "restore-floor",
                1,
                OpAction::TrashRestore,
                head,
                manifest(23),
            ))
            .await,
        "restore",
    );
    let row = ok(index.read(&asset).await, "read").expect("the row exists");
    assert_eq!(row.state, AssetState::Visible);
    assert_eq!(
        row.retention_until, None,
        "an asset back in the live set has no window left to run out"
    );
}

/// Run every case against `index`, in order.
/// A serving hold is placed, reported on every reference, and lifted.
///
/// `S-C17`. Three separate claims, and the third is the one a takedown's reversibility rests
/// on: a lifted hold leaves the row exactly as it was, because the hold never touched the
/// state, the blobs or the feed position in the first place.
pub async fn a_serving_hold_is_placed_reported_and_lifted(index: &dyn AssetIndex) {
    let (asset, _) = publish(index, "takedown", 1).await;
    let metadata = address("takedownm1");
    record(index, &asset, blob(BlobRole::Original, "takedowno1")).await;

    let before = ok(index.read(&asset).await, "read the row").expect("the asset exists");
    assert_eq!(before.hold, None, "a published asset is under no hold");

    assert_eq!(
        ok(
            index.set_hold(&asset, Some(ServingHold::Takedown)).await,
            "place a takedown",
        ),
        HoldOutcome::Applied
    );
    assert_eq!(
        ok(
            index.set_hold(&asset, Some(ServingHold::Takedown)).await,
            "re-place the same takedown",
        ),
        HoldOutcome::Unchanged,
        "re-applying a takedown is not a second takedown, and a moderation log must not say it was",
    );

    // Every blob of the asset carries the hold, because the hold is the asset's.
    for held in [&metadata, &address("takedowno1")] {
        let reference = ok(index.find_reference(held).await, "look up a held blob")
            .expect("a hold does not remove the reference");
        assert_eq!(reference.hold, Some(ServingHold::Takedown));
    }

    let during = ok(index.read(&asset).await, "read the held row").expect("the asset exists");
    assert_eq!(
        during.state, before.state,
        "a hold is a serving constraint; it must not move the asset's lifecycle state",
    );
    assert_eq!(
        during.blobs, before.blobs,
        "a takedown is not a destruction: every blob reference survives it",
    );
    assert_eq!(
        during.sync_seq, before.sync_seq,
        "a hold publishes nothing; it is invisible to the feed",
    );

    // A legal hold replaces a takedown rather than stacking with it.
    assert_eq!(
        ok(
            index.set_hold(&asset, Some(ServingHold::LegalHold)).await,
            "escalate to a legal hold",
        ),
        HoldOutcome::Applied
    );

    assert_eq!(
        ok(index.set_hold(&asset, None).await, "lift the hold"),
        HoldOutcome::Applied
    );
    assert_eq!(
        ok(index.set_hold(&asset, None).await, "lift it again"),
        HoldOutcome::Unchanged
    );

    let after = ok(index.read(&asset).await, "read the freed row").expect("the asset exists");
    assert_eq!(after.hold, None);
    assert_eq!(
        after.blobs, before.blobs,
        "lifting restores serving and nothing else, because nothing else changed",
    );
    let reference = ok(
        index.find_reference(&metadata).await,
        "look up a freed blob",
    )
    .expect("the reference is still live");
    assert_eq!(reference.hold, None);
}

/// A hold on an asset that does not exist is `NotFound`, not a silently created row.
pub async fn holding_an_unknown_asset_is_not_found(index: &dyn AssetIndex) {
    assert_eq!(
        ok(
            index
                .set_hold(&AssetId::new("hold-nobody"), Some(ServingHold::Takedown))
                .await,
            "hold a stranger",
        ),
        HoldOutcome::NotFound
    );
}

// ---------------------------------------------------------------------------------------
// The scrub's walk
// ---------------------------------------------------------------------------------------

/// Every row is walked, whatever its state, and the walk resumes where it stopped.
///
/// The scrub's input, and it was uncovered until the Postgres adapter arrived — which is
/// exactly the shape of gap a shared suite exists to close. Two properties, and both are the
/// port's own words:
///
/// - *"Every row, whatever its state. A scrub that skipped pending or tombstoned rows would
///   skip exactly the rows a half-finished write leaves behind."*
/// - *"an interrupted pass resumes where it stopped rather than starting over — which for a
///   store worth scrubbing is the difference between a check that finishes and one that never
///   does."*
pub async fn the_row_walk_covers_every_state_and_resumes(index: &dyn AssetIndex) {
    // One row in each state, so a filter on state would drop one of them.
    let pending_only = pending("walk", 1);
    let unseen = pending_only.asset_id.clone();
    ok(index.reserve(pending_only).await, "reserve a pending row");
    let (visible, _) = publish(index, "walk", 2).await;
    let (deleted, _) = publish(index, "walk", 3).await;
    ok(
        index.tombstone(&deleted, Timestamp::UNIX_EPOCH).await,
        "tombstone a row",
    );

    for page_size in [1_usize, 2, 50] {
        let mut cursor: Option<AssetId> = None;
        let mut seen: Vec<AssetId> = Vec::new();
        loop {
            let page = ok(
                index.rows(cursor.as_ref(), page_size).await,
                "walk the asset rows",
            );
            if page.is_empty() {
                break;
            }
            assert!(
                page.len() <= page_size,
                "a page of {} exceeded the requested {page_size}",
                page.len()
            );
            for row in &page {
                if let Some(previous) = seen.last() {
                    assert!(
                        &row.asset_id > previous,
                        "the walk went backwards: {} came after {previous}",
                        row.asset_id
                    );
                }
                seen.push(row.asset_id.clone());
            }
            cursor = page.last().map(|row| row.asset_id.clone());
        }

        for expected in [&unseen, &visible, &deleted] {
            assert!(
                seen.contains(expected),
                "the walk at page size {page_size} missed {expected}; a scrub that skips a \
                 pending or tombstoned row skips exactly the rows a half-finished write leaves \
                 behind"
            );
        }
        let mut unique = seen.clone();
        unique.dedup();
        assert_eq!(
            unique.len(),
            seen.len(),
            "the walk at page size {page_size} returned a row twice"
        );
    }
}

/// The walk's order is the identifier's own byte order, not a locale's.
///
/// Asset ids are the manifest's client-chosen `file_id`, so they are full of punctuation and are
/// not a shape the suite gets to pick. A backend ordering them under a locale collation — which
/// is what `en_US.utf8` does, ignoring `-` at the primary level — walks them in a different
/// order from the deterministic double, and a cursor handed between the two skips rows. The
/// three ids below are the smallest set where byte order and a punctuation-ignoring collation
/// disagree.
pub async fn the_row_walk_orders_by_the_identifiers_own_bytes(index: &dyn AssetIndex) {
    let ids = ["walkord-a-b", "walkord-ab", "walkord-a-c"];
    for id in ids {
        let mut row = pending("walkord", 0);
        row.asset_id = AssetId::new(id);
        ok(index.reserve(row).await, "reserve a row");
    }

    let walked: Vec<String> = ok(index.rows(None, 100).await, "walk the asset rows")
        .into_iter()
        .filter(|row| row.asset_id.as_str().starts_with("walkord-"))
        .map(|row| row.asset_id.as_str().to_owned())
        .collect();
    let mut expected: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
    expected.sort();
    assert_eq!(
        walked, expected,
        "the walk must order asset ids by their bytes, so a cursor means the same thing to \
         every adapter"
    );
}

/// An album page is the owner's sequence filtered to one album, with its own head (`S-C51`).
///
/// Positions are the owner's numbers, so a member's per-album anti-rewind mark is the same value
/// the owner's feed carries; gaps are the other albums' entries. The head is the album's last
/// entry, not the owner's allocator: a member who has seen it is caught up whatever the owner
/// minted elsewhere since.
pub async fn an_album_page_is_the_owners_sequence_filtered_to_one_album(index: &dyn AssetIndex) {
    let owner = OwnerId::new("albumpage-owner");
    let shared = AlbumId::new("albumpage-shared");
    let private = AlbumId::new("albumpage-private");
    let mut in_shared = Vec::new();
    for (n, album) in [(1_u32, &shared), (2, &private), (3, &shared), (4, &private)] {
        let row = PendingAsset {
            asset_id: AssetId::new(format!("albumpage-asset-{n}")),
            owner_id: owner.clone(),
            album_id: album.clone(),
            protocol_version: "2026-01-01".to_owned(),
            crypto_suite_id: 1,
            created_at: Timestamp::UNIX_EPOCH,
        };
        let asset = row.asset_id.clone();
        ok(index.reserve(row).await, "reserve a row");
        record(
            index,
            &asset,
            blob(BlobRole::Provenance, &format!("albumpage-p{n}")),
        )
        .await;
        let seq = record(
            index,
            &asset,
            blob(BlobRole::Metadata, &format!("albumpage-m{n}")),
        )
        .await
        .expect("landing the index tier publishes");
        if album == &shared {
            in_shared.push(seq);
        }
    }

    let page = ok(
        index.album_feed_page(&owner, &shared, 0, 10).await,
        "page an album",
    );
    assert_eq!(
        page.iter().map(|entry| entry.sync_seq).collect::<Vec<_>>(),
        in_shared,
        "the album page is the owner's sequence, filtered, in order"
    );
    assert!(page.iter().all(|entry| entry.album_id == shared));
    assert_eq!(
        ok(
            index.album_head_seq(&owner, &shared).await,
            "read an album head"
        ),
        in_shared[1],
        "the head is the album's last entry, not the owner's allocator"
    );
    assert!(
        ok(
            index.album_head_seq(&owner, &shared).await,
            "read an album head"
        ) < ok(index.head_seq(&owner).await, "read the owner's head"),
        "the owner minted more in another album since"
    );

    // Resuming past the first shared entry yields exactly the second.
    let resumed = ok(
        index
            .album_feed_page(&owner, &shared, in_shared[0], 10)
            .await,
        "resume an album page",
    );
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].sync_seq, in_shared[1]);
    // And a bounded page is bounded.
    assert_eq!(
        ok(
            index.album_feed_page(&owner, &shared, 0, 1).await,
            "page one"
        )
        .len(),
        1
    );

    // A row another account filed under the *same* album id is not this album's: the page is
    // bound to the owner the album record names, which is what the index is keyed on.
    let squatter = OwnerId::new("albumpage-squatter");
    let row = PendingAsset {
        asset_id: AssetId::new("albumpage-asset-squat"),
        owner_id: squatter.clone(),
        album_id: shared.clone(),
        protocol_version: "2026-01-01".to_owned(),
        crypto_suite_id: 1,
        created_at: Timestamp::UNIX_EPOCH,
    };
    ok(index.reserve(row).await, "reserve a row");
    record(
        index,
        &AssetId::new("albumpage-asset-squat"),
        blob(BlobRole::Provenance, "albumpage-ps"),
    )
    .await;
    record(
        index,
        &AssetId::new("albumpage-asset-squat"),
        blob(BlobRole::Metadata, "albumpage-ms"),
    )
    .await;
    assert_eq!(
        ok(
            index.album_feed_page(&owner, &shared, 0, 10).await,
            "page an album"
        )
        .len(),
        2,
        "another owner's row under the same album id is not on this owner's album page"
    );
    assert_eq!(
        ok(
            index.album_head_seq(&owner, &shared).await,
            "read an album head"
        ),
        in_shared[1]
    );

    // An album nothing was filed into: empty, head zero — not an error.
    let unknown = AlbumId::new("albumpage-unknown");
    assert!(
        ok(
            index.album_feed_page(&owner, &unknown, 0, 10).await,
            "page an unknown album"
        )
        .is_empty()
    );
    assert_eq!(
        ok(
            index.album_head_seq(&owner, &unknown).await,
            "head of an unknown album"
        ),
        0
    );
}

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
    an_album_page_is_the_owners_sequence_filtered_to_one_album(index).await;
    an_albums_numbers_are_monotonic_with_gaps(index).await;
    a_tombstone_reaches_every_reader(index).await;
    tombstoning_a_pending_row_publishes_nothing(index).await;
    a_late_blob_does_not_revive_a_tombstone(index).await;
    the_duplicate_lookup_is_scoped_to_owner_and_album(index).await;
    the_serving_lookup_ignores_unpublished_rows(index).await;
    the_serving_lookup_reports_state_and_original_holding(index).await;
    a_live_holder_outranks_a_deleted_one(index).await;
    a_lifecycle_write_extends_the_chain(index).await;
    re_applying_a_manifest_is_a_replay(index).await;
    racing_identical_submissions_apply_once_and_replay_once(index).await;
    delete_and_restore_are_both_publishable_changes(index).await;
    an_epoch_that_regresses_the_album_is_refused(index).await;
    an_op_on_an_asset_that_is_not_the_callers_is_not_found(index).await;
    a_lifecycle_write_repoints_the_provenance_blob(index).await;
    a_replace_repoints_the_original_the_upload_path_may_not_move(index).await;
    a_lifecycle_write_retains_the_manifest_it_supersedes(index).await;
    references_are_counted_from_the_rows_that_name_them(index).await;
    purging_drops_the_references_and_keeps_the_tombstone(index).await;
    a_restore_clears_the_retention_floor(index).await;
    a_serving_hold_is_placed_reported_and_lifted(index).await;
    holding_an_unknown_asset_is_not_found(index).await;
    the_row_walk_covers_every_state_and_resumes(index).await;
    the_row_walk_orders_by_the_identifiers_own_bytes(index).await;
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
