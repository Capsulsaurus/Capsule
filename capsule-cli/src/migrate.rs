//! `capsule library migrate` — admit a pre-signed-path library's unsigned sidecars as signed
//! assets (issue `#456`, over slice `S-D24`).
//!
//! The verb itself is [`Workspace::migrate_unsigned_sidecars`]; this module is only the CLI's
//! half of it: the options the command line resolves ([`options`]), the localized rendering of
//! the [`UnsignedMigrationReport`] it returns ([`render`], [`describe_skip`]), and the notice
//! every workspace-opening verb prints while sidecars are still waiting ([`unmigrated_notice`]),
//! so the count is visible before the user runs the migration.
//!
//! The fallback album is the workspace's default album — the one `open_workspace` resolves at
//! open exactly as every other verb does — so a legacy record with no usable `album_id` lands
//! where an import would have put it.

use capsule_core::lifecycle::{
    LifecycleError, MigrationSkip, UnmigratedSidecar, UnsignedMigrationOptions,
    UnsignedMigrationReport, Workspace,
};
use capsule_i18n::Bundle;
use colored::Colorize as _;

use crate::i18n::{Value, keys};

/// The retention window, in days, stamped on the `delete` record of a legacy asset whose record
/// says it was deleted. The same default trash retention every other soft delete in this CLI
/// uses.
pub const DEFAULT_RETAIN_DAYS: i64 = crate::cull::DEFAULT_RETAIN_DAYS;

/// The options one `capsule library migrate` run passes to the core verb: the workspace's
/// default album as the fallback, and the requested retention.
#[must_use]
pub fn options(ws: &Workspace, retain_days: i64) -> UnsignedMigrationOptions {
    UnsignedMigrationOptions {
        fallback_album: ws.default_album_id(),
        trash_retain_days: retain_days,
    }
}

/// Run the migration over an opened workspace, logging what it did.
pub fn run(
    ws: &mut Workspace,
    retain_days: i64,
) -> Result<UnsignedMigrationReport, LifecycleError> {
    let opts = options(ws, retain_days);
    let found = ws.unmigrated_sidecars().len();
    tracing::info!(
        found,
        fallback_album = %opts.fallback_album,
        retain_days,
        "library migrate: starting"
    );
    let report = ws.migrate_unsigned_sidecars(&opts)?;
    tracing::info!(
        migrated = report.migrated.len(),
        trashed = report.trashed.len(),
        stacks = report.stacks.len(),
        skipped = report.skipped.len(),
        remaining = ws.unmigrated_sidecars().len(),
        "library migrate: finished"
    );
    Ok(report)
}

/// The line a workspace-opening verb prints when `open` found sidecars no provenance chain
/// anchors; `None` for a library written entirely on the signed path.
#[must_use]
pub fn unmigrated_notice(bundle: &Bundle, unmigrated: &[UnmigratedSidecar]) -> Option<String> {
    (!unmigrated.is_empty()).then(|| {
        bundle.format(
            keys::LIBRARY_MIGRATE_UNMIGRATED_NOTICE,
            &[("count", Value::Int(unmigrated.len() as i64))],
        )
    })
}

/// Localize why the migration refused one sidecar.
#[must_use]
pub fn describe_skip(bundle: &Bundle, skip: &MigrationSkip) -> String {
    let id = |asset_id: &uuid::Uuid| asset_id.to_string();
    match skip {
        MigrationSkip::InvalidAssetId(name) => bundle.format(
            keys::LIBRARY_MIGRATE_SKIP_INVALID_ASSET_ID,
            &[("name", Value::Str(name))],
        ),
        MigrationSkip::UnknownShape => bundle.format(keys::LIBRARY_MIGRATE_SKIP_UNKNOWN_SHAPE, &[]),
        MigrationSkip::Undecodable(detail) => bundle.format(
            keys::LIBRARY_MIGRATE_SKIP_UNDECODABLE,
            &[("detail", Value::Str(detail))],
        ),
        MigrationSkip::IdCollision(asset_id) => bundle.format(
            keys::LIBRARY_MIGRATE_SKIP_ID_COLLISION,
            &[("asset_id", Value::Str(&id(asset_id)))],
        ),
        MigrationSkip::OriginalMissing(asset_id) => bundle.format(
            keys::LIBRARY_MIGRATE_SKIP_ORIGINAL_MISSING,
            &[("asset_id", Value::Str(&id(asset_id)))],
        ),
        MigrationSkip::HashMismatch {
            asset_id,
            recorded,
            actual,
        } => bundle.format(
            keys::LIBRARY_MIGRATE_SKIP_HASH_MISMATCH,
            &[
                ("asset_id", Value::Str(&id(asset_id))),
                ("recorded", Value::Str(recorded)),
                ("actual", Value::Str(actual)),
            ],
        ),
        MigrationSkip::QuarantineConflict(asset_id) => bundle.format(
            keys::LIBRARY_MIGRATE_SKIP_QUARANTINE_CONFLICT,
            &[("asset_id", Value::Str(&id(asset_id)))],
        ),
        MigrationSkip::OutsideMonthBucket { asset_id, dir } => bundle.format(
            keys::LIBRARY_MIGRATE_SKIP_OUTSIDE_MONTH_BUCKET,
            &[
                ("asset_id", Value::Str(&id(asset_id))),
                ("dir", Value::Str(&dir.to_string_lossy())),
            ],
        ),
        MigrationSkip::UnusualExtension { asset_id, ext } => bundle.format(
            keys::LIBRARY_MIGRATE_SKIP_UNUSUAL_EXTENSION,
            &[
                ("asset_id", Value::Str(&id(asset_id))),
                ("ext", Value::Str(ext)),
            ],
        ),
        MigrationSkip::AlbumReadOnly { asset_id, album_id } => bundle.format(
            keys::LIBRARY_MIGRATE_SKIP_ALBUM_READ_ONLY,
            &[
                ("asset_id", Value::Str(&id(asset_id))),
                ("album_id", Value::Str(&album_id.to_string())),
            ],
        ),
        MigrationSkip::Stranded(asset_id) => bundle.format(
            keys::LIBRARY_MIGRATE_SKIP_STRANDED,
            &[("asset_id", Value::Str(&id(asset_id)))],
        ),
    }
}

/// Render one run as the lines `capsule library migrate` prints: a row per migrated asset,
/// per asset moved to trash, per derived stack, and per refused sidecar with its reason, then
/// the summary. `remaining` is how many unanchored sidecars the closing rescan still found.
#[must_use]
pub fn render(bundle: &Bundle, report: &UnsignedMigrationReport, remaining: usize) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };

    for asset_id in &report.migrated {
        line(
            bundle
                .format(
                    keys::LIBRARY_MIGRATE_MIGRATED_ROW,
                    &[("asset_id", Value::Str(&asset_id.to_string()))],
                )
                .green()
                .to_string(),
        );
    }
    for asset_id in &report.trashed {
        line(bundle.format(
            keys::LIBRARY_MIGRATE_TRASHED_ROW,
            &[("asset_id", Value::Str(&asset_id.to_string()))],
        ));
    }
    for stack_id in &report.stacks {
        line(bundle.format(
            keys::LIBRARY_MIGRATE_STACK_ROW,
            &[("stack_id", Value::Str(&stack_id.to_string()))],
        ));
    }
    for (path, skip) in &report.skipped {
        let reason = describe_skip(bundle, skip);
        line(
            bundle
                .format(
                    keys::LIBRARY_MIGRATE_SKIPPED_ROW,
                    &[
                        ("path", Value::Str(&path.to_string_lossy())),
                        ("reason", Value::Str(&reason)),
                    ],
                )
                .yellow()
                .to_string(),
        );
    }

    let nothing = report.migrated.is_empty()
        && report.trashed.is_empty()
        && report.skipped.is_empty()
        && remaining == 0;
    if nothing {
        line(
            bundle
                .format(keys::LIBRARY_MIGRATE_NOTHING, &[])
                .green()
                .to_string(),
        );
    } else {
        line(bundle.format(
            keys::LIBRARY_MIGRATE_SUMMARY,
            &[
                ("migrated", Value::Int(report.migrated.len() as i64)),
                ("trashed", Value::Int(report.trashed.len() as i64)),
                ("stacks", Value::Int(report.stacks.len() as i64)),
                ("skipped", Value::Int(report.skipped.len() as i64)),
                ("remaining", Value::Int(remaining as i64)),
            ],
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use capsule_core::lifecycle::UnmigratedShape;
    use uuid::Uuid;

    use super::*;

    fn en() -> Bundle {
        Bundle::for_locale("en")
    }

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
    fn every_skip_reason_renders_a_catalog_message_carrying_its_facts() {
        let bundle = en();
        let mut seen = std::collections::BTreeSet::new();
        for skip in every_skip() {
            let text = describe_skip(&bundle, &skip);
            assert!(!text.is_empty(), "{skip:?}");
            assert!(!text.contains("cli.library"), "a raw key leaked: {text}");
            assert!(!text.contains('{'), "an unfilled placeholder: {text}");
            assert!(
                seen.insert(text.clone()),
                "two reasons render alike: {text}"
            );
        }
        let a = Uuid::from_u128(0xA1).to_string();
        let mismatch = describe_skip(
            &bundle,
            &MigrationSkip::HashMismatch {
                asset_id: Uuid::from_u128(0xA1),
                recorded: "aa".into(),
                actual: "bb".into(),
            },
        );
        assert!(mismatch.contains(&a) && mismatch.contains("aa") && mismatch.contains("bb"));
        let ext = describe_skip(
            &bundle,
            &MigrationSkip::UnusualExtension {
                asset_id: Uuid::from_u128(0xA1),
                ext: "JPG".into(),
            },
        );
        assert!(ext.contains("JPG"), "{ext}");
    }

    #[test]
    fn the_notice_is_silent_for_a_signed_library_and_counts_otherwise() {
        let bundle = en();
        assert_eq!(unmigrated_notice(&bundle, &[]), None);
        let one = UnmigratedSidecar {
            path: PathBuf::from("media/1970/1970-01/x.cbor"),
            asset_id: None,
            shape: UnmigratedShape::LegacyUnsigned,
        };
        let text = unmigrated_notice(&bundle, &[one.clone(), one]).expect("a notice");
        assert!(text.contains('2'), "{text}");
        assert!(text.contains("capsule library migrate"), "{text}");
    }

    #[test]
    fn an_empty_run_with_nothing_remaining_says_there_was_nothing_to_do() {
        let page = render(&en(), &UnsignedMigrationReport::default(), 0);
        assert_eq!(page.lines().count(), 1, "{page}");
        assert!(
            page.contains(&en().format(keys::LIBRARY_MIGRATE_NOTHING, &[])),
            "{page}"
        );
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
        let page = render(&en(), &report, 1);
        let lines: Vec<&str> = page.lines().collect();
        assert_eq!(lines.len(), 6, "{page}");
        assert!(lines[0].contains(&migrated.to_string()), "{page}");
        assert!(lines[1].contains(&trashed.to_string()), "{page}");
        assert!(lines[2].contains(&trashed.to_string()), "{page}");
        assert!(lines[3].contains(&stack.to_string()), "{page}");
        assert!(
            lines[4].contains("bad.cbor")
                && lines[4].contains(&describe_skip(
                    &en(),
                    &MigrationSkip::InvalidAssetId("bad".into())
                )),
            "{page}"
        );
        assert_eq!(
            lines[5],
            en().format(
                keys::LIBRARY_MIGRATE_SUMMARY,
                &[
                    ("migrated", Value::Int(2)),
                    ("trashed", Value::Int(1)),
                    ("stacks", Value::Int(1)),
                    ("skipped", Value::Int(1)),
                    ("remaining", Value::Int(1)),
                ],
            )
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
        let page = render(&en(), &UnsignedMigrationReport::default(), 3);
        assert!(
            !page.contains(&en().format(keys::LIBRARY_MIGRATE_NOTHING, &[])),
            "{page}"
        );
        assert!(page.contains('3'), "{page}");
    }
}
