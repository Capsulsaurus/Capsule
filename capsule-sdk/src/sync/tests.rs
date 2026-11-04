//! Pure reconciliation tests for the client-side halves of the sync doc's
//! Validation bullets: forward-version rejection, rewind
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
        derivative_manifests: Vec::new(),
        original_held: true,
        purged: false,
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

/// **Persistence round-trip.** A state advanced by a page is torn
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

// ─── Wire decoding: derivative manifests (#556) ──────────────────────────────

/// A wire entry as the server renders it, with `extra` merged in.
fn wire_entry(extra: serde_json::Value) -> rest::types::SyncEntry {
    let mut entry = serde_json::json!({
        "asset_id": "asset",
        "album_id": "album",
        "protocol_version": "2026-05-31",
        "sync_seq": 1,
        "change": "created",
        "manifest_cbor": BASE64.encode([0xa0]),
        "metadata_blob": "metadata",
        "blobs": [{ "role": "derivative", "hash": "thumb", "size": 8 }],
        "original_held": false,
        "changed_at": "1970-01-01T00:00:00Z",
    });
    for (key, value) in extra.as_object().unwrap() {
        entry[key] = value.clone();
    }
    serde_json::from_value(entry).unwrap()
}

/// Each inlined manifest decodes to its exact bytes, in the order the server sent them.
#[test]
fn derivative_manifests_decode_to_their_exact_bytes() {
    let thumbnail = b"signed thumbnail manifest".to_vec();
    let preview = b"signed preview manifest".to_vec();
    let decoded = decode_entry(wire_entry(serde_json::json!({
        "derivative_manifests": [BASE64.encode(&thumbnail), BASE64.encode(&preview)],
    })))
    .unwrap();
    assert_eq!(decoded.derivative_manifests, vec![thumbnail, preview]);
    assert_eq!(
        decoded.blobs.derivatives.len(),
        1,
        "the blob list is unchanged"
    );
}

/// A server that predates the field sends no key, and that is an asset with no manifests yet,
/// not a malformed entry — native clients stay lenient across versions.
#[test]
fn an_entry_without_derivative_manifests_decodes_to_none() {
    let decoded = decode_entry(wire_entry(serde_json::json!({}))).unwrap();
    assert!(decoded.derivative_manifests.is_empty());
}

// ─── Wire decoding: the purge signal (#885) ──────────────────────────────────

/// A purged tombstone decodes with its signal set.
#[test]
fn a_purged_tombstone_decodes_with_the_signal() {
    let decoded = decode_entry(wire_entry(serde_json::json!({
        "change": "deleted",
        "manifest_cbor": null,
        "purged": true,
    })))
    .unwrap();
    assert_eq!(decoded.kind, ChangeKind::Deleted);
    assert!(decoded.purged);
    assert!(decoded.manifest_cbor.is_empty());
}

/// A server that predates the signal sends no key, and that is no purge: the tombstone behaves
/// as it did before the signal existed.
#[test]
fn a_tombstone_without_the_signal_is_not_purged() {
    let decoded = decode_entry(wire_entry(serde_json::json!({
        "change": "deleted",
        "manifest_cbor": null,
    })))
    .unwrap();
    assert!(!decoded.purged);
}

/// The signal means something only on a tombstone; a live entry carrying it is not purged.
#[test]
fn the_signal_on_a_live_entry_is_ignored() {
    let decoded = decode_entry(wire_entry(serde_json::json!({ "purged": true }))).unwrap();
    assert_eq!(decoded.kind, ChangeKind::Created);
    assert!(!decoded.purged);
}

// ─── Sync epoch (#697) ───────────────────────────────────────────────────────

/// A stale cursor is recognised by its code, and nothing else is.
#[test]
fn a_stale_cursor_is_recognised_by_its_code() {
    let stale = SyncError::Rejected {
        code: Some(error_codes::SYNC_CURSOR_STALE.to_owned()),
        message: String::new(),
    };
    assert!(stale.is_stale_cursor());
    let invalid = SyncError::Rejected {
        code: Some(error_codes::SYNC_CURSOR_INVALID.to_owned()),
        message: String::new(),
    };
    assert!(!invalid.is_stale_cursor());
    assert!(!SyncError::Transport("down".to_owned()).is_stale_cursor());
}

/// After an epoch change the client starts from zero with no floor, so the rebuilt numbering —
/// which may sit below the marks it held — applies rather than reading as a rewind.
#[test]
fn a_resync_after_an_epoch_change_drops_the_cursor_and_the_marks() {
    let a = album(11);
    let mut state = SyncState::new(MAX_KNOWN);
    state
        .apply_page(&page(vec![entry(&a, 9, "2026-05-31")], 0x40))
        .unwrap();
    assert_eq!(state.high_water(&a), Some(9));

    state.reset_for_resync();
    assert!(state.cursor().is_start());
    assert_eq!(state.high_water(&a), None);

    // The rebuilt library numbers the same asset lower; it applies and becomes the new floor.
    state
        .apply_page(&page(vec![entry(&a, 4, "2026-05-31")], 0x41))
        .unwrap();
    assert_eq!(state.high_water(&a), Some(4));
    let err = state
        .apply_page(&page(vec![entry(&a, 3, "2026-05-31")], 0x42))
        .unwrap_err();
    assert!(
        matches!(err, SyncError::Rewind { high_water: 4, .. }),
        "the anti-rewind layer is re-established from the new epoch's first page"
    );
}

/// A manifest that is not base64 is a server that cannot be talked to, as for `manifest_cbor`.
#[test]
fn a_derivative_manifest_that_is_not_base64_is_malformed() {
    let refused = decode_entry(wire_entry(serde_json::json!({
        "derivative_manifests": ["not base64!"],
    })));
    assert!(matches!(refused, Err(SyncError::MalformedManifest(_))));
}

/// **The protocol window (issue #674).** A feed pull is a read: it is served whatever window
/// the server advertises, and the window it carried is latched, so the next write to that
/// origin is judged against it.
#[tokio::test]
async fn a_feed_pull_is_served_and_latches_the_window() {
    let server = crate::testmock::MockServer::start(|_req| {
        crate::testmock::MockResponse::new(200, "OK")
            .header("X-Capsule-Protocol-Min", "2026-01-01")
            .header("X-Capsule-Protocol-Max", "2099-12-31")
            .header("X-Capsule-Min-Client-Build", "999.0.0")
            .json_body(r#"{"entries":[],"next_cursor":"c1","has_more":false}"#)
    })
    .await;
    let consumer = SyncConsumer::with_static_token(&server.base_url(), "test-token").unwrap();
    let page = consumer.pull(&SyncCursor::start(), 10).await.unwrap();
    assert!(page.entries.is_empty(), "the read is served");

    let origin = reqwest::Url::parse(&server.base_url()).unwrap();
    let latched = crate::net::negotiation::advertised(&origin).expect("the window was latched");
    assert_eq!(latched.min_client_build.as_deref(), Some("999.0.0"));
    assert_eq!(
        crate::net::negotiation::upgrade_required_for(&origin).map(|refusal| refusal.reason),
        Some(crate::net::negotiation::UpgradeReason::BuildBelowFloor)
    );
}

// ─── Provenance history ──────────────────────────────────────────────────────

/// A blob source holding a set of blobs at their addresses, answering `404` for any other, and
/// counting what it was asked for.
struct Blobs {
    held: HashMap<String, Vec<u8>>,
    asked: std::sync::Mutex<Vec<String>>,
}

impl Blobs {
    fn new(blobs: impl IntoIterator<Item = Vec<u8>>) -> Self {
        Self {
            held: blobs
                .into_iter()
                .map(|bytes| {
                    (
                        capsule_core::crypto::hash::hash_bytes(&bytes).to_hex(),
                        bytes,
                    )
                })
                .collect(),
            asked: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl crate::fetch::BlobSource for Blobs {
    async fn get_range(
        &self,
        hash: &str,
        start: u64,
        _max_len: Option<u64>,
    ) -> crate::fetch::RangeOutcome {
        self.asked.lock().unwrap().push(hash.to_owned());
        match self.held.get(hash) {
            Some(bytes) => crate::fetch::RangeOutcome::Complete {
                bytes: bytes[usize::try_from(start).unwrap().min(bytes.len())..].to_vec(),
            },
            None => crate::fetch::RangeOutcome::Status {
                status: 404,
                code: None,
            },
        }
    }
}

/// A real asset's chain: created, edited twice and deleted, each record its canonical CBOR,
/// oldest first. The library is returned so it outlives the bytes' borrow-free contents.
fn real_chain() -> (tempfile::TempDir, Vec<Vec<u8>>) {
    use capsule_library::lifecycle::Workspace;

    let dir = tempfile::TempDir::new().unwrap();
    let lib = dir.path().join("library");
    std::fs::create_dir_all(&lib).unwrap();
    let src = dir.path().join("photo.jpg");
    std::fs::write(&src, b"\xFF\xD8\xFF a chain walked back to its create").unwrap();
    let fast = capsule_core::crypto::primitives::Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    };
    let mut ws = Workspace::create_with_params(&lib, b"passphrase", fast).unwrap();
    let album = ws.default_album_id();
    ws.ensure_album(album, "Imports").unwrap();
    let asset = ws.import_asset(album, &src).unwrap();
    ws.tag_add(&asset, "dusk").unwrap();
    ws.tag_add(&asset, "night").unwrap();
    ws.soft_delete(&asset, 30).unwrap();
    let records = ws
        .asset(&asset)
        .unwrap()
        .chain
        .records()
        .iter()
        .map(|record| capsule_core::cbor::to_canonical_vec(record).unwrap())
        .collect();
    (dir, records)
}

fn fetcher() -> crate::fetch::RangedFetcher {
    crate::fetch::RangedFetcher::new(crate::net::ConnectionClass::Unmetered)
}

/// The walk follows each record's link back to the create and hands the records before the
/// head over oldest first, byte for byte as the server holds them, asking for each once.
#[tokio::test]
async fn the_history_walk_follows_the_links_back_to_the_create() {
    let (_lib, chain) = real_chain();
    let (head, before) = chain.split_last().unwrap();
    let source = Blobs::new(chain.clone());

    let history = fetch_history(&fetcher(), &source, head).await.unwrap();
    assert_eq!(history, before.to_vec());
    assert_eq!(source.asked().len(), before.len(), "{:?}", source.asked());
}

/// A create is its own start: nothing to walk, and nothing asked for.
#[tokio::test]
async fn a_create_has_no_history_to_fetch() {
    let (_lib, chain) = real_chain();
    let source = Blobs::new(chain.clone());
    let history = fetch_history(&fetcher(), &source, &chain[0]).await.unwrap();
    assert!(history.is_empty());
    assert!(source.asked().is_empty());
}

/// A record the server will not serve stops the walk and names the link it could not follow.
#[tokio::test]
async fn a_record_the_server_refuses_stops_the_walk_at_its_link() {
    let (_lib, chain) = real_chain();
    let missing = capsule_core::crypto::hash::hash_bytes(&chain[1]).to_hex();
    let source = Blobs::new([chain[0].clone(), chain[2].clone()]);
    match fetch_history(&fetcher(), &source, &chain[3]).await {
        Err(HistoryError::Fetch { hash, source }) => {
            assert_eq!(hash, missing);
            assert!(source.is_permanent(), "{source:?}");
        }
        other => panic!("{other:?}"),
    }
}

/// Bytes that are not the record a link names are refused by the fetch's content check, and a
/// head that is not a record is refused before anything is fetched.
#[tokio::test]
async fn what_is_not_the_named_record_is_refused() {
    let (_lib, chain) = real_chain();
    let wanted = capsule_core::crypto::hash::hash_bytes(&chain[2]).to_hex();
    let mut source = Blobs::new(chain.clone());
    source.held.insert(wanted.clone(), chain[0].clone());
    match fetch_history(&fetcher(), &source, &chain[3]).await {
        Err(HistoryError::Fetch {
            hash,
            source: crate::fetch::FetchError::IntegrityFailed { .. },
        }) => assert_eq!(hash, wanted),
        other => panic!("{other:?}"),
    }

    let source = Blobs::new(chain);
    match fetch_history(&fetcher(), &source, b"not a record").await {
        Err(HistoryError::Malformed { hash, .. }) => assert_eq!(hash, "head"),
        other => panic!("{other:?}"),
    }
    assert!(source.asked().is_empty());
}
