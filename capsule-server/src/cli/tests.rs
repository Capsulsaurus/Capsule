use clap::{CommandFactory as _, Parser as _};

use super::{
    Cli, CollectionReport, Command, Mode, PurgeReport, QuotaAction, RebuildReport, ScrubReport,
    mode, render_collection, render_purge, render_rebuild, render_scrub,
};

#[test]
fn the_command_line_is_well_formed() {
    // clap's own consistency check: a duplicate long flag, a subcommand with two positional
    // arguments in the wrong order, or an argument whose value name collides is a panic
    // here rather than a report from the first operator to run `--help`.
    Cli::command().debug_assert();
}

#[test]
fn gen_openapi_defaults_to_the_committed_document() {
    let cli = Cli::parse_from(["capsule-server", "gen-openapi"]);
    let Command::GenOpenapi { output, check } = cli.command else {
        panic!("that is the subcommand that was parsed")
    };
    assert_eq!(output, std::path::Path::new("capsule-server/openapi.json"));
    assert!(!check, "writing is the default; checking is opt-in");
}

#[test]
fn serve_carries_its_flags_into_the_overrides_the_loader_reads() {
    // The command line's half of the precedence table. A flag that parsed but never reached
    // `Config::load` would be a flag that silently does nothing.
    let cli = Cli::parse_from([
        "capsule-server",
        "serve",
        "--memory",
        "--listen",
        "127.0.0.1:6000",
        "--blob-root",
        "/var/lib/capsule/blobs",
    ]);
    let overrides = cli.command.overrides(None);
    assert!(overrides.memory);
    assert_eq!(
        overrides.listen,
        Some("127.0.0.1:6000".parse().expect("a literal address parses"))
    );
    assert_eq!(
        overrides.blob_root.as_deref(),
        Some(std::path::Path::new("/var/lib/capsule/blobs"))
    );
}

#[test]
fn serving_demands_a_key_and_describing_the_router_demands_nothing() {
    assert_eq!(
        Cli::parse_from(["capsule-server", "serve"])
            .command
            .demands(),
        crate::config::Demands::Serve
    );
    assert_eq!(
        Cli::parse_from(["capsule-server", "gen-openapi"])
            .command
            .demands(),
        crate::config::Demands::Nothing
    );
}

#[test]
fn the_quota_acts_are_maintenance_commands_that_carry_their_backend() {
    for act in ["suspend", "unsuspend"] {
        let cli = Cli::parse_from([
            "capsule-server",
            "quota",
            act,
            "user-1",
            "--memory",
            "--blob-root",
            "/var/lib/capsule/blobs",
        ]);
        assert_eq!(
            cli.command.demands(),
            crate::config::Demands::Maintenance,
            "{act}: an operator's quota act needs no key material"
        );
        let overrides = cli.command.overrides(None);
        assert!(overrides.memory, "{act}");
        assert_eq!(
            overrides.blob_root.as_deref(),
            Some(std::path::Path::new("/var/lib/capsule/blobs")),
            "{act}"
        );
        let Command::Quota { action } = cli.command else {
            panic!("{act}: that is the subcommand that was parsed")
        };
        let (QuotaAction::Suspend { user, .. } | QuotaAction::Unsuspend { user, .. }) = action;
        assert_eq!(user, "user-1", "{act}");
    }
}

#[test]
fn a_config_path_is_accepted_by_the_parser_so_it_can_be_refused_by_the_loader() {
    let cli = Cli::parse_from([
        "capsule-server",
        "--config",
        "/etc/capsule.toml",
        "gen-openapi",
    ]);
    assert_eq!(
        cli.config.as_deref(),
        Some(std::path::Path::new("/etc/capsule.toml"))
    );
}

/// A well-formed content address, distinguished by `seed`.
fn address(seed: u8) -> crate::blob::ContentAddress {
    let hex: String = std::iter::repeat_n(format!("{seed:02x}"), 32).collect();
    crate::blob::ContentAddress::parse(&hex).expect("an address")
}

#[test]
fn a_quiet_collection_pass_is_one_short_line() {
    // Six zeroes would be six lines an operator learns to skip, and the whole point of the
    // report is that they read it.
    let rendered = render_collection(&CollectionReport::default(), Mode::DryRun).to_string();
    assert!(rendered.contains("dry run"), "{rendered}");
    assert!(rendered.contains("nothing to do"), "{rendered}");
}

#[test]
fn a_collection_pass_names_the_blobs_rather_than_counting_them() {
    // `CollectionReport`'s own docs: "a count tells an operator that something happened
    // without telling them what to look at."
    let report = CollectionReport {
        marked: vec![address(0xAA), address(0xBB)],
        swept: vec![address(0xCC)],
        credited: vec![(crate::store::UserId::new("a-user"), 4096)],
        ..CollectionReport::default()
    };
    let rendered = render_collection(&report, Mode::Apply).to_string();
    assert!(rendered.contains("applied"), "{rendered}");
    assert!(rendered.contains("marked (2)"), "{rendered}");
    assert!(rendered.contains(&address(0xAA).to_string()), "{rendered}");
    assert!(rendered.contains(&address(0xBB).to_string()), "{rendered}");
    assert!(rendered.contains("swept (1)"), "{rendered}");
    assert!(
        rendered.contains("credited (1 accounts, 4096 bytes)"),
        "{rendered}"
    );
    // Classes with nothing in them are omitted rather than printed as zero.
    assert!(!rendered.contains("unmarked"), "{rendered}");
    assert!(!rendered.contains("nothing to do"), "{rendered}");
}

#[test]
fn a_purge_reports_what_is_still_waiting() {
    // "Why has this not gone yet" is exactly the question a dry run is run to answer.
    let report = PurgeReport {
        purged: vec![crate::store::AssetId::new("gone")],
        retained: vec![crate::store::AssetId::new("waiting")],
        skipped: vec![crate::store::AssetId::new("restored")],
        unrecorded: vec![crate::store::AssetId::new("unledgered")],
    };
    let rendered = render_purge(&report, Mode::DryRun).to_string();
    assert!(rendered.contains("purged (1)"), "{rendered}");
    assert!(rendered.contains("gone"), "{rendered}");
    assert!(rendered.contains("retained (1)"), "{rendered}");
    assert!(rendered.contains("waiting"), "{rendered}");
    assert!(rendered.contains("skipped (1)"), "{rendered}");
    assert!(rendered.contains("restored"), "{rendered}");
    assert!(rendered.contains("unrecorded, kept (1)"), "{rendered}");
    assert!(rendered.contains("unledgered"), "{rendered}");
}

#[test]
fn a_clean_scrub_says_the_two_sides_agree() {
    let rendered = render_scrub(&ScrubReport::default()).to_string();
    assert!(rendered.contains("0 findings"), "{rendered}");
    assert!(rendered.contains("agree"), "{rendered}");
}

#[test]
fn a_scrub_groups_findings_by_the_class_an_operator_alerts_on() {
    let report = ScrubReport {
        findings: vec![
            crate::scrub::Finding::Orphan {
                address: address(0xAA),
            },
            crate::scrub::Finding::Orphan {
                address: address(0xBB),
            },
            crate::scrub::Finding::Debris {
                path: "blobs/aa/not-a-blob".to_owned(),
            },
        ],
        bytes_hashed: 0,
        budget_exhausted: false,
        ledger_links_only: false,
    };
    let rendered = render_scrub(&report).to_string();
    assert!(rendered.contains("3 findings"), "{rendered}");
    assert!(rendered.contains("orphan (2)"), "{rendered}");
    assert!(rendered.contains("debris (1)"), "{rendered}");
    assert!(rendered.contains("not-a-blob"), "{rendered}");
    assert!(!rendered.contains("agree"), "{rendered}");
}

#[test]
fn a_truncated_deep_pass_is_not_a_clean_one() {
    // A clean report from a pass that stopped early is the one answer a scrub must never
    // give, so the rendering says so out loud as well.
    let report = ScrubReport {
        findings: Vec::new(),
        bytes_hashed: 1024,
        budget_exhausted: true,
        ledger_links_only: false,
    };
    let rendered = render_scrub(&report).to_string();
    assert!(rendered.contains("budget exhausted"), "{rendered}");
    assert!(!rendered.contains("agree"), "{rendered}");
}

#[test]
fn a_scrub_that_checked_ledger_links_only_says_so() {
    let report = ScrubReport {
        ledger_links_only: true,
        ..ScrubReport::default()
    };
    let rendered = render_scrub(&report).to_string();
    assert!(rendered.contains("signatures not"), "{rendered}");
    assert!(rendered.contains("agree"), "{rendered}");
}

#[test]
fn a_rebuild_is_a_dry_run_unless_applied() {
    let dry = Cli::parse_from(["capsule-server", "rebuild"]).command;
    assert!(matches!(dry, Command::Rebuild { apply: false, .. }));
    let applied = Cli::parse_from(["capsule-server", "rebuild", "--apply"]).command;
    assert!(matches!(
        applied,
        Command::Rebuild {
            apply: true,
            accept_rewrites: false,
            ..
        }
    ));
    let accepting =
        Cli::parse_from(["capsule-server", "rebuild", "--apply", "--accept-rewrites"]).command;
    assert!(matches!(
        accepting,
        Command::Rebuild {
            apply: true,
            accept_rewrites: true,
            ..
        }
    ));
    // Accepting a rewrite is a write, so it is refused without `--apply`.
    assert!(Cli::try_parse_from(["capsule-server", "rebuild", "--accept-rewrites"]).is_err());
}

#[test]
fn a_rebuild_report_names_what_it_restored_and_what_it_found() {
    use crate::ledger::ChainFault;
    use crate::ledger::directory::{DirectoryFault, Refusal};
    use crate::ledger::rebuild::{OwnerReport, RowDifference};
    use crate::store::{AssetId, OwnerId};

    let owner = OwnerReport {
        owner: OwnerId::new("owner-1"),
        verified: 4,
        chain_fault: Some(ChainFault::BadSignature { seq: 5 }),
        restored: vec![AssetId::new("asset-restored")],
        agreed: vec![AssetId::new("asset-agreed")],
        differs: vec![RowDifference {
            asset: AssetId::new("asset-differs"),
            facts: vec!["album_id"],
        }],
        unprovable: vec![AssetId::new("asset-unprovable")],
        incomplete: Vec::new(),
        purged: Vec::new(),
        dangling: vec![(AssetId::new("asset-dangling"), address(0xCC))],
        unusable_manifests: Vec::new(),
        foreign_manifests: Vec::new(),
        unverified_manifests: vec![(
            AssetId::new("asset-unverified"),
            address(0xEE),
            Refusal::NoDirectory,
        )],
        unusable_directories: vec![(3, DirectoryFault::Missing)],
        epoch_bump_needed: true,
        epoch: None,
        reanchored: None,
    };
    let report = RebuildReport {
        applied: false,
        links_only: true,
        owners: vec![owner],
        orphans: vec![address(0xDD)],
    };
    let rendered = render_rebuild(&report, Mode::DryRun).to_string();
    for expected in [
        "index rebuild (dry run",
        "signatures not",
        "owner owner-1 (4 entries verified, 1 rows agree)",
        "ledger broken",
        "would restore (1)",
        "asset-restored",
        "asset-differs: album_id",
        "unprovable sequence (1)",
        "dangling (1)",
        "device signature not vouched for, not restored (1)",
        "asset-unverified",
        "no device directory in force",
        "device directory with no usable copy (1)",
        "ledger entry 3 (the directory copy is missing)",
        "needs a new sync epoch",
        "orphans, left to gc (1)",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected:?} in {rendered}"
        );
    }
    assert!(
        !rendered.contains("asset-agreed"),
        "agreeing rows are counted, not listed"
    );
}

/// A caught rewrite names the operator's way out; a re-anchored owner says it was.
#[test]
fn a_rebuild_report_names_the_way_out_of_a_caught_rewrite() {
    use crate::ledger::ChainFault;
    use crate::ledger::rebuild::OwnerReport;
    use crate::store::OwnerId;

    let owner = |name: &str, fault, reanchored| OwnerReport {
        owner: OwnerId::new(name),
        verified: 1,
        chain_fault: Some(fault),
        restored: Vec::new(),
        agreed: Vec::new(),
        differs: Vec::new(),
        unprovable: Vec::new(),
        incomplete: Vec::new(),
        purged: Vec::new(),
        dangling: Vec::new(),
        unusable_manifests: Vec::new(),
        foreign_manifests: Vec::new(),
        unverified_manifests: Vec::new(),
        unusable_directories: Vec::new(),
        epoch_bump_needed: true,
        epoch: Some(1),
        reanchored,
    };
    let report = RebuildReport {
        applied: true,
        links_only: false,
        owners: vec![
            owner("rewritten", ChainFault::AnchorMismatch { seq: 2 }, None),
            owner(
                "cut",
                ChainFault::BehindAnchor {
                    found: 1,
                    anchored: 3,
                },
                Some(1),
            ),
        ],
        orphans: Vec::new(),
    };
    let rendered = render_rebuild(&report, Mode::Apply).to_string();
    assert!(
        rendered.contains("rebuild --apply --accept-rewrites"),
        "{rendered}"
    );
    assert!(
        rendered.contains("ledger re-anchored at entry 1"),
        "{rendered}"
    );
    assert_eq!(
        rendered.matches("--accept-rewrites").count(),
        1,
        "only the rewritten owner needs the way out: {rendered}"
    );
}

#[test]
fn dry_run_is_the_default_for_the_two_subcommands_that_write() {
    // The first thing an operator does with a collector is find out what it thinks.
    let gc = Cli::parse_from(["capsule-server", "gc"]).command;
    assert!(matches!(gc, Command::Gc { apply: false, .. }));
    let purge = Cli::parse_from(["capsule-server", "purge"]).command;
    assert!(matches!(purge, Command::Purge { apply: false, .. }));
    assert_eq!(mode(false), Mode::DryRun);
    assert_eq!(mode(true), Mode::Apply);
}

#[test]
fn a_structural_scrub_is_the_default_and_deep_carries_a_budget() {
    let shallow = Cli::parse_from(["capsule-server", "scrub"]).command;
    assert!(matches!(shallow, Command::Scrub { deep: false, .. }));
    let deep = Cli::parse_from(["capsule-server", "scrub", "--deep"]).command;
    let Command::Scrub { deep, budget, .. } = deep else {
        panic!("that is the subcommand that was parsed")
    };
    assert!(deep);
    assert_eq!(budget, super::DEFAULT_SCRUB_BUDGET);
}

#[test]
fn the_operator_commands_demand_a_blob_root_and_no_key_material() {
    for argv in [
        ["capsule-server", "gc"],
        ["capsule-server", "purge"],
        ["capsule-server", "scrub"],
    ] {
        assert_eq!(
            Cli::parse_from(argv).command.demands(),
            crate::config::Demands::Maintenance,
            "{argv:?}"
        );
    }
}
