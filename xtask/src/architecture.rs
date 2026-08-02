//! Enforces the active Rust workspace and retired-boundary decisions.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use toml_edit::DocumentMut;

const RETIRED_DEPENDENCIES: &[&str] = &[
    "async-graphql",
    "async-graphql-salvo",
    "capsule-media",
    "chromahash",
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
    check_legacy_manifests(root, &mut violations)?;
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
        Some(".git" | ".gradle" | "build" | "dist" | "legacy-review" | "node_modules" | "target")
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

    #[test]
    fn retired_dependency_list_pins_deferred_chromahash() {
        assert!(RETIRED_DEPENDENCIES.contains(&"chromahash"));
        assert!(RETIRED_DEPENDENCIES.contains(&"object_store"));
        assert!(RETIRED_DEPENDENCIES.contains(&"tonic"));
    }
}
