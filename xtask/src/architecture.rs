//! Enforces the active Rust workspace and retired-boundary decisions.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use eyre::{ContextCompat, Result, WrapErr, bail};
use serde_json::Value;
use toml_edit::DocumentMut;

// Two different reasons live in one list, and the violation text says so: a dependency is here
// because it was **retired** (its stack moved to `legacy-review/`) or because it was
// **not yet approved** (it existed but the project had not accepted it). `chromahash` was the
// second kind, gated by AGENTS.md to "after its v1 release" — it left this list when 0.7.1 shipped
// and the gate was amended to that version, because a check that forbids an approved dependency
// stops describing a decision and starts blocking one. `thumbhash` stays: it is what `chromahash`
// replaces, and it is still a live optional dependency of `capsule-core`'s retiring `media`
// feature, so it remains a genuine violation until that stack retires.
const RETIRED_DEPENDENCIES: &[&str] = &[
    "async-graphql",
    "async-graphql-salvo",
    "capsule-media",
    "graphql-client",
    "object_store",
    "progenitor",
    "prost",
    "prost-types",
    "salvo",
    "thumbhash",
    "tonic",
    "tonic-prost",
    "tonic-web",
    "tus",
];

// `[workspace.dependencies]` entries no member consumes yet, each with the decision that
// sanctions it. Cargo does not lock an unused workspace dependency, so without this list the
// only options for a not-yet-wired pin are "delete it" or "leave the tree carrying a
// declaration nothing can verify" — which is how `testcontainers` came to be cited by nine
// design docs as proof of a smoke tier that has never run.
const PLANNED_WORKSPACE_DEPENDENCIES: &[(&str, &str)] = &[
    (
        "bb8",
        "Valkey adapter pooling for the two typed state ports (AGENTS.md)",
    ),
    (
        "bb8-redis",
        "Valkey adapter pooling for the two typed state ports (AGENTS.md)",
    ),
    (
        "redis",
        "the `redis-rs` Valkey adapters AGENTS.md requires for AuthStateStore/UploadSessionStore",
    ),
];

const RETIRED_COMPONENT_NAMES: &[&str] = &[
    "capsule-api-auth",
    "capsule-api-library",
    "capsule-api-media",
    "capsule-api-service",
    "capsule-api-sync",
    "capsule-api-upload",
    "capsule-media",
];

pub(crate) fn run(root: &Path) -> Result<()> {
    let mut violations = Vec::new();
    check_workspace_members(root, &mut violations)?;
    check_dependencies(root, &mut violations)?;
    check_workspace_dependencies(root, &mut violations)?;
    check_legacy_manifests(root, &mut violations)?;
    check_chrono_isolation(root, &mut violations)?;
    check_retired_references(root, &mut violations)?;

    if violations.is_empty() {
        println!("Rust architecture boundaries are intact");
        return Ok(());
    }

    violations.sort();
    bail!(
        "Rust architecture boundary violations:\n- {}",
        violations.join("\n- ")
    )
}

fn cargo_metadata(root: &Path) -> Result<Value> {
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args([
            "metadata",
            "--offline",
            "--format-version",
            "1",
            "--no-deps",
        ])
        .current_dir(root)
        .output()
        .context("running cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("parsing cargo metadata")
}

fn check_workspace_members(root: &Path, violations: &mut Vec<String>) -> Result<()> {
    let manifest = fs::read_to_string(root.join("Cargo.toml")).context("reading Cargo.toml")?;
    let document = manifest
        .parse::<DocumentMut>()
        .context("parsing Cargo.toml")?;
    let members = document["workspace"]["members"]
        .as_array()
        .context("[workspace].members must be an explicit array")?;

    let mut declared = BTreeSet::new();
    for member in members {
        let relative = member
            .as_str()
            .context("workspace member must be a string")?;
        if relative.chars().any(|character| "*?[".contains(character)) {
            violations.push(format!(
                "workspace member `{relative}` is a glob; list every active package explicitly"
            ));
            continue;
        }
        let path = root.join(relative).join("Cargo.toml");
        declared.insert(
            fs::canonicalize(&path)
                .with_context(|| format!("resolving declared member {}", path.display()))?,
        );
    }

    let metadata = cargo_metadata(root)?;
    let packages = metadata["packages"]
        .as_array()
        .context("cargo metadata packages missing")?;
    let manifests_by_id: BTreeMap<&str, &str> = packages
        .iter()
        .map(|package| {
            Ok((
                package["id"].as_str().context("package id missing")?,
                package["manifest_path"]
                    .as_str()
                    .context("package manifest_path missing")?,
            ))
        })
        .collect::<Result<_>>()?;
    let active = metadata["workspace_members"]
        .as_array()
        .context("cargo metadata workspace_members missing")?
        .iter()
        .map(|id| {
            let id = id.as_str().context("workspace member id missing")?;
            let path = manifests_by_id
                .get(id)
                .with_context(|| format!("workspace package `{id}` missing from metadata"))?;
            fs::canonicalize(path).with_context(|| format!("resolving active manifest {path}"))
        })
        .collect::<Result<BTreeSet<_>>>()?;

    for path in declared.difference(&active) {
        violations.push(format!(
            "declared workspace member is not active: {}",
            display_relative(root, path)
        ));
    }
    for path in active.difference(&declared) {
        violations.push(format!(
            "implicit workspace member is active: {}",
            display_relative(root, path)
        ));
    }
    Ok(())
}

fn check_dependencies(root: &Path, violations: &mut Vec<String>) -> Result<()> {
    let metadata = cargo_metadata(root)?;
    let member_ids: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .context("cargo metadata workspace_members missing")?
        .iter()
        .map(|id| id.as_str().context("workspace member id missing"))
        .collect::<Result<_>>()?;

    for package in metadata["packages"]
        .as_array()
        .context("cargo metadata packages missing")?
    {
        let id = package["id"].as_str().context("package id missing")?;
        if !member_ids.contains(id) {
            continue;
        }
        let package_name = package["name"].as_str().context("package name missing")?;
        for dependency in package["dependencies"]
            .as_array()
            .context("package dependencies missing")?
        {
            let name = dependency["name"]
                .as_str()
                .context("dependency name missing")?;
            if RETIRED_DEPENDENCIES.contains(&name) {
                violations.push(format!(
                    "active package `{package_name}` depends on retired or not-yet-approved `{name}`"
                ));
            }
        }
    }
    Ok(())
}

/// `check_dependencies` reads `cargo metadata`, which only ever reports *member* packages, so a
/// `[workspace.dependencies]` entry no member inherits is invisible to it — and, because cargo
/// does not lock one either, invisible to `Cargo.lock` too. That is how `salvo`, `tonic*` and
/// `prost*` stayed declared in the root manifest while `AGENTS.md` banned them by name.
///
/// Two rules, both over the root manifest: nothing retired may be declared, and nothing declared
/// may go unused unless [`PLANNED_WORKSPACE_DEPENDENCIES`] says why.
fn check_workspace_dependencies(root: &Path, violations: &mut Vec<String>) -> Result<()> {
    let manifest = root.join("Cargo.toml");
    let document: DocumentMut = fs::read_to_string(&manifest)
        .wrap_err_with(|| format!("reading {}", manifest.display()))?
        .parse()
        .wrap_err("parsing the root Cargo.toml")?;

    let Some(declared) = document
        .get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(toml_edit::Item::as_table)
    else {
        return Ok(());
    };

    let planned: BTreeMap<&str, &str> = PLANNED_WORKSPACE_DEPENDENCIES.iter().copied().collect();
    let consumed = member_dependency_names(root)?;

    for (name, _) in declared {
        if RETIRED_DEPENDENCIES.contains(&name) {
            violations.push(format!(
                "`[workspace.dependencies] {name}` is retired or not-yet-approved; remove the declaration"
            ));
            continue;
        }
        if !consumed.contains(name) && !planned.contains_key(name) {
            violations.push(format!(
                "`[workspace.dependencies] {name}` is declared but no workspace member uses it; \
                 remove it, or add it to PLANNED_WORKSPACE_DEPENDENCIES with a reason"
            ));
        }
    }

    Ok(())
}

/// Every dependency name any workspace member declares, in any dependency kind.
fn member_dependency_names(root: &Path) -> Result<BTreeSet<String>> {
    let metadata = cargo_metadata(root)?;
    let member_ids: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .context("cargo metadata workspace_members missing")?
        .iter()
        .map(|id| id.as_str().context("workspace member id missing"))
        .collect::<Result<_>>()?;

    let mut names = BTreeSet::new();
    for package in metadata["packages"]
        .as_array()
        .context("cargo metadata packages missing")?
    {
        let id = package["id"].as_str().context("package id missing")?;
        if !member_ids.contains(id) {
            continue;
        }
        for dependency in package["dependencies"]
            .as_array()
            .context("package dependencies missing")?
        {
            names.insert(
                dependency["name"]
                    .as_str()
                    .context("dependency name missing")?
                    .to_owned(),
            );
        }
    }
    Ok(names)
}

/// `capsule-server` must not be on the `chrono` path.
///
/// design/dependencies.md makes `cargo tree -i chrono -e no-dev` a review-blocking gate: chrono
/// exists in this workspace only as the sea-orm column type inside `capsule-cli/entity`, and the
/// server's own datetime crate is `jiff`. #402 puts sea-orm into `capsule-server` for the
/// Postgres adapters, which makes that gate worth *enforcing* rather than reading.
///
/// The check is deliberately **per package** rather than a grep of the workspace-wide tree.
/// `capsule-cli` inherits sea-orm with its default features, `with-chrono` among them, and
/// cargo unifies features across the packages a workspace build selects — so the workspace-wide
/// `cargo tree` output lists `capsule-server` under sea-orm and always will, whatever this crate
/// declares. What is actually decidable, and what the rule is about, is whether *this package's
/// own manifest* asks for chrono: `cargo tree -p capsule-server -i chrono -e no-dev` must print
/// nothing. That is what `default-features = false` on the server's sea-orm entry buys, and this
/// is what stops somebody restoring the defaults without noticing.
fn check_chrono_isolation(root: &Path, violations: &mut Vec<String>) -> Result<()> {
    const PACKAGES: &[&str] = &["capsule-server", "capsule-server-migration"];
    // The migration crate is exempt from the *conclusion* but not from the check: it is a
    // dev-dependency of the server precisely because `sea-orm-migration` drags sea-orm's
    // defaults in, so it is expected to be on the path and is listed here only so a reader sees
    // that the exemption is deliberate rather than an omission.
    const EXPECTED_ON_THE_PATH: &[&str] = &["capsule-server-migration"];

    for package in PACKAGES {
        let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args([
                "tree",
                "--offline",
                "-p",
                package,
                "-i",
                "chrono",
                "-e",
                "no-dev",
            ])
            .current_dir(root)
            .output()
            .with_context(|| format!("running cargo tree for {package}"))?;
        // A package that does not depend on chrono makes `cargo tree -i` exit non-zero with
        // "nothing to print" or "did not match any packages"; both are the passing shape.
        let reaches_chrono =
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("chrono v");
        let expected = EXPECTED_ON_THE_PATH.contains(package);
        if reaches_chrono && !expected {
            violations.push(format!(
                "`{package}` reaches `chrono` through its own manifest; design/dependencies.md \
                 permits chrono only as the sea-orm column type in `capsule-cli/entity`. Check \
                 that sea-orm is declared with `default-features = false`"
            ));
        }
        if !reaches_chrono && expected {
            violations.push(format!(
                "`{package}` no longer reaches `chrono`; remove it from EXPECTED_ON_THE_PATH so \
                 the exemption stops describing something that is not true"
            ));
        }
    }
    Ok(())
}

fn check_legacy_manifests(root: &Path, violations: &mut Vec<String>) -> Result<()> {
    let legacy = root.join("legacy-review");
    if legacy.exists() {
        walk_files(&legacy, &mut |path| {
            if path.file_name().is_some_and(|name| name == "Cargo.toml") {
                violations.push(format!(
                    "review-only source contains an active Cargo.toml: {}",
                    display_relative(root, path)
                ));
            }
            Ok(())
        })?;
    }
    Ok(())
}

fn check_retired_references(root: &Path, violations: &mut Vec<String>) -> Result<()> {
    walk_files(root, &mut |path| {
        if ignored_path(root, path) || !is_architecture_text(path) {
            return Ok(());
        }
        let contents = fs::read_to_string(path)
            .with_context(|| format!("reading {}", display_relative(root, path)))?;
        for retired in RETIRED_COMPONENT_NAMES {
            if contents.contains(retired) {
                violations.push(format!(
                    "{} still references retired component `{retired}`",
                    display_relative(root, path)
                ));
            }
        }
        Ok(())
    })
}

fn walk_files(directory: &Path, visit: &mut impl FnMut(&Path) -> Result<()>) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("reading directory {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            if is_ignored_directory(&path) {
                continue;
            }
            walk_files(&path, visit)?;
        } else {
            visit(&path)?;
        }
    }
    Ok(())
}

fn is_ignored_directory(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        // `.claude` holds agent worktrees (`.claude/worktrees/<name>`), each a full checkout of
        // this repository. Walking them counts every violation a second time per worktree — an
        // agent working in one saw 63 while the parent tree reported 115 for the same commit.
        // The worktree has its own root and checks itself; from here it is somebody else's tree.
        Some(
            ".claude"
                | ".git"
                | ".gradle"
                | "build"
                | "dist"
                | "legacy-review"
                | "node_modules"
                | "target"
        )
    )
}

fn ignored_path(root: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some(
                ".git" | ".gradle" | "build" | "dist" | "legacy-review" | "node_modules" | "target"
            )
        )
    }) || relative == Path::new("xtask/src/architecture.rs")
}

fn is_architecture_text(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("js" | "json" | "md" | "rs" | "toml" | "ts" | "tsx" | "yaml" | "yml")
    ) && !matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("Cargo.lock" | "bun.lock")
    )
}

fn display_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    /// to that version, and `S-B14` adopts it. Re-adding it would silently un-approve a decision
    /// the design docs record, and the failure would surface as a confusing dependency error in
    /// whichever crate got there first rather than as "someone changed the policy".
    #[test]
    fn retired_dependency_list_holds_the_retired_but_not_the_approved() {
        assert!(
            !RETIRED_DEPENDENCIES.contains(&"chromahash"),
            "chromahash is approved as of 0.7.1 — see design/thumbnails.md and slice S-B14"
        );

        // Still genuinely retired. `thumbhash` is what chromahash replaces and stays listed until
        // `capsule-core`'s `media` feature retires with it.
        assert!(RETIRED_DEPENDENCIES.contains(&"thumbhash"));
        assert!(RETIRED_DEPENDENCIES.contains(&"object_store"));
        assert!(RETIRED_DEPENDENCIES.contains(&"tonic"));
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
    fn the_valkey_adapters_and_the_smoke_tier_are_the_only_planned_entries() {
        // A guard on scope rather than on content: this list is the one place an
        // unused pin can hide, so growing it should be a deliberate edit here.
        let names: BTreeSet<&str> = PLANNED_WORKSPACE_DEPENDENCIES
            .iter()
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(
            names,
            BTreeSet::from([
                "bb8",
                "bb8-redis",
                "redis",
                "testcontainers",
                "testcontainers-modules",
            ])
        );
    }
}
