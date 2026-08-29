//! The one suite every blob-store adapter must pass.
//!
//! # Why this lives in `src/`
//!
//! For the same reason [`crate::store::conformance`] does: it is part of the contract, not a
//! test of one implementation. Everything here is generic over [`Harness`], so it costs nothing
//! in a binary that never instantiates it, and an adapter written in another crate — a future
//! object-storage backend, a network filesystem worth its own smoke tier — runs the whole thing
//! with one [`run_all`] call. "The in-memory double behaves like the filesystem store" is an
//! assertion here rather than an assumption anywhere.
//!
//! # What is asserted, and what cannot be
//!
//! Two properties are **type-level** and have no runtime case, by design — a test for them would
//! be a test that the code compiles:
//!
//! - **A malformed content address cannot reach a store.** [`ContentAddress`] has one
//!   constructor and no public field, so there is no value to pass. What that constructor
//!   rejects is asserted where it lives, in [`super::address`].
//! - **No operation takes an arbitrary serializable payload.** There is no `T: Serialize` in
//!   [`super`]; a violation is a compile error in the port.
//!
//! The invariant that *is* asserted here from every direction is the one the layout decision
//! rests on: **enumeration is complete, ordered by content address, and resumable** — see
//! [`enumeration_yields_every_blob_in_content_address_order`],
//! [`enumeration_resumes_from_its_cursor_without_gaps_or_repeats`] and
//! [`a_partially_populated_shard_tree_enumerates_completely`]. Those three are why the shard is
//! affordable, and they are the cases a new adapter is most likely to fail.
//!
//! # Reusing a harness
//!
//! Every case scopes its addresses and upload identifiers to itself, so cases may share one
//! harness and [`run_all`] does.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;

use super::address::ContentAddress;
use super::{
    BlobError, BlobFuture, BlobStat, BlobStore, Placement, QuarantineReason, QuarantinedBlob,
};
use crate::store::UploadId;

/// The store under test, plus the one thing a suite cannot do through the port: plant something
/// that is not a blob.
///
/// `plant_debris` is the seam that keeps the suite backend-agnostic, exactly as `advance` is for
/// the state-store suite. A filesystem harness writes a file into the shard tree; the in-memory
/// double records the name. Either way [`enumeration_reports_what_is_not_a_blob_as_debris`] is
/// the same case, and neither adapter gets to decide that debris is invisible.
pub trait Harness: Send + Sync {
    /// The blob store under test.
    fn store(&self) -> &dyn BlobStore;

    /// Plant an entry that is not a blob, named relative to the finalized store's root.
    fn plant_debris(&self, relative: &str) -> BlobFuture<'_, ()>;
}

/// Unwrap a store result, failing with the operation that was expected to work.
fn ok<T>(result: Result<T, BlobError>, operation: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming adapter must succeed at {operation}: {error}"),
    }
}

/// Unwrap an expected-present value.
fn present<T>(value: Option<T>, what: &str) -> T {
    match value {
        Some(value) => value,
        None => panic!("{what} must be present"),
    }
}

/// A deterministic 64-hex address for `case`, distinct per `tag`.
///
/// Derived rather than written out so a case can ask for a hundred blobs across a hundred shards
/// without a hundred literals, and derived *deterministically* so a failure is reproducible.
fn hex(case: &str, tag: u16) -> String {
    let mut state: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in case.bytes().chain(tag.to_be_bytes()) {
        state ^= u64::from(byte);
        state = state.wrapping_mul(0x0000_0100_0000_01b3);
    }

    let mut words = [0_u64; 4];
    for word in &mut words {
        *word = state;
        state = state.wrapping_mul(0x0000_0100_0000_01b3).rotate_left(17) ^ 0x9e37_79b9_7f4a_7c15;
    }
    format!(
        "{:016x}{:016x}{:016x}{:016x}",
        words[0], words[1], words[2], words[3]
    )
}

/// The address `case`/`tag` names.
fn address(case: &str, tag: u16) -> ContentAddress {
    parse(&hex(case, tag))
}

/// The address `case`/`tag` names, forced under the shard `prefix` names.
///
/// The shard is a prefix of the address, so choosing the prefix chooses the directories — which
/// is how a case builds a deliberately lopsided tree.
fn shard_address(prefix: &str, case: &str, tag: u16) -> ContentAddress {
    let mut hex = hex(case, tag);
    hex.replace_range(..prefix.len(), prefix);
    parse(&hex)
}

/// Parse a hex string the suite itself produced.
fn parse(hex: &str) -> ContentAddress {
    match ContentAddress::parse(hex) {
        Ok(address) => address,
        Err(error) => panic!("the suite's own fixture must be an address: {error}"),
    }
}

/// The upload identifier `case`/`tag` names.
fn upload(case: &str, tag: &str) -> UploadId {
    UploadId::new(format!("{case}-{tag}"))
}

/// A rejection record, for the quarantine cases.
fn reason(detail: &str) -> QuarantineReason {
    QuarantineReason {
        code: "error.upload.envelope_malformed".to_owned(),
        detail: detail.to_owned(),
        at: Timestamp::UNIX_EPOCH,
    }
}

/// Walk the whole store `limit` entries at a time, collecting entries and debris.
///
/// The loop is bounded: an adapter whose cursor does not advance fails here rather than hanging
/// a test run.
async fn walk(harness: &dyn Harness, limit: usize) -> (Vec<BlobStat>, BTreeSet<String>) {
    let store = harness.store();
    let mut entries = Vec::new();
    let mut debris = BTreeSet::new();
    let mut cursor: Option<ContentAddress> = None;

    for _ in 0..1_000 {
        let page = ok(store.enumerate(cursor.as_ref(), limit).await, "enumerate");
        assert!(
            page.entries.len() <= limit,
            "a page must respect the limit it was asked for"
        );
        entries.extend(page.entries);
        debris.extend(page.debris);
        match page.next {
            Some(next) => cursor = Some(next),
            None => return (entries, debris),
        }
    }

    panic!("enumeration did not terminate: the cursor is not advancing");
}

/// Store `bytes` at `address` through the streaming path, as a real upload does.
async fn stage_and_commit(
    harness: &dyn Harness,
    upload: &UploadId,
    address: &ContentAddress,
    bytes: &[u8],
) -> Placement {
    let store = harness.store();
    ok(store.begin(upload).await, "begin");
    ok(store.append(upload, 0, bytes).await, "append");
    ok(store.commit(upload, address).await, "commit")
}

// ===========================================================================================
// Staging
// ===========================================================================================

/// A staged file grows only at its end, and its length is readable at every step.
pub async fn staging_appends_only_at_the_end_and_tracks_its_length(h: &dyn Harness) {
    let store = h.store();
    let upload = upload("staging-appends", "a");

    assert_eq!(
        ok(store.staged_len(&upload).await, "staged_len"),
        None,
        "nothing is staged before the stage is opened"
    );

    ok(store.begin(&upload).await, "begin");
    assert_eq!(
        ok(store.staged_len(&upload).await, "staged_len"),
        Some(0),
        "an opened stage is empty, not absent"
    );

    assert_eq!(ok(store.append(&upload, 0, b"hello ").await, "append"), 6);
    assert_eq!(ok(store.append(&upload, 6, b"world").await, "append"), 11);
    assert_eq!(
        ok(store.staged_len(&upload).await, "staged_len"),
        Some(11),
        "the staged length is the truth a session's counter caches"
    );

    ok(store.begin(&upload).await, "begin");
    assert_eq!(
        ok(store.staged_len(&upload).await, "staged_len"),
        Some(0),
        "re-opening a stage starts it over rather than appending to the last attempt"
    );
}

/// An append anywhere but the end writes nothing and names the offset to resume from.
pub async fn an_append_at_the_wrong_offset_is_refused_and_names_the_resume_point(h: &dyn Harness) {
    let store = h.store();
    let upload = upload("wrong-offset", "a");
    ok(store.begin(&upload).await, "begin");
    ok(store.append(&upload, 0, b"0123456789").await, "append");

    for offset in [0_u64, 5, 11, 4096] {
        match store.append(&upload, offset, b"xxx").await {
            Err(BlobError::OffsetMismatch {
                offset: refused,
                actual,
                ..
            }) => {
                assert_eq!(refused, offset);
                assert_eq!(actual, 10, "the refusal carries the offset to resume from");
            }
            Err(other) => panic!("an out-of-order append is an offset mismatch, not {other}"),
            Ok(_) => panic!("an append at {offset} must be refused; the file is append-only"),
        }
    }

    assert_eq!(
        ok(store.staged_len(&upload).await, "staged_len"),
        Some(10),
        "a refused append writes nothing"
    );
    assert_eq!(
        ok(store.append(&upload, 10, b"ab").await, "append"),
        12,
        "and the stage still accepts the append that belongs at the end"
    );
}

/// Appending to an upload nothing opened is absence, named as such.
pub async fn appending_to_an_upload_that_was_never_begun_is_not_staged(h: &dyn Harness) {
    let store = h.store();
    let upload = upload("never-begun", "a");

    match store.append(&upload, 0, b"bytes").await {
        Err(BlobError::NotStaged { upload: named }) => assert_eq!(named, upload),
        Err(other) => panic!("an unopened stage is NotStaged, not {other}"),
        Ok(_) => panic!("there is nothing to append to"),
    }
    assert_eq!(ok(store.staged_len(&upload).await, "staged_len"), None);
}

/// Abandoning clears the stage and reports whether there was anything to clear.
pub async fn abandoning_removes_the_staged_bytes_and_says_whether_there_were_any(h: &dyn Harness) {
    let store = h.store();
    let upload = upload("abandoning", "a");

    assert!(
        !ok(store.abandon(&upload).await, "abandon"),
        "abandoning nothing removes nothing"
    );

    ok(store.begin(&upload).await, "begin");
    ok(store.append(&upload, 0, b"partial upload").await, "append");
    assert!(ok(store.abandon(&upload).await, "abandon"));

    assert_eq!(ok(store.staged_len(&upload).await, "staged_len"), None);
    assert!(
        !ok(store.abandon(&upload).await, "abandon"),
        "abandoning is idempotent, which is what a startup scrub needs"
    );
}

/// The staged listing is ordered, holds every open upload, and drops one the moment it commits.
pub async fn the_staged_listing_is_ordered_and_holds_only_open_uploads(h: &dyn Harness) {
    let store = h.store();
    let case = "staged-listing";
    let later = upload(case, "b-later");
    let earlier = upload(case, "a-earlier");
    let committed = upload(case, "c-committed");

    ok(store.begin(&later).await, "begin");
    ok(store.begin(&earlier).await, "begin");

    let listed = ok(store.staged().await, "staged");
    let ours: Vec<&UploadId> = listed
        .iter()
        .filter(|id| id.as_str().starts_with(case))
        .collect();
    assert_eq!(
        ours,
        vec![&earlier, &later],
        "the listing is ordered by identifier, so a scrub's progress is resumable"
    );

    let address = address(case, 1);
    stage_and_commit(h, &committed, &address, b"finalized").await;

    let listed = ok(store.staged().await, "staged");
    assert!(
        !listed.contains(&committed),
        "a committed upload's bytes are in the store, not on the stage"
    );

    ok(store.abandon(&earlier).await, "abandon");
    ok(store.abandon(&later).await, "abandon");
}

/// An identifier no adapter can name a staged file with is refused by every staging operation.
pub async fn an_upload_id_that_cannot_name_a_file_is_refused_by_every_operation(h: &dyn Harness) {
    let store = h.store();
    let address = address("malformed-upload", 1);

    for hostile in ["", "..", "../../etc/passwd", "a/b", "with space", "dot.bin"] {
        let upload = UploadId::new(hostile);
        for (operation, result) in [
            ("begin", store.begin(&upload).await),
            ("append", store.append(&upload, 0, b"x").await.map(|_| ())),
            ("staged_len", store.staged_len(&upload).await.map(|_| ())),
            ("abandon", store.abandon(&upload).await.map(|_| ())),
            ("commit", store.commit(&upload, &address).await.map(|_| ())),
        ] {
            match result {
                Err(BlobError::MalformedUpload { .. }) => {}
                Err(other) => panic!(
                    "`{hostile}` must be refused as malformed by {operation}, not as {other}"
                ),
                Ok(()) => panic!("`{hostile}` must not reach {operation}"),
            }
        }
    }
}

// ===========================================================================================
// Commit
// ===========================================================================================

/// Committing places the staged bytes at their address, byte for byte, and clears the stage.
pub async fn committing_places_the_staged_bytes_at_their_content_address(h: &dyn Harness) {
    let store = h.store();
    let case = "commit-places";
    let upload = upload(case, "a");
    let address = address(case, 1);
    let bytes = b"ciphertext the server cannot read".to_vec();

    ok(store.begin(&upload).await, "begin");
    ok(store.append(&upload, 0, &bytes[..10]).await, "append");
    ok(store.append(&upload, 10, &bytes[10..]).await, "append");

    assert_eq!(
        ok(store.commit(&upload, &address).await, "commit"),
        Placement::Stored
    );

    let stat = present(ok(store.stat(&address).await, "stat"), "a committed blob");
    assert_eq!(stat.address, address);
    assert_eq!(stat.size, bytes.len() as u64);
    assert_eq!(
        present(
            ok(store.read_at(&address, 0, bytes.len()).await, "read_at"),
            "a committed blob's bytes"
        ),
        bytes,
        "the bytes read back are the bytes staged"
    );
    assert_eq!(
        ok(store.staged_len(&upload).await, "staged_len"),
        None,
        "the stage is cleared by the commit that consumed it"
    );
}

/// A finalized blob is immutable, and a duplicate is never stored twice.
pub async fn committing_onto_an_occupied_address_keeps_the_bytes_already_there(h: &dyn Harness) {
    let store = h.store();
    let case = "commit-dedup";
    let address = address(case, 1);
    let first = upload(case, "first");
    let second = upload(case, "second");

    assert_eq!(
        stage_and_commit(h, &first, &address, b"the original bytes").await,
        Placement::Stored
    );
    assert_eq!(
        stage_and_commit(h, &second, &address, b"different").await,
        Placement::AlreadyPresent,
        "a blob already present is never stored twice"
    );

    assert_eq!(
        present(
            ok(store.read_at(&address, 0, 64).await, "read_at"),
            "the first blob"
        ),
        b"the original bytes".to_vec(),
        "a finalized blob is immutable; the second write must not have replaced it"
    );
    assert_eq!(
        ok(store.staged_len(&second).await, "staged_len"),
        None,
        "the losing stage is cleared too, or it becomes an orphan the scrub must chase"
    );
}

/// Committing an upload nothing staged is absence, named as such.
pub async fn committing_an_upload_that_was_never_staged_is_not_staged(h: &dyn Harness) {
    let store = h.store();
    let case = "commit-unstaged";
    let upload = upload(case, "a");
    let address = address(case, 1);

    match store.commit(&upload, &address).await {
        Err(BlobError::NotStaged { upload: named }) => assert_eq!(named, upload),
        Err(other) => panic!("an unstaged commit is NotStaged, not {other}"),
        Ok(_) => panic!("there were no bytes to commit"),
    }
    assert_eq!(ok(store.stat(&address).await, "stat"), None);
}

/// Bytes on the stage are not in the store, however far the upload has got.
pub async fn staged_bytes_are_not_a_blob_until_they_are_committed(h: &dyn Harness) {
    let store = h.store();
    let case = "stage-invisible";
    let upload = upload(case, "a");
    let address = address(case, 1);

    ok(store.begin(&upload).await, "begin");
    ok(store.append(&upload, 0, b"most of a blob").await, "append");

    assert_eq!(
        ok(store.stat(&address).await, "stat"),
        None,
        "an in-flight upload has no content address yet"
    );
    let (entries, _) = walk(h, 128).await;
    assert!(
        !entries.iter().any(|entry| entry.address == address),
        "an in-flight upload must not appear to a scrub, a GC sweep or a rebuild"
    );

    ok(store.abandon(&upload).await, "abandon");
}

// ===========================================================================================
// Put and read
// ===========================================================================================

/// A whole-blob write stores its bytes and, like a commit, never overwrites.
pub async fn put_stores_bytes_at_its_address_and_never_overwrites(h: &dyn Harness) {
    let store = h.store();
    let case = "put";
    let envelope = address(case, 1);
    let empty = address(case, 2);

    assert_eq!(
        ok(
            store.put(&envelope, b"a signed manifest envelope").await,
            "put"
        ),
        Placement::Stored
    );
    assert_eq!(
        ok(
            store.put(&envelope, b"something else entirely").await,
            "put"
        ),
        Placement::AlreadyPresent
    );
    assert_eq!(
        present(
            ok(store.read_at(&envelope, 0, 128).await, "read_at"),
            "the blob"
        ),
        b"a signed manifest envelope".to_vec()
    );

    assert_eq!(ok(store.put(&empty, b"").await, "put"), Placement::Stored);
    let stat = present(ok(store.stat(&empty).await, "stat"), "an empty blob");
    assert_eq!(stat.size, 0, "an empty blob is a blob, not an absence");
}

/// An address nothing was ever stored under reads as absent, not as a failure.
pub async fn an_absent_address_stats_and_reads_as_none(h: &dyn Harness) {
    let store = h.store();
    let missing = address("absent", 1);

    assert_eq!(ok(store.stat(&missing).await, "stat"), None);
    assert_eq!(ok(store.read_at(&missing, 0, 16).await, "read_at"), None);
    assert!(
        !ok(store.remove(&missing).await, "remove"),
        "removing what is not there removes nothing"
    );
}

/// A ranged read returns exactly its window, and clamps rather than refusing at the end.
pub async fn a_ranged_read_returns_exactly_its_window_and_clamps_at_the_end(h: &dyn Harness) {
    let store = h.store();
    let case = "ranged-read";
    let address = address(case, 1);
    let bytes: Vec<u8> = (0..=255_u8).collect();
    ok(store.put(&address, &bytes).await, "put");

    let read = |offset: u64, len: usize| store.read_at(&address, offset, len);

    assert_eq!(
        present(ok(read(0, 16).await, "read_at"), "a window"),
        bytes[..16].to_vec()
    );
    assert_eq!(
        present(ok(read(64, 32).await, "read_at"), "a window"),
        bytes[64..96].to_vec(),
        "a window in the middle is the bytes at that offset"
    );
    assert_eq!(
        present(ok(read(250, 32).await, "read_at"), "a clamped window"),
        bytes[250..].to_vec(),
        "a window running past the end stops there"
    );
    assert_eq!(
        present(ok(read(256, 8).await, "read_at"), "an empty window"),
        Vec::<u8>::new(),
        "a window starting at the end is empty, not absent"
    );
    assert_eq!(
        present(ok(read(0, 0).await, "read_at"), "a zero-length window"),
        Vec::<u8>::new()
    );
}

// ===========================================================================================
// Enumeration — the operation the shard exists for
// ===========================================================================================

/// A full walk yields every blob exactly once, in ascending content-address order.
pub async fn enumeration_yields_every_blob_in_content_address_order(h: &dyn Harness) {
    let store = h.store();
    let case = "enumerate-order";

    let mut expected = BTreeMap::new();
    for tag in 0..24_u16 {
        let address = address(case, tag);
        let bytes = vec![b'x'; usize::from(tag) + 1];
        ok(store.put(&address, &bytes).await, "put");
        expected.insert(address, bytes.len() as u64);
    }

    let (entries, _) = walk(h, 128).await;
    let ours: Vec<&BlobStat> = entries
        .iter()
        .filter(|entry| expected.contains_key(&entry.address))
        .collect();

    assert_eq!(
        ours.len(),
        expected.len(),
        "a walk must find every blob in the store, whichever shard it landed in"
    );
    for (entry, (address, size)) in ours.iter().zip(expected.iter()) {
        assert_eq!(
            &entry.address, address,
            "entries are ordered by content address"
        );
        assert_eq!(
            entry.size, *size,
            "an entry carries the size a scrub compares"
        );
    }

    let addresses: Vec<&ContentAddress> = entries.iter().map(|entry| &entry.address).collect();
    let mut sorted = addresses.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        addresses, sorted,
        "the whole listing is ordered and free of repeats, which is what makes the cursor work"
    );
}

/// Paging the walk yields the same sequence as taking it whole, at any page size.
pub async fn enumeration_resumes_from_its_cursor_without_gaps_or_repeats(h: &dyn Harness) {
    let store = h.store();
    let case = "enumerate-cursor";
    for tag in 0..17_u16 {
        ok(store.put(&address(case, tag), b"page me").await, "put");
    }

    let (whole, _) = walk(h, 4_096).await;
    for limit in [1_usize, 2, 3, 5, 16] {
        let (paged, _) = walk(h, limit).await;
        assert_eq!(
            paged, whole,
            "a walk paged {limit} at a time must be the walk taken whole — no gaps, no repeats"
        );
    }
}

/// A store with nothing in it enumerates to nothing.
pub async fn an_empty_store_enumerates_to_nothing_rather_than_failing(h: &dyn Harness) {
    let store = h.store();
    let case = "enumerate-empty";

    // Whatever else the shared harness holds, a cursor past the last address is the empty tail.
    let past_the_end = parse(&"f".repeat(64));
    let page = ok(store.enumerate(Some(&past_the_end), 16).await, "enumerate");
    assert!(
        page.entries.is_empty(),
        "a cursor past the last address is the end of the walk"
    );
    assert_eq!(page.next, None);

    // And a store emptied of one case's blobs still walks.
    let address = address(case, 1);
    ok(store.put(&address, b"transient").await, "put");
    assert!(ok(store.remove(&address).await, "remove"));
    let (entries, _) = walk(h, 8).await;
    assert!(!entries.iter().any(|entry| entry.address == address));
}

/// A lopsided tree — most shards absent, some holding one blob — walks completely.
///
/// This is design/filesystem/server.md's "partially-populated shard tree enumerates without
/// error": the layout creates shard directories on demand, so the overwhelming majority of the
/// 65,536 possible ones never exist, and a walk that assumed otherwise would fail on every real
/// store.
pub async fn a_partially_populated_shard_tree_enumerates_completely(h: &dyn Harness) {
    let store = h.store();
    let case = "lopsided";

    let mut expected = BTreeSet::new();
    // Several blobs sharing one first-level shard but split across second-level ones.
    for (index, prefix) in ["aa00", "aa01", "aaff", "aa02"].iter().enumerate() {
        let address = shard_address(prefix, case, index as u16);
        ok(store.put(&address, b"crowded shard").await, "put");
        expected.insert(address);
    }
    // A first-level shard holding exactly one blob, far from the others.
    for (index, prefix) in ["00ab", "ffee", "7f7f"].iter().enumerate() {
        let address = shard_address(prefix, case, 100 + index as u16);
        ok(store.put(&address, b"lonely shard").await, "put");
        expected.insert(address);
    }

    let (entries, _) = walk(h, 3).await;
    let found: BTreeSet<ContentAddress> = entries
        .into_iter()
        .map(|entry| entry.address)
        .filter(|address| expected.contains(address))
        .collect();

    assert_eq!(
        found, expected,
        "every blob is found whether its shard holds one or many, and absent shards are not \
         entries"
    );
}

/// What is not a blob is reported as debris, never silently skipped and never mistaken for one.
///
/// The integrity scrub's debris inventory is one of enumeration's three consumers; a walk that
/// dropped an unrecognised entry is how a crashed temp file becomes permanent.
pub async fn enumeration_reports_what_is_not_a_blob_as_debris(h: &dyn Harness) {
    let store = h.store();
    let case = "debris";
    let address = shard_address("ab", case, 1);
    ok(store.put(&address, b"a real blob").await, "put");

    let planted = [
        // A stray file inside a real shard, a temp file a crash left behind mid-`put`, a file
        // loose in a first-level shard, and one loose at the top of the store. Each is named
        // exactly as a walk of the finalized store would name it, so the two adapters report
        // one string rather than two spellings of one idea.
        "ab/cd/not-a-blob.txt".to_owned(),
        format!("ab/cd/.{}.0000.tmp", "a".repeat(64)),
        "ab/loose-file".to_owned(),
        "stray-file".to_owned(),
    ];
    for name in &planted {
        ok(h.plant_debris(name).await, "plant_debris");
    }

    let (entries, debris) = walk(h, 64).await;
    for name in &planted {
        assert!(
            debris.contains(name),
            "`{name}` is not a blob and must be inventoried as debris; the walk reported {debris:?}"
        );
    }
    assert!(
        entries.iter().any(|entry| entry.address == address),
        "debris beside a blob must not hide the blob"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.address == address)
            .count(),
        1,
        "and must not be counted as one"
    );
}

// ===========================================================================================
// Removal and hold
// ===========================================================================================

/// Removing a blob drops it from lookup and from the walk together.
pub async fn removing_a_blob_drops_it_from_lookup_and_from_enumeration(h: &dyn Harness) {
    let store = h.store();
    let case = "remove";
    let kept = shard_address("cc11", case, 1);
    let swept = shard_address("cc11", case, 2);

    ok(store.put(&kept, b"referenced").await, "put");
    ok(store.put(&swept, b"orphaned").await, "put");

    assert!(ok(store.remove(&swept).await, "remove"));
    assert_eq!(ok(store.stat(&swept).await, "stat"), None);
    assert!(
        !ok(store.remove(&swept).await, "remove"),
        "a second sweep of the same blob removes nothing"
    );

    let (entries, _) = walk(h, 8).await;
    assert!(!entries.iter().any(|entry| entry.address == swept));
    assert!(
        entries.iter().any(|entry| entry.address == kept),
        "removing a blob must not disturb its shard-mates"
    );
    assert!(ok(store.remove(&kept).await, "remove"));

    let (entries, _) = walk(h, 8).await;
    assert!(
        !entries.iter().any(|entry| entry.address == kept),
        "emptying a shard entirely still leaves a walkable tree"
    );
}

/// Quarantine pulls a blob out of the store and preserves it with the reason it was pulled.
pub async fn quarantining_pulls_a_blob_out_of_the_store_and_records_why(h: &dyn Harness) {
    let store = h.store();
    let case = "quarantine";
    let address = address(case, 1);
    ok(
        store
            .put(&address, b"an envelope that failed validation")
            .await,
        "put",
    );

    let reason = reason("prior_provenance_hash is not a 64-character hex string");
    assert!(ok(
        store.quarantine(&address, reason.clone()).await,
        "quarantine"
    ));

    assert_eq!(
        ok(store.stat(&address).await, "stat"),
        None,
        "a held blob is out of the store"
    );
    let (entries, _) = walk(h, 64).await;
    assert!(
        !entries.iter().any(|entry| entry.address == address),
        "and out of the walk, so a rebuild does not keep re-reading it"
    );

    let held = ok(store.quarantined().await, "quarantined");
    assert!(
        held.contains(&QuarantinedBlob {
            address: address.clone(),
            reason,
        }),
        "the bytes are preserved for forensic inspection, with the check that rejected them"
    );

    let addresses: Vec<&ContentAddress> = held.iter().map(|blob| &blob.address).collect();
    let mut sorted = addresses.clone();
    sorted.sort();
    assert_eq!(
        addresses, sorted,
        "the hold is ordered like the store it came from"
    );
}

/// Holding an address with nothing behind it holds nothing.
pub async fn quarantining_an_absent_address_holds_nothing(h: &dyn Harness) {
    let store = h.store();
    let address = address("quarantine-absent", 1);

    assert!(!ok(
        store.quarantine(&address, reason("nothing to hold")).await,
        "quarantine"
    ));
    assert!(
        !ok(store.quarantined().await, "quarantined")
            .iter()
            .any(|blob| blob.address == address),
        "a dangling reference is surfaced by the caller, not invented by the store"
    );
}

// ===========================================================================================
// The whole suite
// ===========================================================================================

/// Run every case above against one harness, in order.
///
/// For a backend where standing up a harness is expensive this is the entry point. A unit-tier
/// adapter should prefer calling the cases individually, one test each, so a failure names the
/// property that broke rather than the suite.
pub async fn run_all(h: &dyn Harness) {
    staging_appends_only_at_the_end_and_tracks_its_length(h).await;
    an_append_at_the_wrong_offset_is_refused_and_names_the_resume_point(h).await;
    appending_to_an_upload_that_was_never_begun_is_not_staged(h).await;
    abandoning_removes_the_staged_bytes_and_says_whether_there_were_any(h).await;
    the_staged_listing_is_ordered_and_holds_only_open_uploads(h).await;
    an_upload_id_that_cannot_name_a_file_is_refused_by_every_operation(h).await;

    committing_places_the_staged_bytes_at_their_content_address(h).await;
    committing_onto_an_occupied_address_keeps_the_bytes_already_there(h).await;
    committing_an_upload_that_was_never_staged_is_not_staged(h).await;
    staged_bytes_are_not_a_blob_until_they_are_committed(h).await;

    put_stores_bytes_at_its_address_and_never_overwrites(h).await;
    an_absent_address_stats_and_reads_as_none(h).await;
    a_ranged_read_returns_exactly_its_window_and_clamps_at_the_end(h).await;

    enumeration_yields_every_blob_in_content_address_order(h).await;
    enumeration_resumes_from_its_cursor_without_gaps_or_repeats(h).await;
    an_empty_store_enumerates_to_nothing_rather_than_failing(h).await;
    a_partially_populated_shard_tree_enumerates_completely(h).await;
    enumeration_reports_what_is_not_a_blob_as_debris(h).await;

    removing_a_blob_drops_it_from_lookup_and_from_enumeration(h).await;
    quarantining_pulls_a_blob_out_of_the_store_and_records_why(h).await;
    quarantining_an_absent_address_holds_nothing(h).await;
}
