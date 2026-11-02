//! Each rule invariant 20 re-applies, asserted at the rule that refuses it.

use capsule_core::crypto::hash::hash_bytes;
use serde::Serialize;

use super::*;
use crate::federation::pull::fixtures::{
    METADATA, PIN, album, asset, core, entry, now, original, page, record,
};

fn policy() -> UploadPolicy {
    UploadPolicy::default()
}

fn established() -> Rules {
    Rules::for_tier(PeerTier::Established)
}

fn probation() -> Rules {
    Rules::for_tier(PeerTier::Probation)
}

fn screen(page: SyncPageResponse, from: &PulledAlbum, rules: &Rules) -> Result<Screened, Refused> {
    let album = album();
    let policy = policy();
    let context = Context {
        album: &album,
        pin: PIN,
        policy: &policy,
        now: now(),
    };
    screen_page(page, from, &context, rules)
}

/// The rule a refusal names, or a panic naming what was accepted instead.
fn refused_at(result: Result<Screened, Refused>) -> &'static str {
    match result {
        Err(Refused::Invalid(invalid)) => invalid.rule,
        Err(other) => panic!("refused, but not as invalid: {other}"),
        Ok(screened) => panic!("accepted: {:?}", screened.page.entries),
    }
}

fn create(n: u128) -> Vec<u8> {
    record(core(asset(n), Action::Create, None, 1))
}

/// The state after `bytes` is accepted as asset `n`'s create at `sync_seq` 1.
fn holding(n: u128, bytes: &[u8]) -> PulledAlbum {
    screen(
        page(
            vec![entry(asset(n), 1, "created", Some(bytes))],
            "c1",
            false,
        ),
        &PulledAlbum::default(),
        &established(),
    )
    .expect("the create is accepted")
    .page
    .state
}

#[test]
fn a_well_formed_create_is_accepted_and_moves_the_album() {
    let bytes = create(1);
    let screened = screen(
        page(
            vec![entry(asset(1), 7, "created", Some(&bytes))],
            "c7",
            true,
        ),
        &PulledAlbum::default(),
        &established(),
    )
    .expect("accepted");

    assert!(screened.has_more);
    let state = &screened.page.state;
    assert_eq!(state.cursor.as_deref(), Some("c7"));
    assert_eq!(state.high_water, Some(7));
    assert_eq!(state.amk_version, Some(1));
    let head = hash_bytes(&bytes);
    assert_eq!(state.assets[&asset(1).to_string()].head, head);

    let accepted = &screened.page.entries[0];
    assert_eq!(accepted.manifest.as_deref(), Some(bytes.as_slice()));
    assert_eq!(accepted.head, Some(head));
    assert_eq!(accepted.metadata_blob, Some(hash_bytes(METADATA)));
    assert_eq!(accepted.change, Change::Created);
    assert_eq!(accepted.blobs.len(), 2);
    assert!(
        screened.page.metadata_blobs.is_empty(),
        "nothing fetched yet"
    );
    assert_eq!(
        screened.needed,
        vec![MetadataNeed {
            hash: hash_bytes(METADATA),
            size: Some(METADATA.len() as u64),
        }]
    );
}

#[test]
fn an_empty_page_keeps_the_position_and_takes_the_cursor() {
    let from = PulledAlbum {
        high_water: Some(4),
        ..PulledAlbum::default()
    };
    let screened = screen(page(Vec::new(), "c4b", false), &from, &established()).expect("ok");
    assert_eq!(screened.page.state.high_water, Some(4));
    assert_eq!(screened.page.state.cursor.as_deref(), Some("c4b"));
}

#[test]
fn probation_is_held_to_smaller_bounds_and_stricter_rules() {
    let (normal, strict) = (established(), probation());
    assert!(strict.page_size < normal.page_size);
    assert!(strict.max_pages < normal.max_pages);
    assert!(strict.max_page_bytes < normal.max_page_bytes);
    assert!(strict.max_metadata_blob_bytes < normal.max_metadata_blob_bytes);
    assert!(strict.manifest_required && !normal.manifest_required);
    assert!(!strict.unknown_fields_allowed && normal.unknown_fields_allowed);
}

#[test]
fn a_page_longer_than_asked_for_is_refused() {
    let rules = probation();
    let bytes = create(1);
    let entries = (1..=u64::from(rules.page_size) + 1)
        .map(|seq| entry(asset(1), seq, "created", Some(&bytes)))
        .collect();
    assert_eq!(
        refused_at(screen(
            page(entries, "c", false),
            &PulledAlbum::default(),
            &rules
        )),
        "page bound"
    );
}

#[test]
fn a_cursor_too_long_to_keep_is_refused() {
    let cursor = "c".repeat(MAX_CURSOR_BYTES + 1);
    assert_eq!(
        refused_at(screen(
            page(Vec::new(), &cursor, false),
            &PulledAlbum::default(),
            &established()
        )),
        "cursor bound"
    );
}

#[test]
fn an_entry_for_another_album_is_refused() {
    let bytes = create(1);
    let mut stray = entry(asset(1), 1, "created", Some(&bytes));
    stray["album_id"] = serde_json::json!(uuid::Uuid::from_u128(99).to_string());
    assert_eq!(
        refused_at(screen(
            page(vec![stray], "c", false),
            &PulledAlbum::default(),
            &established()
        )),
        "invariant 6"
    );
}

#[test]
fn a_sync_seq_that_does_not_advance_is_refused() {
    let one = create(1);
    let two = create(2);
    // Within a page.
    let entries = vec![
        entry(asset(1), 5, "created", Some(&one)),
        entry(asset(2), 5, "created", Some(&two)),
    ];
    assert_eq!(
        refused_at(screen(
            page(entries, "c", false),
            &PulledAlbum::default(),
            &established()
        )),
        "anti-rewind"
    );
    // Against what was already accepted.
    let from = PulledAlbum {
        high_water: Some(9),
        ..PulledAlbum::default()
    };
    assert_eq!(
        refused_at(screen(
            page(vec![entry(asset(1), 9, "created", Some(&one))], "c", false),
            &from,
            &established()
        )),
        "anti-rewind"
    );
}

#[test]
fn a_protocol_date_outside_the_window_or_off_the_pin_is_refused() {
    let bytes = create(1);
    let mut outside = entry(asset(1), 1, "created", Some(&bytes));
    outside["protocol_version"] = serde_json::json!("2099-01-01");
    assert_eq!(
        refused_at(screen(
            page(vec![outside], "c", false),
            &PulledAlbum::default(),
            &established()
        )),
        "invariant 1"
    );

    let mut off_pin = entry(asset(1), 1, "created", Some(&bytes));
    off_pin["protocol_version"] = serde_json::json!("2026-02-01");
    assert_eq!(
        refused_at(screen(
            page(vec![off_pin], "c", false),
            &PulledAlbum::default(),
            &established()
        )),
        "invariant 6"
    );

    let mut manifest = core(asset(1), Action::Create, None, 1);
    manifest.protocol_version = "2026-02-01".into();
    let bytes = record(manifest);
    assert_eq!(
        refused_at(screen(
            page(
                vec![entry(asset(1), 1, "created", Some(&bytes))],
                "c",
                false
            ),
            &PulledAlbum::default(),
            &established()
        )),
        "invariant 6"
    );
}

#[test]
fn a_manifest_for_another_asset_or_album_is_refused() {
    let other_asset = create(2);
    assert_eq!(
        refused_at(screen(
            page(
                vec![entry(asset(1), 1, "created", Some(&other_asset))],
                "c",
                false
            ),
            &PulledAlbum::default(),
            &established()
        )),
        "invariant 16"
    );

    let mut manifest = core(asset(1), Action::Create, None, 1);
    manifest.album_id = uuid::Uuid::from_u128(99);
    let other_album = record(manifest);
    assert_eq!(
        refused_at(screen(
            page(
                vec![entry(asset(1), 1, "created", Some(&other_album))],
                "c",
                false
            ),
            &PulledAlbum::default(),
            &established()
        )),
        "invariant 6"
    );
}

#[test]
fn bytes_that_are_not_a_manifest_are_refused() {
    let mut garbage = entry(asset(1), 1, "created", None);
    garbage["manifest_cbor"] = serde_json::json!("not base64!");
    assert_eq!(
        refused_at(screen(
            page(vec![garbage], "c", false),
            &PulledAlbum::default(),
            &established()
        )),
        "invariant 16"
    );
    assert_eq!(
        refused_at(screen(
            page(
                vec![entry(asset(1), 1, "created", Some(b"\xa1\x01\x02"))],
                "c",
                false
            ),
            &PulledAlbum::default(),
            &established()
        )),
        "invariant 16"
    );
}

#[test]
fn a_non_canonical_encoding_is_refused() {
    let bytes = create(1);
    // The same record with its `version: 1` written in two bytes rather than one: it decodes to
    // the same record, and hashes to a different head.
    let short = b"\x67version\x01";
    let at = bytes
        .windows(short.len())
        .position(|window| window == short)
        .expect("the record's version is in the bytes");
    let mut loose = bytes[..at].to_vec();
    loose.extend_from_slice(b"\x67version\x18\x01");
    loose.extend_from_slice(&bytes[at + short.len()..]);
    assert_eq!(
        ProvenanceRecord::decode(&loose).expect("the loose encoding decodes"),
        ProvenanceRecord::decode(&bytes).expect("the canonical encoding decodes"),
    );
    assert_eq!(
        refused_at(screen(
            page(
                vec![entry(asset(1), 1, "created", Some(&loose))],
                "c",
                false
            ),
            &PulledAlbum::default(),
            &established()
        )),
        "invariant 16"
    );
}

#[test]
fn an_action_that_disagrees_with_the_change_is_refused() {
    let bytes = create(1);
    assert_eq!(
        refused_at(screen(
            page(
                vec![entry(asset(1), 1, "deleted", Some(&bytes))],
                "c",
                false
            ),
            &PulledAlbum::default(),
            &established()
        )),
        "invariant 16"
    );
}

#[test]
fn an_unknown_suite_is_refused() {
    let mut manifest = core(asset(1), Action::Create, None, 1);
    manifest.crypto_suite_id = 0xffff;
    let bytes = record(manifest);
    assert_eq!(
        refused_at(screen(
            page(
                vec![entry(asset(1), 1, "created", Some(&bytes))],
                "c",
                false
            ),
            &PulledAlbum::default(),
            &established()
        )),
        "invariant 2"
    );
}

#[test]
fn a_timestamp_from_the_future_is_refused_and_an_old_one_is_not() {
    let mut future = core(asset(1), Action::Create, None, 1);
    future.timestamp = "2026-09-01T00:00:00Z".into();
    let bytes = record(future);
    assert_eq!(
        refused_at(screen(
            page(
                vec![entry(asset(1), 1, "created", Some(&bytes))],
                "c",
                false
            ),
            &PulledAlbum::default(),
            &established()
        )),
        "invariant 8"
    );

    // An album shared a year after its photos were taken is not malformed.
    let mut old = core(asset(1), Action::Create, None, 1);
    old.timestamp = "2025-01-01T00:00:00Z".into();
    let bytes = record(old);
    screen(
        page(
            vec![entry(asset(1), 1, "created", Some(&bytes))],
            "c",
            false,
        ),
        &PulledAlbum::default(),
        &established(),
    )
    .expect("an old timestamp is accepted");
}

#[test]
fn blob_references_are_bounded_and_well_formed() {
    let bytes = create(1);
    let with_blobs = |blobs: serde_json::Value| {
        let mut entry = entry(asset(1), 1, "created", Some(&bytes));
        entry["blobs"] = blobs;
        screen(
            page(vec![entry], "c", false),
            &PulledAlbum::default(),
            &established(),
        )
    };
    let hex = original().to_hex();
    assert_eq!(
        refused_at(with_blobs(
            serde_json::json!([{ "role": "original", "hash": hex, "size": 0 }])
        )),
        "invariant 4"
    );
    assert_eq!(
        refused_at(with_blobs(serde_json::json!([
            { "role": "original", "hash": hex, "size": policy().max_file_bytes() + 1 }
        ]))),
        "invariant 4"
    );
    assert_eq!(
        refused_at(with_blobs(serde_json::json!([
            { "role": "original", "hash": hex.to_uppercase(), "size": 1 }
        ]))),
        "invariant 3"
    );
    assert_eq!(
        refused_at(with_blobs(serde_json::json!([
            { "role": "original", "hash": "abcd", "size": 1 }
        ]))),
        "invariant 3"
    );
    assert_eq!(
        refused_at(with_blobs(
            serde_json::json!([{ "role": "backup", "hash": hex, "size": 1 }])
        )),
        "invariant 16"
    );
    let many: Vec<_> = (0..=established().max_blobs_per_entry)
        .map(|_| serde_json::json!({ "role": "derivative", "hash": hex, "size": 1 }))
        .collect();
    assert_eq!(
        refused_at(with_blobs(serde_json::json!(many))),
        "entry bound"
    );
}

#[test]
fn an_original_other_than_the_committed_one_is_refused() {
    let bytes = create(1);
    let mut swapped = entry(asset(1), 1, "created", Some(&bytes));
    swapped["blobs"][0]["hash"] = serde_json::json!(hash_bytes(b"another original").to_hex());
    assert_eq!(
        refused_at(screen(
            page(vec![swapped], "c", false),
            &PulledAlbum::default(),
            &established()
        )),
        "invariant 14"
    );
}

#[test]
fn a_metadata_blob_other_than_the_committed_one_is_refused() {
    let bytes = create(1);
    let mut swapped = entry(asset(1), 1, "created", Some(&bytes));
    swapped["metadata_blob"] = serde_json::json!(hash_bytes(b"other metadata").to_hex());
    assert_eq!(
        refused_at(screen(
            page(vec![swapped], "c", false),
            &PulledAlbum::default(),
            &established()
        )),
        "invariant 25"
    );

    let mut absent = entry(asset(1), 1, "created", Some(&bytes));
    absent["metadata_blob"] = serde_json::Value::Null;
    assert_eq!(
        refused_at(screen(
            page(vec![absent], "c", false),
            &PulledAlbum::default(),
            &established()
        )),
        "invariant 25"
    );
}

#[test]
fn derivative_manifests_are_bounded_and_must_decode() {
    let bytes = create(1);
    let mut undecodable = entry(asset(1), 1, "created", Some(&bytes));
    undecodable["derivative_manifests"] = serde_json::json!([BASE64.encode(b"\x01")]);
    assert_eq!(
        refused_at(screen(
            page(vec![undecodable], "c", false),
            &PulledAlbum::default(),
            &established()
        )),
        "invariant 16"
    );

    let mut many = entry(asset(1), 1, "created", Some(&bytes));
    many["derivative_manifests"] = serde_json::json!(vec![
        BASE64.encode(b"x");
        established().max_derivative_manifests
            + 1
    ]);
    assert_eq!(
        refused_at(screen(
            page(vec![many], "c", false),
            &PulledAlbum::default(),
            &established()
        )),
        "entry bound"
    );
}

#[test]
fn a_successor_is_accepted_and_the_head_it_replaced_cannot_return() {
    let first = create(1);
    let state = holding(1, &first);
    let first_head = hash_bytes(&first);

    let second = record(core(asset(1), Action::MetadataUpdate, Some(first_head), 1));
    let moved = screen(
        page(
            vec![entry(asset(1), 2, "updated", Some(&second))],
            "c2",
            false,
        ),
        &state,
        &established(),
    )
    .expect("the successor is accepted")
    .page
    .state;
    let held = &moved.assets[&asset(1).to_string()];
    assert_eq!(held.head, hash_bytes(&second));
    assert_eq!(held.superseded, vec![first_head]);

    // The home server serves the superseded manifest again, at a new position.
    assert_eq!(
        refused_at(screen(
            page(
                vec![entry(asset(1), 3, "updated", Some(&first))],
                "c3",
                false
            ),
            &moved,
            &established()
        )),
        "invariant 17"
    );
}

#[test]
fn a_second_create_for_a_held_asset_is_refused() {
    let state = holding(1, &create(1));
    let mut again = core(asset(1), Action::Create, None, 1);
    again.plaintext_size = 2048;
    let again = record(again);
    assert_eq!(
        refused_at(screen(
            page(
                vec![entry(asset(1), 2, "created", Some(&again))],
                "c",
                false
            ),
            &state,
            &established()
        )),
        "invariant 17"
    );
}

#[test]
fn the_held_head_served_again_is_no_change_and_not_a_revival() {
    // After a cursor restart the feed replays from zero: the current manifest arrives again.
    let bytes = create(1);
    let mut state = holding(1, &bytes);
    state.high_water = None;
    state.cursor = None;
    let replayed = screen(
        page(
            vec![entry(asset(1), 1, "created", Some(&bytes))],
            "c1",
            false,
        ),
        &state,
        &established(),
    )
    .expect("the held head is accepted again")
    .page
    .state;
    assert!(replayed.assets[&asset(1).to_string()].superseded.is_empty());
}

#[test]
fn a_successor_past_heads_this_server_never_saw_is_accepted() {
    // The feed is compacted: two edits between pulls arrive as the latest manifest only.
    let state = holding(1, &create(1));
    let unseen = hash_bytes(b"an intermediate record this server never pulled");
    let latest = record(core(asset(1), Action::MetadataUpdate, Some(unseen), 1));
    screen(
        page(
            vec![entry(asset(1), 3, "updated", Some(&latest))],
            "c3",
            false,
        ),
        &state,
        &established(),
    )
    .expect("the current manifest is accepted");
}

#[test]
fn an_assets_epoch_never_regresses_along_its_chain() {
    let first = record(core(asset(1), Action::Create, None, 3));
    let state = holding(1, &first);
    assert_eq!(state.assets[&asset(1).to_string()].amk_version, 3);
    let older = record(core(
        asset(1),
        Action::MetadataUpdate,
        Some(hash_bytes(&first)),
        2,
    ));
    assert_eq!(
        refused_at(screen(
            page(
                vec![entry(asset(1), 2, "updated", Some(&older))],
                "c",
                false
            ),
            &state,
            &established()
        )),
        "invariant 18"
    );
}

#[test]
fn a_position_re_minted_past_a_newer_epoch_is_accepted() {
    // The home server's index re-mints `sync_seq` when a late blob lands on a visible row, with
    // no new record: asset 1's epoch-1 create is served again after asset 2's epoch-2 create.
    let one = record(core(asset(1), Action::Create, None, 1));
    let two = record(core(asset(2), Action::Create, None, 2));
    let screened = screen(
        page(
            vec![
                entry(asset(1), 1, "created", Some(&one)),
                entry(asset(2), 2, "created", Some(&two)),
                entry(asset(1), 3, "created", Some(&one)),
            ],
            "c3",
            false,
        ),
        &PulledAlbum::default(),
        &established(),
    )
    .expect("a re-minted position is accepted");
    let state = &screened.page.state;
    assert_eq!(state.amk_version, Some(2));
    assert_eq!(state.assets[&asset(1).to_string()].amk_version, 1);
    assert!(state.assets[&asset(1).to_string()].superseded.is_empty());

    // A pull that starts after the re-mint sees asset 1 only at its new position.
    screen(
        page(
            vec![
                entry(asset(2), 2, "created", Some(&two)),
                entry(asset(1), 3, "created", Some(&one)),
            ],
            "c3",
            false,
        ),
        &PulledAlbum::default(),
        &established(),
    )
    .expect("an older epoch's asset at a later position is accepted");
}

#[test]
fn a_restart_after_a_key_rotation_replays_the_older_epoch() {
    let one = record(core(asset(1), Action::Create, None, 1));
    let two = record(core(asset(2), Action::Create, None, 2));
    let served = || {
        page(
            vec![
                entry(asset(1), 1, "created", Some(&one)),
                entry(asset(2), 2, "created", Some(&two)),
            ],
            "c2",
            false,
        )
    };
    let mut state = screen(served(), &PulledAlbum::default(), &established())
        .expect("the first pull is accepted")
        .page
        .state;
    // `409 error.sync.cursor_stale`: the position is forgotten, the heads and epoch kept.
    state.cursor = None;
    state.high_water = None;
    assert_eq!(state.amk_version, Some(2));
    screen(served(), &state, &established()).expect("the replay from zero is accepted");
}

/// Asset 1's create, then `action` directly on top of it at `sync_seq` 2, naming `metadata`.
fn binding_none(action: Action, metadata: &[u8]) -> (PulledAlbum, Result<Screened, Refused>) {
    let first = create(1);
    let state = holding(1, &first);
    let next = record(core(asset(1), action, Some(hash_bytes(&first)), 1));
    let mut served = entry(asset(1), 2, "updated", Some(&next));
    served["metadata_blob"] = serde_json::json!(hash_bytes(metadata).to_hex());
    let screened = screen(page(vec![served], "c2", false), &state, &established());
    (state, screened)
}

#[test]
fn a_head_binding_no_metadata_blob_is_served_with_the_one_its_asset_holds() {
    // What `GET /v1/sync` serves after a thumbnail lands: the derivative-add record as the head,
    // and the metadata blob the create bound, which the index keeps.
    for action in [Action::DerivativeAdd, Action::DerivativeReplace] {
        let (_, screened) = binding_none(action, METADATA);
        let screened = screened.unwrap_or_else(|refused| panic!("{action:?}: {refused}"));
        let accepted = &screened.page.entries[0];
        assert_eq!(accepted.metadata_blob, Some(hash_bytes(METADATA)));
        assert_eq!(
            screened.needed,
            vec![MetadataNeed {
                hash: hash_bytes(METADATA),
                size: Some(METADATA.len() as u64),
            }],
            "the blob is still fetched and checked against its address"
        );
        let held = &screened.page.state.assets[&asset(1).to_string()];
        assert_eq!(held.metadata_blob, Some(hash_bytes(METADATA)));
    }

    // The first pull of such an asset: the binding record is one the compacted feed no longer
    // serves.
    let unseen = hash_bytes(b"a create this server never pulled");
    let head = record(core(asset(1), Action::DerivativeAdd, Some(unseen), 1));
    screen(
        page(
            vec![entry(asset(1), 4, "created", Some(&head))],
            "c4",
            false,
        ),
        &PulledAlbum::default(),
        &established(),
    )
    .expect("a derivative-add head never pulled before is accepted");
}

#[test]
fn a_head_binding_no_metadata_blob_cannot_swap_the_one_its_asset_holds() {
    for action in [Action::DerivativeAdd, Action::DerivativeReplace] {
        let (_, screened) = binding_none(action, b"another metadata blob");
        assert_eq!(refused_at(screened), "invariant 25", "{action:?}");
    }

    // The held derivative-add head, served again at a re-minted position, may not swap it either.
    let first = create(1);
    let state = holding(1, &first);
    let head = record(core(
        asset(1),
        Action::DerivativeAdd,
        Some(hash_bytes(&first)),
        1,
    ));
    let state = screen(
        page(
            vec![entry(asset(1), 2, "updated", Some(&head))],
            "c2",
            false,
        ),
        &state,
        &established(),
    )
    .expect("the derivative-add is accepted")
    .page
    .state;
    let mut again = entry(asset(1), 3, "updated", Some(&head));
    again["metadata_blob"] = serde_json::json!(hash_bytes(b"another").to_hex());
    assert_eq!(
        refused_at(screen(
            page(vec![again], "c3", false),
            &state,
            &established()
        )),
        "invariant 25"
    );
}

#[test]
fn a_record_binding_none_that_skips_the_held_head_cannot_roll_back_its_metadata_blob() {
    // A genuine derivative-add or trash-restore that continues past records this server never
    // pulled, paired with an older genuine metadata blob of the same asset: the record proves
    // nothing about the blob, so the held one is kept and the named one is never fetched.
    let first = create(1);
    let state = holding(1, &first);
    let unseen = hash_bytes(b"a record this server never pulled");
    let older = b"an older metadata blob of the same asset";
    for action in [
        Action::DerivativeAdd,
        Action::DerivativeReplace,
        Action::TrashRestore,
    ] {
        let skipping = record(core(asset(1), action, Some(unseen), 1));
        let mut served = entry(asset(1), 2, "updated", Some(&skipping));
        served["metadata_blob"] = serde_json::json!(hash_bytes(older).to_hex());
        let screened = screen(page(vec![served], "c2", false), &state, &established())
            .unwrap_or_else(|refused| panic!("{action:?}: {refused}"));
        assert_eq!(
            screened.page.entries[0].metadata_blob,
            Some(hash_bytes(METADATA)),
            "{action:?}: the entry carries the held blob"
        );
        assert!(
            screened.needed.is_empty(),
            "{action:?}: the named blob is never fetched"
        );
        let held = &screened.page.state.assets[&asset(1).to_string()];
        assert_eq!(held.head, hash_bytes(&skipping), "{action:?}");
        assert_eq!(
            held.metadata_blob,
            Some(hash_bytes(METADATA)),
            "{action:?}: the held blob is kept"
        );
    }

    // Served with the held blob, the same record is accepted and its blob checked as before.
    let skipping = record(core(asset(1), Action::DerivativeAdd, Some(unseen), 1));
    let screened = screen(
        page(
            vec![entry(asset(1), 2, "updated", Some(&skipping))],
            "c2",
            false,
        ),
        &state,
        &established(),
    )
    .expect("a skipping record served with the held blob is accepted");
    assert_eq!(screened.needed.len(), 1);
}

#[test]
fn a_restore_brings_back_the_metadata_blob_the_delete_kept() {
    let first = create(1);
    let state = holding(1, &first);
    let delete = record(core(asset(1), Action::Delete, Some(hash_bytes(&first)), 1));
    let state = screen(
        page(
            vec![entry(asset(1), 2, "deleted", Some(&delete))],
            "c2",
            false,
        ),
        &state,
        &established(),
    )
    .expect("the delete is accepted")
    .page
    .state;
    assert_eq!(
        state.assets[&asset(1).to_string()].metadata_blob,
        Some(hash_bytes(METADATA)),
        "a delete names no blob, and the asset keeps the one it bound"
    );

    let restore = record(core(
        asset(1),
        Action::TrashRestore,
        Some(hash_bytes(&delete)),
        1,
    ));
    let served = entry(asset(1), 3, "updated", Some(&restore));
    screen(
        page(vec![served.clone()], "c3", false),
        &state,
        &established(),
    )
    .expect("the restore is accepted with the held blob");

    let mut swapped = served;
    swapped["metadata_blob"] = serde_json::json!(hash_bytes(b"another").to_hex());
    assert_eq!(
        refused_at(screen(
            page(vec![swapped], "c3", false),
            &state,
            &established()
        )),
        "invariant 25"
    );
}

#[test]
fn a_delete_is_accepted_and_a_purge_without_a_record_is_kept() {
    let first = create(1);
    let state = holding(1, &first);
    let delete = record(core(asset(1), Action::Delete, Some(hash_bytes(&first)), 1));
    let deleted = screen(
        page(
            vec![entry(asset(1), 2, "deleted", Some(&delete))],
            "c2",
            false,
        ),
        &state,
        &established(),
    )
    .expect("the delete is accepted");
    assert!(deleted.needed.is_empty(), "a delete binds no metadata blob");
    let state = deleted.page.state;
    assert!(state.assets[&asset(1).to_string()].deleted);

    let mut purged = entry(asset(1), 3, "deleted", None);
    purged["purged"] = serde_json::json!(true);
    let screened = screen(page(vec![purged], "c3", false), &state, &established())
        .expect("a purged tombstone is accepted");
    let accepted = &screened.page.entries[0];
    assert!(accepted.purged);
    assert_eq!(accepted.manifest, None);
    assert_eq!(screened.page.state.high_water, Some(3));
}

#[test]
fn a_purge_on_an_entry_that_is_no_delete_is_refused() {
    let bytes = create(1);
    let mut purged = entry(asset(1), 1, "created", Some(&bytes));
    purged["purged"] = serde_json::json!(true);
    assert_eq!(
        refused_at(screen(
            page(vec![purged], "c", false),
            &PulledAlbum::default(),
            &established()
        )),
        "invariant 16"
    );
}

#[test]
fn a_missing_manifest_is_passed_over_unless_the_peer_is_on_probation() {
    let entries = || vec![entry(asset(1), 4, "created", None)];
    let screened = screen(
        page(entries(), "c4", false),
        &PulledAlbum::default(),
        &established(),
    )
    .expect("passed over");
    assert!(screened.page.entries.is_empty());
    assert_eq!(screened.page.state.high_water, Some(4));

    assert_eq!(
        refused_at(screen(
            page(entries(), "c4", false),
            &PulledAlbum::default(),
            &probation()
        )),
        "invariant 16"
    );
}

#[test]
fn unknown_manifest_fields_are_refused_only_on_probation() {
    let mut future = core(asset(1), Action::Create, None, 1);
    future.unknown = capsule_core::cbor::from_slice(
        &capsule_core::cbor::to_canonical_vec(&std::collections::BTreeMap::from([(
            "zz_future_field",
            "a value a newer writer added",
        )]))
        .expect("encodes"),
    )
    .expect("decodes");
    let bytes = record(future);
    let entries = || vec![entry(asset(1), 1, "created", Some(&bytes))];

    screen(
        page(entries(), "c", false),
        &PulledAlbum::default(),
        &established(),
    )
    .expect("an established peer's newer fields are preserved");
    assert_eq!(
        refused_at(screen(
            page(entries(), "c", false),
            &PulledAlbum::default(),
            &probation()
        )),
        "invariant 16"
    );
}

#[test]
fn a_record_from_a_newer_writer_is_refused_without_blame() {
    #[derive(Serialize)]
    struct Newer<'a> {
        version: u16,
        asset_id: uuid::Uuid,
        manifest: &'a capsule_core::crypto::provenance::AssetManifest,
        prior_provenance_hash: Option<Hash32>,
    }
    let decoded = ProvenanceRecord::decode(&create(1)).expect("decodes");
    let bytes = capsule_core::cbor::to_canonical_vec(&Newer {
        version: 2,
        asset_id: decoded.asset_id,
        manifest: &decoded.manifest,
        prior_provenance_hash: None,
    })
    .expect("encodes");
    let refused = screen(
        page(
            vec![entry(asset(1), 1, "created", Some(&bytes))],
            "c",
            false,
        ),
        &PulledAlbum::default(),
        &established(),
    );
    assert!(
        matches!(&refused, Err(Refused::Newer { found: 2, .. })),
        "{refused:?}"
    );
}

#[test]
fn a_refused_page_leaves_the_state_it_was_given() {
    // Screening is pure: the caller's state is only replaced by a page that passed whole.
    let one = create(1);
    let from = PulledAlbum::default();
    let entries = vec![
        entry(asset(1), 1, "created", Some(&one)),
        entry(asset(2), 2, "created", Some(b"\x00")),
    ];
    assert!(screen(page(entries, "c", false), &from, &established()).is_err());
    assert_eq!(from, PulledAlbum::default());
}

#[test]
fn a_fetched_metadata_blob_must_be_whole_bounded_and_its_address() {
    let rules = established();
    let need = MetadataNeed {
        hash: hash_bytes(METADATA),
        size: Some(METADATA.len() as u64),
    };
    check_metadata_blob(&need, METADATA, &rules).expect("the committed blob");
    assert_eq!(
        check_metadata_blob(&need, b"an encrypted metadata blob, altered", &rules)
            .map_err(|invalid| invalid.rule),
        Err("invariant 13")
    );
    let unsized_need = MetadataNeed { size: None, ..need };
    assert_eq!(
        check_metadata_blob(&unsized_need, b"other bytes", &rules).map_err(|invalid| invalid.rule),
        Err("invariant 25")
    );
    let huge = vec![0_u8; usize::try_from(rules.max_metadata_blob_bytes).expect("fits") + 1];
    assert_eq!(
        check_metadata_blob(&unsized_need, &huge, &rules).map_err(|invalid| invalid.rule),
        Err("invariant 11")
    );
}

#[test]
fn the_read_limit_is_the_declared_size_never_past_the_bound() {
    let rules = probation();
    let need = |size| MetadataNeed {
        hash: hash_bytes(METADATA),
        size,
    };
    assert_eq!(metadata_read_limit(&need(Some(10)), &rules), 10);
    assert_eq!(
        metadata_read_limit(&need(Some(u64::MAX)), &rules),
        rules.max_metadata_blob_bytes
    );
    assert_eq!(
        metadata_read_limit(&need(None), &rules),
        rules.max_metadata_blob_bytes
    );
}
