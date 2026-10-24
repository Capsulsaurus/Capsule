use super::*;

fn core_violations(source: &str) -> Vec<String> {
    stateless_core_violations("capsule-core/src/x.rs", source)
}

#[test]
fn a_core_file_without_state_is_clean() {
    let source = "use std::io::{self, Read};\nuse std::collections::BTreeMap;\n\
                      const COMMIT: Option<&str> = option_env!(\"CAPSULE_COMMIT\");\n\
                      fn f() -> &'static str { env!(\"CARGO_PKG_VERSION\") }\n";
    assert_eq!(core_violations(source), Vec::<String>::new());
}

#[test]
fn a_written_out_filesystem_or_environment_path_is_reported_with_its_line() {
    let found = core_violations("fn a() {}\nfn b() { std::fs::read(\"x\").ok(); }\n");
    assert_eq!(found.len(), 1);
    assert!(
        found[0].starts_with("capsule-core/src/x.rs:2:"),
        "{found:?}"
    );
    assert!(found[0].contains("`std::fs`"));

    let found = core_violations("let home = ::std::env::var(\"HOME\");\n");
    assert_eq!(found.len(), 1);
    assert!(found[0].contains("`std::env`"));
}

#[test]
fn a_path_filesystem_method_is_reported_but_a_keyed_lookup_is_not() {
    let found = core_violations("fn f(p: &Path) -> bool { p.is_file() || p.exists() }\n");
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found[0].starts_with("capsule-core/src/x.rs:1:"));
    assert_eq!(
        core_violations("fn f(s: &Store) -> bool { s.exists(key) }\n"),
        Vec::<String>::new()
    );
}

#[test]
fn a_database_crate_is_reported() {
    assert_eq!(
        core_violations("use rusqlite::Connection;\nuse sea_orm::Database;\n").len(),
        2
    );
}

#[test]
fn a_grouped_import_of_fs_or_env_is_reported_across_lines() {
    let found = core_violations("use std::{\n    collections::BTreeMap,\n    env, fs,\n};\n");
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found
            .iter()
            .all(|line| line.starts_with("capsule-core/src/x.rs:1:"))
    );
    assert!(found.iter().any(|line| line.contains("std::{fs}")));
    assert!(found.iter().any(|line| line.contains("std::{env}")));
}

#[test]
fn a_nested_group_member_named_like_fs_is_not_a_reach() {
    assert_eq!(
        core_violations("use std::{io::{fs_like, Read}, path::Path};\n"),
        Vec::<String>::new()
    );
}

#[test]
fn comments_and_longer_identifiers_are_not_reaches() {
    let source = "//! This module must stay `std::fs`-free.\n\
                      /// Persisting it is `rusqlite`'s business.\n\
                      fn f(my_std::fs: u8) {}\nconst A: &str = \"std::fsck\";\n";
    assert_eq!(core_violations(source), Vec::<String>::new());
}

#[test]
fn a_clock_read_or_identifier_draw_is_reported_with_its_line() {
    let source = "fn a() {}\nlet id = Uuid::now_v7();\nlet r = uuid::Uuid::new_v4();\n\
                  let t = std::time::SystemTime::now();\nlet z = jiff::Timestamp::now();\n\
                  let i = Instant::now();\n";
    let found = core_violations(source);
    assert_eq!(found.len(), 5, "{found:?}");
    for (line, path) in [
        (2, "Uuid::now_v7"),
        (3, "Uuid::new_v4"),
        (4, "SystemTime::now"),
        (5, "Timestamp::now"),
        (6, "Instant::now"),
    ] {
        assert!(
            found
                .iter()
                .any(|v| v.starts_with(&format!("capsule-core/src/x.rs:{line}:"))
                    && v.contains(&format!("`{path}`"))),
            "{path} on line {line}: {found:?}"
        );
    }
}

#[test]
fn an_injected_source_comment_or_longer_name_is_not_an_ambient_read() {
    let source = "// falls back to `Timestamp::now()` no more\n\
                  fn f(ids: IdSource<'_>) -> Uuid { ids.uuid_v7() }\n\
                  fn g(c: &dyn Clock) -> Timestamp { c.now() }\nlet x = Timestamp::nowhere;\n";
    assert_eq!(core_violations(source), Vec::<String>::new());
}

#[test]
fn test_code_may_read_the_clock_but_not_the_filesystem() {
    let source = "let id = Uuid::now_v7();\nlet t = Timestamp::now();\n";
    for path in [
        "capsule-core/src/federation/tests.rs",
        "capsule-core/src/crypto/keys/keystore/tests.rs",
        "capsule-core/src/tests/fixtures.rs",
    ] {
        assert_eq!(
            stateless_core_violations(path, source),
            Vec::<String>::new(),
            "{path}"
        );
    }
    assert_eq!(
        stateless_core_violations("capsule-core/src/x/tests.rs", "std::fs::read(\"x\");\n").len(),
        1
    );
    for path in [
        "capsule-core/src/tests_util.rs",
        "capsule-core/src/contests.rs",
        "capsule-core/src/tests.rs/x.rs",
    ] {
        assert_eq!(stateless_core_violations(path, source).len(), 2, "{path}");
    }
}

#[test]
fn architecture_text_excludes_lockfiles() {
    assert!(is_architecture_text(Path::new("docs/design.md")));
    assert!(is_architecture_text(Path::new("src/lib.rs")));
    assert!(!is_architecture_text(Path::new("Cargo.lock")));
    assert!(!is_architecture_text(Path::new("image.png")));
}

/// The list holds what is retired or unapproved, and nothing that has since been approved.
///
/// `chromahash`'s **absence** is asserted rather than left implicit. It was on this list as the
/// enforcement of AGENTS.md's "after its v1 release" gate; 0.7.1 shipped, the gate was amended
/// to that version, and Capsule adopts it. Re-adding it would silently un-approve a decision
/// the design docs record, and the failure would surface as a confusing dependency error in
/// whichever crate got there first rather than as "someone changed the policy".
#[test]
fn retired_dependency_list_holds_the_retired_but_not_the_approved() {
    assert!(
        !RETIRED_DEPENDENCIES.contains(&"chromahash"),
        "chromahash is approved as of 0.7.1 — see design/thumbnails.md"
    );

    // Still genuinely retired. `thumbhash` is what chromahash replaces and stays listed until
    // `capsule-core`'s `media` feature retires with it.
    assert!(RETIRED_DEPENDENCIES.contains(&"thumbhash"));
    assert!(RETIRED_DEPENDENCIES.contains(&"object_store"));
    assert!(RETIRED_DEPENDENCIES.contains(&"tonic"));
}

#[test]
fn a_retired_crate_reached_through_another_is_reported_with_its_path() {
    let lock = r#"
version = 4

[[package]]
name = "capsule-server"
version = "0.1.0"
dependencies = ["kynos", "direct-only", "serde 1.0.0"]

[[package]]
name = "kynos"
version = "0.2.0"
dependencies = ["web-glue", "tzf-rs"]

[[package]]
name = "tzf-rs"
version = "1.0.0"
dependencies = ["prost"]

[[package]]
name = "prost"
version = "0.13.0"

[[package]]
name = "web-glue"
version = "0.3.0"
dependencies = ["salvo", "serde 2.0.0 (registry+https://github.com/rust-lang/crates.io-index)"]

[[package]]
name = "direct-only"
version = "0.1.0"

[[package]]
name = "salvo"
version = "0.70.0"

[[package]]
name = "serde"
version = "1.0.0"

[[package]]
name = "serde"
version = "2.0.0"

[[package]]
name = "capsule-cli"
version = "0.1.0"
dependencies = ["tonic"]

[[package]]
name = "tonic"
version = "0.12.0"
"#;
    let members = BTreeSet::from(["capsule-server".to_owned(), "capsule-cli".to_owned()]);
    let found = transitive_retired_paths(lock, &members).expect("the lock parses");
    assert_eq!(
        found,
        [
            "active package `capsule-server` depends on retired or not-yet-approved \
                 `salvo` transitively (capsule-server -> kynos -> web-glue -> salvo); remove \
                 the path, or add `web-glue` -> `salvo` to SANCTIONED_TRANSITIVE_EDGES with a \
                 reason"
        ],
        "a direct dependency (`capsule-cli` -> `tonic`) is check_dependencies's to report, \
             and `tzf-rs` -> `prost` is sanctioned"
    );
}

#[test]
fn a_sanctioned_edge_does_not_hide_another_edge_to_the_same_crate() {
    let lock = r#"
[[package]]
name = "capsule-core"
version = "0.1.0"
dependencies = ["tzf-rs", "rpc-shim"]

[[package]]
name = "tzf-rs"
version = "1.0.0"
dependencies = ["prost"]

[[package]]
name = "rpc-shim"
version = "0.1.0"
dependencies = ["prost"]

[[package]]
name = "prost"
version = "0.13.0"
"#;
    let members = BTreeSet::from(["capsule-core".to_owned()]);
    let found = transitive_retired_paths(lock, &members).expect("the lock parses");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].contains("capsule-core -> rpc-shim -> prost"),
        "{found:?}"
    );
}

#[test]
fn a_path_through_another_member_is_reported_once_by_that_member() {
    let lock = r#"
[[package]]
name = "capsule-sdk"
version = "0.1.0"
dependencies = ["capsule-core"]

[[package]]
name = "capsule-core"
version = "0.1.0"
dependencies = ["prost", "rpc-shim"]

[[package]]
name = "rpc-shim"
version = "0.1.0"
dependencies = ["prost"]

[[package]]
name = "prost"
version = "0.13.0"
"#;
    let members = BTreeSet::from(["capsule-sdk".to_owned(), "capsule-core".to_owned()]);
    let found = transitive_retired_paths(lock, &members).expect("the lock parses");
    assert_eq!(
        found,
        [
            "active package `capsule-core` depends on retired or not-yet-approved `prost` \
                 transitively (capsule-core -> rpc-shim -> prost); remove the path, or add \
                 `rpc-shim` -> `prost` to SANCTIONED_TRANSITIVE_EDGES with a reason"
        ],
        "`capsule-core` -> `prost` is check_dependencies's, and nothing below \
             `capsule-core` is reported again from `capsule-sdk`"
    );
}

#[test]
fn every_sanctioned_edge_ends_at_a_retired_crate_and_states_its_reason() {
    for (from, to, reason) in SANCTIONED_TRANSITIVE_EDGES {
        assert!(
            RETIRED_DEPENDENCIES.contains(to),
            "`{from}` -> `{to}` sanctions an edge into a crate that is not retired"
        );
        assert!(
            !reason.trim().is_empty(),
            "`{from}` -> `{to}` has no reason"
        );
    }
}

#[test]
fn an_ambiguous_lock_entry_is_an_error_rather_than_a_guess() {
    let lock = r#"
[[package]]
name = "capsule-server"
version = "0.1.0"
dependencies = ["serde"]

[[package]]
name = "serde"
version = "1.0.0"

[[package]]
name = "serde"
version = "2.0.0"
"#;
    let members = BTreeSet::from(["capsule-server".to_owned()]);
    assert!(transitive_retired_paths(lock, &members).is_err());
}

#[test]
fn a_dependency_is_never_both_retired_and_planned() {
    for (name, _) in PLANNED_WORKSPACE_DEPENDENCIES {
        assert!(
            !RETIRED_DEPENDENCIES.contains(name),
            "`{name}` cannot be both retired and sanctioned pending work"
        );
    }
}

#[test]
fn every_planned_workspace_dependency_states_its_reason() {
    for (name, reason) in PLANNED_WORKSPACE_DEPENDENCIES {
        assert!(
            !reason.trim().is_empty(),
            "`{name}` is exempted with no reason, which is the exemption this list exists to prevent"
        );
    }
}

#[test]
fn a_failed_cargo_tree_is_never_read_as_chrono_absent() {
    // The fail-open this guard used to have. `cargo tree` exits non-zero for a *failure* as
    // well as for "that crate is not in the lock", and treating every non-zero exit as the
    // second one means the guard reports clean exactly when it knows least.
    let failed = classify_chrono_tree(
        false,
        "",
        "error: failed to parse manifest at `/x/Cargo.toml`\n",
    );
    assert!(
        failed.is_err(),
        "a broken invocation must not decide anything, got {failed:?}"
    );

    // The one non-zero exit that *is* an answer: chrono has left the lock file entirely.
    assert_eq!(
        classify_chrono_tree(
            false,
            "",
            "error: package ID specification `chrono` did not match any packages",
        ),
        Ok(false)
    );
}

#[test]
fn a_successful_cargo_tree_is_read_from_its_output() {
    assert_eq!(
        classify_chrono_tree(true, "", "warning: nothing to print."),
        Ok(false)
    );
    assert_eq!(
        classify_chrono_tree(true, "chrono v0.4.45\n└── capsule-cli-entity v0.1.0\n", ""),
        Ok(true)
    );
}

#[test]
fn the_valkey_adapters_are_the_only_planned_entries() {
    // A guard on scope rather than on content: this list is the one place an
    // unused pin can hide, so growing it should be a deliberate edit here.
    //
    // `testcontainers` and `testcontainers-modules` left the list with #402. They were
    // exempted because "no test starts a container yet", and the Postgres conformance
    // suites are tests that start one — so the exemption stopped describing a decision.
    // Shrinking this list is the shape a planned pin is meant to leave in: it graduates
    // into a member's manifest, where `check_dependencies` can see it.
    let names: BTreeSet<&str> = PLANNED_WORKSPACE_DEPENDENCIES
        .iter()
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(names, BTreeSet::from(["bb8", "bb8-redis", "redis"]));
}
