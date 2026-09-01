//! Pure reconciliation tests for the client-side halves of the download-sync
//! Validation bullets (slice `S-D2`): forward-version rejection, rewind
//! rejection, and the client-side of cursor authenticity (a validly-MAC'd but
//! *older* cursor is refused by the high-water mark). No network — the
//! anti-rewind contract is exercised entirely through [`SyncState::apply_page`].
//!
//! The wire round-trip (opaque cursor, high-water anti-rewind against the real
//! served feed) is covered by the real-server integration test in
//! `capsule-api/sync/src/tests/sdk_client.rs`.

#![allow(clippy::unwrap_used)]

use super::*;

/// The protocol version the client treats as its max known.
const MAX_KNOWN: &str = "2026-12-31";

fn album(tag: u8) -> Vec<u8> {
    vec![tag; 8]
}

fn entry(album_id: &[u8], sync_seq: u64, protocol: &str) -> FeedEntry {
    FeedEntry {
        album_id: album_id.to_vec(),
        sync_seq,
        protocol_version: protocol.to_string(),
        kind: ChangeKind::Created,
        asset_id: vec![0xaa; 8],
        manifest_cbor: vec![0xa0],
        metadata_blob: Vec::new(),
        blobs: BlobManifest::default(),
        original_held: true,
        changed_at: "1970-01-01T00:00:00Z".to_string(),
    }
}

fn page(entries: Vec<FeedEntry>, cursor_tag: u8) -> SyncPage {
    SyncPage {
        entries,
        next_cursor: SyncCursor::from_bytes(vec![cursor_tag; 41]),
        has_more: false,
    }
}

/// **Sync feed forward-version rejection.** A feed entry whose `protocol_version`
/// is above the client's max known is rejected without partial application — the
/// whole page is refused and no high-water mark advances.
#[test]
fn forward_version_entry_is_rejected_without_partial_apply() {
    let a = album(1);
    let mut state = SyncState::new(MAX_KNOWN);

    // A page that is fine up to a forward-version entry: the first entry must NOT
    // land, proving "no partial application".
    let p = page(
        vec![
            entry(&a, 1, "2026-05-31"),
            entry(&a, 2, "2027-01-01"), // beyond MAX_KNOWN
        ],
        0x10,
    );

    let err = state.apply_page(&p).unwrap_err();
    assert!(
        matches!(err, SyncError::ForwardVersion { ref entry_version, .. } if entry_version == "2027-01-01")
    );
    assert_eq!(
        err.error_code(),
        Some(capsule_i18n::error_codes::PROTOCOL_VERSION_UNSUPPORTED)
    );
    // Nothing applied: no high-water mark moved, cursor unchanged.
    assert_eq!(state.high_water(&a), None);
    assert!(state.cursor().is_start());
}

/// A boundary version equal to max-known is accepted (inclusive), one day past is
/// rejected.
#[test]
fn forward_version_boundary_is_inclusive() {
    let a = album(2);
    let mut state = SyncState::new(MAX_KNOWN);
    state
        .apply_page(&page(vec![entry(&a, 1, MAX_KNOWN)], 0x20))
        .expect("boundary version applies");
    assert_eq!(state.high_water(&a), Some(1));
}

/// A `protocol_version` that is not a `YYYY-MM-DD` date is a structural error.
#[test]
fn malformed_protocol_version_is_rejected() {
    let a = album(3);
    let mut state = SyncState::new(MAX_KNOWN);
    let err = state
        .apply_page(&page(vec![entry(&a, 1, "2026-5-31")], 0x30))
        .unwrap_err();
    assert!(matches!(err, SyncError::MalformedProtocol(v) if v == "2026-5-31"));
    assert_eq!(state.high_water(&a), None);
}

/// **Sync feed rewind rejection.** After applying a page that advances the
/// per-album high-water mark, a later page whose `sync_seq` regresses is surfaced,
/// not applied — and the state is left untouched.
#[test]
fn regressing_sync_seq_is_surfaced_not_applied() {
    let a = album(4);
    let mut state = SyncState::new(MAX_KNOWN);
    state
        .apply_page(&page(
            vec![
                entry(&a, 1, "2026-05-31"),
                entry(&a, 2, "2026-05-31"),
                entry(&a, 3, "2026-05-31"),
            ],
            0x40,
        ))
        .expect("initial page applies");
    assert_eq!(state.high_water(&a), Some(3));
    let cursor_after = state.cursor().clone();

    // A page whose sequences regress against the high-water mark (3).
    let rewind = page(
        vec![entry(&a, 2, "2026-05-31"), entry(&a, 3, "2026-05-31")],
        0x41,
    );
    let err = state.apply_page(&rewind).unwrap_err();
    assert!(matches!(
        err,
        SyncError::Rewind {
            entry_seq: 2,
            high_water: 3,
            ..
        }
    ));
    // State unchanged: high-water still 3, cursor not advanced to the rewind page.
    assert_eq!(state.high_water(&a), Some(3));
    assert_eq!(state.cursor(), &cursor_after);
}

/// **Sync cursor authenticity (client-side).** A malicious server can hand back
/// one of its own *older*, validly-MAC'd cursors; the page it yields replays
/// already-seen sequences, and the client's per-album high-water mark refuses the
/// rewind even though the cursor itself is authentic.
#[test]
fn older_valid_cursor_is_refused_by_the_high_water_mark() {
    let a = album(5);
    let mut state = SyncState::new(MAX_KNOWN);

    // Client advances through two pages (seqs 1..=4).
    state
        .apply_page(&page(
            vec![entry(&a, 1, "2026-05-31"), entry(&a, 2, "2026-05-31")],
            0x50,
        ))
        .unwrap();
    state
        .apply_page(&page(
            vec![entry(&a, 3, "2026-05-31"), entry(&a, 4, "2026-05-31")],
            0x51,
        ))
        .unwrap();
    assert_eq!(state.high_water(&a), Some(4));

    // The server replays the FIRST page's authentic cursor → seqs 1,2 again.
    let replay = page(
        vec![entry(&a, 1, "2026-05-31"), entry(&a, 2, "2026-05-31")],
        0x50,
    );
    let err = state.apply_page(&replay).unwrap_err();
    assert!(matches!(err, SyncError::Rewind { high_water: 4, .. }));
    assert_eq!(state.high_water(&a), Some(4));
}

/// Within a single page, per-album `sync_seq` must strictly increase; a
/// non-increasing pair inside one page is rejected.
#[test]
fn within_page_non_monotonic_is_rejected() {
    let a = album(6);
    let mut state = SyncState::new(MAX_KNOWN);
    let err = state
        .apply_page(&page(
            vec![
                entry(&a, 5, "2026-05-31"),
                entry(&a, 5, "2026-05-31"), // equal ⇒ not strictly increasing
            ],
            0x60,
        ))
        .unwrap_err();
    assert!(matches!(err, SyncError::Rewind { entry_seq: 5, .. }));
    assert_eq!(state.high_water(&a), None);
}

/// Distinct albums keep independent high-water marks: a low `sync_seq` in album B
/// is fine even after album A advanced far, and the cursor round-trips.
#[test]
fn per_album_high_water_is_independent_and_cursor_round_trips() {
    let a = album(7);
    let b = album(8);
    let mut state = SyncState::new(MAX_KNOWN);
    state
        .apply_page(&page(
            vec![
                entry(&a, 1, "2026-05-31"),
                entry(&a, 2, "2026-05-31"),
                entry(&b, 1, "2026-05-31"),
            ],
            0x70,
        ))
        .unwrap();
    assert_eq!(state.high_water(&a), Some(2));
    assert_eq!(state.high_water(&b), Some(1));

    // The cursor advanced to the page's next_cursor and is handed back verbatim.
    assert_eq!(state.cursor().as_bytes(), &[0x70u8; 41]);

    // Album B advancing does not require album A to move.
    state
        .apply_page(&page(vec![entry(&b, 2, "2026-05-31")], 0x71))
        .unwrap();
    assert_eq!(state.high_water(&a), Some(2));
    assert_eq!(state.high_water(&b), Some(2));
    assert_eq!(state.cursor().as_bytes(), &[0x71u8; 41]);
}

/// **Persistence round-trip (slice `S-D5`).** A state advanced by a page is torn
/// down to its persistable parts (cursor + per-album high-water marks) and
/// rehydrated through [`SyncState::restore`]; the restored state enforces the same
/// anti-rewind floor — an older authentic cursor's page is still refused — proving
/// the high-water mark survives a process restart, not just an in-memory run.
#[test]
fn restore_rehydrates_cursor_and_high_water_and_still_refuses_rewind() {
    let a = album(9);
    let b = album(10);
    let mut state = SyncState::new(MAX_KNOWN);
    state
        .apply_page(&page(
            vec![
                entry(&a, 1, "2026-05-31"),
                entry(&a, 2, "2026-05-31"),
                entry(&b, 5, "2026-05-31"),
            ],
            0x90,
        ))
        .unwrap();

    // Snapshot exactly what a client persists to its durable store.
    let cursor = state.cursor().clone();
    let marks: Vec<(Vec<u8>, u64)> = state
        .high_water_marks()
        .map(|(album, seq)| (album.to_vec(), seq))
        .collect();

    // Rehydrate as if on the next `capsule sync` run.
    let mut restored = SyncState::restore(MAX_KNOWN, cursor, marks);
    assert_eq!(restored.cursor().as_bytes(), &[0x90u8; 41]);
    assert_eq!(restored.high_water(&a), Some(2));
    assert_eq!(restored.high_water(&b), Some(5));

    // The restored high-water mark refuses a replayed older page (anti-rewind).
    let err = restored
        .apply_page(&page(vec![entry(&a, 1, "2026-05-31")], 0x91))
        .unwrap_err();
    assert!(matches!(err, SyncError::Rewind { high_water: 2, .. }));
    // A genuine forward entry still applies and advances the mark + cursor.
    restored
        .apply_page(&page(vec![entry(&a, 3, "2026-05-31")], 0x92))
        .unwrap();
    assert_eq!(restored.high_water(&a), Some(3));
    assert_eq!(restored.cursor().as_bytes(), &[0x92u8; 41]);
}
