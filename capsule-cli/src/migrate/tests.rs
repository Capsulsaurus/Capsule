use std::path::PathBuf;

use capsule_library::lifecycle::UnmigratedShape;
use uuid::Uuid;

use super::*;

/// One instance of every skip reason. (A variant added to `MigrationSkip` already fails
/// to compile in [`describe_skip`]'s exhaustive `match`; add its row here with it.)
fn every_skip() -> Vec<MigrationSkip> {
    let a = Uuid::from_u128(0xA1);
    vec![
        MigrationSkip::InvalidAssetId("not-a-uuid".into()),
        MigrationSkip::UnknownShape,
        MigrationSkip::Undecodable("field rating: bad".into()),
        MigrationSkip::IdCollision(a),
        MigrationSkip::OriginalMissing(a),
        MigrationSkip::HashMismatch {
            asset_id: a,
            recorded: "aa".into(),
            actual: "bb".into(),
        },
        MigrationSkip::QuarantineConflict(a),
        MigrationSkip::OutsideMonthBucket {
            asset_id: a,
            dir: PathBuf::from("media/loose"),
        },
        MigrationSkip::UnusualExtension {
            asset_id: a,
            ext: "JPG".into(),
        },
        MigrationSkip::AlbumReadOnly {
            asset_id: a,
            album_id: Uuid::from_u128(0xB2),
        },
        MigrationSkip::Stranded(a),
    ]
}

#[test]
fn every_skip_reason_renders_a_distinct_message_carrying_its_facts() {
    let mut seen = std::collections::BTreeSet::new();
    for skip in every_skip() {
        let text = describe_skip(&skip);
        assert!(!text.is_empty(), "{skip:?}");
        assert!(!text.contains('{'), "an unfilled placeholder: {text}");
        assert!(
            seen.insert(text.clone()),
            "two reasons render alike: {text}"
        );
    }
    let a = Uuid::from_u128(0xA1).to_string();
    let mismatch = describe_skip(&MigrationSkip::HashMismatch {
        asset_id: Uuid::from_u128(0xA1),
        recorded: "aa".into(),
        actual: "bb".into(),
    });
    assert!(mismatch.contains(&a) && mismatch.contains("aa") && mismatch.contains("bb"));
    let ext = describe_skip(&MigrationSkip::UnusualExtension {
        asset_id: Uuid::from_u128(0xA1),
        ext: "JPG".into(),
    });
    assert!(ext.contains("JPG"), "{ext}");
}

#[test]
fn the_notice_is_silent_for_a_signed_library_and_counts_otherwise() {
    assert_eq!(unmigrated_notice(&[]), None);
    let one = UnmigratedSidecar {
        path: PathBuf::from("media/1970/1970-01/x.cbor"),
        asset_id: None,
        shape: UnmigratedShape::LegacyUnsigned,
    };
    let single = unmigrated_notice(std::slice::from_ref(&one)).expect("a notice");
    assert!(
        single.starts_with("1 sidecar in this library is"),
        "{single}"
    );
    let text = unmigrated_notice(&[one.clone(), one]).expect("a notice");
    assert!(text.starts_with("2 sidecars in this library are"), "{text}");
    assert!(text.contains("capsule library migrate"), "{text}");
}

#[test]
fn an_empty_run_with_nothing_remaining_says_there_was_nothing_to_do() {
    let page = render(&UnsignedMigrationReport::default(), 0);
    assert_eq!(page.lines().count(), 1, "{page}");
    assert!(page.contains(NOTHING_TO_MIGRATE), "{page}");
}

#[test]
fn a_run_page_lists_every_report_entry_then_the_summary() {
    let migrated = Uuid::from_u128(0x01);
    let trashed = Uuid::from_u128(0x02);
    let stack = Uuid::from_u128(0x03);
    let report = UnsignedMigrationReport {
        migrated: vec![migrated, trashed],
        trashed: vec![trashed],
        stacks: vec![stack],
        skipped: vec![(
            PathBuf::from("media/1970/1970-01/bad.cbor"),
            MigrationSkip::InvalidAssetId("bad".into()),
        )],
    };
    let page = render(&report, 1);
    let lines: Vec<&str> = page.lines().collect();
    assert_eq!(lines.len(), 6, "{page}");
    assert!(lines[0].contains(&migrated.to_string()), "{page}");
    assert!(lines[1].contains(&trashed.to_string()), "{page}");
    assert!(lines[2].contains(&trashed.to_string()), "{page}");
    assert!(lines[3].contains(&stack.to_string()), "{page}");
    assert!(
        lines[4].contains("bad.cbor")
            && lines[4].contains(&describe_skip(&MigrationSkip::InvalidAssetId("bad".into()))),
        "{page}"
    );
    assert_eq!(
        lines[5],
        "Migration: 2 migrated, 1 moved to trash, 1 stack(s), 1 skipped, 1 still unmigrated."
    );
}

#[test]
fn options_carry_the_requested_retention_and_the_default_album() {
    const FAST_KDF: capsule_core::crypto::primitives::Argon2Params =
        capsule_core::crypto::primitives::Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        };
    let dir = std::env::temp_dir().join(format!("capsule-cli-migrate-{}", nanoid::nanoid!()));
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).expect("scratch library dir");
    let ws = Workspace::create_with_params(&lib, b"pw", FAST_KDF).expect("create workspace");

    for retain_days in [0, 7, DEFAULT_RETAIN_DAYS, 365] {
        let opts = options(&ws, retain_days);
        assert_eq!(opts.trash_retain_days, retain_days);
        assert_eq!(opts.fallback_album, ws.default_album_id());
    }
    assert_ne!(ws.default_album_id(), Uuid::nil());

    drop(ws);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_run_that_left_sidecars_behind_reports_a_summary_not_nothing() {
    let page = render(&UnsignedMigrationReport::default(), 3);
    assert!(!page.contains(NOTHING_TO_MIGRATE), "{page}");
    assert!(page.contains('3'), "{page}");
}
