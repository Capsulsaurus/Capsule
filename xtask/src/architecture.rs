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
    "capsule-wire",
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

// Lock-graph edges into a retired crate that are not the decision the retirement records, each
// with its reason. `prost`/`tonic` are retired as **Capsule's public surface** (no gRPC; see
// AGENTS.md), and these crates use them as an internal encoding or a container-runtime client,
// which exposes no surface of Capsule's. An edge is sanctioned, not a crate: the same retired
// crate arriving through anything else is still a violation.
const SANCTIONED_TRANSITIVE_EDGES: &[(&str, &str, &str)] = &[
    (
        "tzf-rs",
        "prost",
        "the timezone finder's compiled boundary data is protobuf; no RPC surface",
    ),
    (
        "prost-build",
        "prost",
        "build-time code generation for tzf-rs's data format",
    ),
    (
        "prost-build",
        "prost-types",
        "build-time code generation for tzf-rs's data format",
    ),
    (
        "bollard",
        "tonic",
        "testcontainers' Docker client (dev-dependency); BuildKit over the local daemon socket",
    ),
    (
        "bollard-stubs",
        "prost",
        "testcontainers' Docker client (dev-dependency); Docker API message types",
    ),
    (
        "bollard-buildkit-proto",
        "tonic",
        "testcontainers' Docker client (dev-dependency); BuildKit message types",
    ),
    (
        "bollard-buildkit-proto",
        "prost",
        "testcontainers' Docker client (dev-dependency); BuildKit message types",
    ),
    (
        "bollard-buildkit-proto",
        "prost-types",
        "testcontainers' Docker client (dev-dependency); BuildKit message types",
    ),
    (
        "bollard-buildkit-proto",
        "tonic-prost",
        "testcontainers' Docker client (dev-dependency); BuildKit message types",
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
    check_transitive_dependencies(root, &mut violations)?;
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

/// A retired dependency reached **through** another crate is as much a violation as a direct one.
///
/// [`check_dependencies`] reads each member's own manifest, so `salvo` arriving as a dependency
/// of a dependency would pass it. This walks the resolved graph in `Cargo.lock` instead, which
/// cargo resolves with every feature of every member enabled, so an optional path counts too.
/// Only paths of two or more edges are reported here; a direct dependency is
/// `check_dependencies`'s to report, and reporting it twice would read as two violations. An
/// edge [`SANCTIONED_TRANSITIVE_EDGES`] lists is not reported.
fn check_transitive_dependencies(root: &Path, violations: &mut Vec<String>) -> Result<()> {
    let metadata = cargo_metadata(root)?;
    let member_ids: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .context("cargo metadata workspace_members missing")?
        .iter()
        .map(|id| id.as_str().context("workspace member id missing"))
        .collect::<Result<_>>()?;
    let mut members = BTreeSet::new();
    for package in metadata["packages"]
        .as_array()
        .context("cargo metadata packages missing")?
    {
        let id = package["id"].as_str().context("package id missing")?;
        if member_ids.contains(id) {
            members.insert(
                package["name"]
                    .as_str()
                    .context("package name missing")?
                    .to_owned(),
            );
        }
    }
    let lock_path = root.join("Cargo.lock");
    let lock = fs::read_to_string(&lock_path)
        .wrap_err_with(|| format!("reading {}", lock_path.display()))?;
    violations.extend(transitive_retired_paths(&lock, &members)?);
    Ok(())
}

/// Every unsanctioned edge into a [`RETIRED_DEPENDENCIES`] entry reachable, two or more edges
/// out, from a package in `members` of the lock file `lock` — one violation line each, carrying
/// a shortest path to it.
fn transitive_retired_paths(lock: &str, members: &BTreeSet<String>) -> Result<Vec<String>> {
    let document: DocumentMut = lock.parse().wrap_err("parsing Cargo.lock")?;
    let packages = document
        .get("package")
        .and_then(toml_edit::Item::as_array_of_tables)
        .context("Cargo.lock has no [[package]] tables")?;

    // Keyed by `name version`; a lock entry names a dependency by `name` alone when only one
    // version is locked, and by `name version` (plus a source) when several are.
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut versions_by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for package in packages {
        let name = package
            .get("name")
            .and_then(toml_edit::Item::as_str)
            .context("a Cargo.lock package has no name")?;
        let version = package
            .get("version")
            .and_then(toml_edit::Item::as_str)
            .context("a Cargo.lock package has no version")?;
        let dependencies = package
            .get("dependencies")
            .and_then(toml_edit::Item::as_array)
            .map(|array| {
                array
                    .iter()
                    .filter_map(|entry| entry.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let key = format!("{name} {version}");
        versions_by_name
            .entry(name.to_owned())
            .or_default()
            .push(key.clone());
        graph.insert(key, dependencies);
    }
    let resolve = |entry: &str| -> Result<String> {
        let mut parts = entry.split_whitespace();
        let name = parts.next().context("an empty Cargo.lock dependency")?;
        match parts.next() {
            Some(version) => Ok(format!("{name} {version}")),
            None => match versions_by_name.get(name).map(Vec::as_slice) {
                Some([only]) => Ok(only.clone()),
                _ => bail!("Cargo.lock dependency `{name}` does not name one locked package"),
            },
        }
    };
    let name_of = |key: &str| key.split(' ').next().unwrap_or(key).to_owned();

    let mut found = BTreeSet::new();
    for member in members {
        let Some(starts) = versions_by_name.get(member) else {
            continue;
        };
        // Breadth-first, so the path reported is a shortest one.
        let mut parents: BTreeMap<String, Option<String>> = BTreeMap::new();
        let mut queue = std::collections::VecDeque::new();
        for start in starts {
            parents.insert(start.clone(), None);
            queue.push_back(start.clone());
        }
        while let Some(current) = queue.pop_front() {
            for entry in graph.get(&current).into_iter().flatten() {
                let next = resolve(entry)?;
                if parents.contains_key(&next) {
                    continue;
                }
                parents.insert(next.clone(), Some(current.clone()));
                queue.push_back(next);
            }
        }
        // Judged per edge rather than per reached crate, so a retired crate a sanctioned edge
        // brings in does not hide a second, unsanctioned edge to it. A member's own edges are
        // `check_dependencies`'s, and a retired crate's own edges are not judged at all: its
        // presence is what was judged.
        for via in parents.keys() {
            let via_name = name_of(via);
            if starts.contains(via) || RETIRED_DEPENDENCIES.contains(&via_name.as_str()) {
                continue;
            }
            for entry in graph.get(via).into_iter().flatten() {
                let retired = name_of(&resolve(entry)?);
                if !RETIRED_DEPENDENCIES.contains(&retired.as_str())
                    || SANCTIONED_TRANSITIVE_EDGES
                        .iter()
                        .any(|(from, to, _)| *from == via_name && *to == retired)
                {
                    continue;
                }
                let mut path = vec![retired.clone()];
                let mut cursor = Some(via.clone());
                while let Some(key) = cursor {
                    path.push(name_of(&key));
                    cursor = parents.get(&key).cloned().flatten();
                }
                path.reverse();
                found.insert(format!(
                    "active package `{member}` depends on retired or not-yet-approved \
                     `{retired}` transitively ({}); remove the path, or add `{via_name}` -> \
                     `{retired}` to SANCTIONED_TRANSITIVE_EDGES with a reason",
                    path.join(" -> ")
                ));
            }
        }
    }
    Ok(found.into_iter().collect())
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

    // A positive control before any negative is trusted. `cargo tree -p <name>` exits **zero and
    // prints nothing** when `<name>` is not a package it knows, so a rename or a typo in
    // `PACKAGES` would make this whole guard pass silently for the rest of its life — the exact
    // failure mode a boundary check exists to prevent.
    let members = workspace_member_names(root)?;
    for package in PACKAGES {
        if !members.contains(*package) {
            violations.push(format!(
                "the chrono guard names `{package}`, which is not a workspace member; \
                 `cargo tree -p` would exit zero and print nothing, so the guard would pass \
                 without checking anything"
            ));
        }
    }

    for package in PACKAGES {
        if !members.contains(*package) {
            continue;
        }
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

        let reaches_chrono = match classify_chrono_tree(
            output.status.success(),
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        ) {
            Ok(reaches) => reaches,
            Err(detail) => {
                violations.push(format!(
                    "the chrono guard could not decide anything about `{package}`: {detail}"
                ));
                continue;
            }
        };

        let expected = EXPECTED_ON_THE_PATH.contains(package);
        if reaches_chrono && !expected {
            violations.push(format!(
                "`{package}` reaches `chrono` through its own manifest; design/dependencies.md \
                 permits chrono only as the sea-orm column type in `capsule-cli/entity` and in \
                 `capsule-server-migration`. Check that sea-orm is declared with \
                 `default-features = false`"
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

/// Decide what one `cargo tree -i chrono` invocation established, or that it established nothing.
///
/// Split out of [`check_chrono_isolation`] because it is the whole of the guard's judgement and
/// the only part that can be tested without a cargo invocation — and because the shape it is
/// written against is not obvious. `cargo tree -i` has **three** outcomes and the previous
/// version of this guard collapsed them into two:
///
/// - exit zero, a tree on stdout — the crate is reachable;
/// - exit zero, `warning: nothing to print.` on stderr — it is not;
/// - exit non-zero, `did not match any packages` — `chrono` is not in the lock file at all,
///   which is also "not reachable", and is the state this repository would be in if the CLI's
///   entity crate ever dropped it.
///
/// Anything else non-zero is a **failed invocation** — a manifest error, a lock that needs
/// updating under `--offline`, a cargo that is not there — and reading it as "chrono absent" is
/// failing open: the guard would report clean precisely when it knows least. So it is an error,
/// and the caller turns it into a violation.
fn classify_chrono_tree(success: bool, stdout: &str, stderr: &str) -> Result<bool, String> {
    if success {
        return Ok(stdout.contains("chrono v"));
    }
    if stderr.contains("did not match any packages") {
        return Ok(false);
    }
    Err(format!(
        "`cargo tree` failed: {}",
        stderr.trim().lines().next().unwrap_or("no diagnostic")
    ))
}

/// Every workspace member's package name.
fn workspace_member_names(root: &Path) -> Result<BTreeSet<String>> {
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
        if member_ids.contains(id) {
            names.insert(
                package["name"]
                    .as_str()
                    .context("package name missing")?
                    .to_owned(),
            );
        }
    }
    Ok(names)
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
}
