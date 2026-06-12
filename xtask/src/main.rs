//! Repo-maintenance tasks for the Capsule workspace.
//!
//! Two commands:
//!
//! - `set-version <X.Y.Z>` writes a single repo-wide version string into every
//!   package's source of truth so a release bump stays in sync across Rust, web,
//!   docs, Python, Android, and iOS. Each per-format editor is a pure
//!   `&str -> Result<String>` function so it can be unit-tested without disk I/O.
//! - `i18n [--check]` compiles the canonical `locales/` catalogs into each
//!   platform's native localization format (see [`i18n`]). `--check` verifies the
//!   committed files are up to date instead of writing them.

mod i18n;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use regex::{Captures, Regex};
use semver::Version;
use toml_edit::Item;

const USAGE: &str = "usage: xtask <set-version <X.Y.Z> | i18n [--check]>";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("set-version") => {
            let raw = args.next().context("usage: xtask set-version <X.Y.Z>")?;
            let version = Version::parse(&raw)
                .with_context(|| format!("`{raw}` is not a valid semantic version"))?;
            set_version(&repo_root(), &version.to_string())
        }
        Some("i18n") => {
            let check = args.next().as_deref() == Some("--check");
            i18n::run(&repo_root(), check)
        }
        Some(other) => bail!("unknown command `{other}`; {USAGE}"),
        None => bail!("{USAGE}"),
    }
}

/// The workspace root — `xtask` lives at `<root>/xtask`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// A per-format editor: rewrites file `contents` with the new `version`.
type Editor = fn(&str, &str) -> Result<String>;

/// Write `version` into every package's version source of truth.
fn set_version(root: &Path, version: &str) -> Result<()> {
    let edits: &[(&str, Editor)] = &[
        ("Cargo.toml", set_cargo_workspace_version),
        ("capsule-vision/pyproject.toml", set_pyproject_version),
        ("capsule-web/package.json", set_package_json_version),
        ("capsule-docs/package.json", set_package_json_version),
        ("gradle.properties", set_gradle_properties_version),
        ("capsule-swift/Project.swift", set_marketing_version),
    ];
    for (rel, edit) in edits {
        let path = root.join(rel);
        let input =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let output = edit(&input, version).with_context(|| format!("updating {rel}"))?;
        if output == input {
            println!("unchanged {rel}");
        } else {
            fs::write(&path, output).with_context(|| format!("writing {}", path.display()))?;
            println!("updated   {rel} -> {version}");
        }
    }
    Ok(())
}

/// Root `Cargo.toml`'s `[workspace.package] version` — the SSoT for every Rust crate.
fn set_cargo_workspace_version(input: &str, version: &str) -> Result<String> {
    let mut doc = input
        .parse::<toml_edit::DocumentMut>()
        .context("parsing Cargo.toml")?;
    // toml_edit's chained `get_mut`/indexing auto-vivifies, so check existence first.
    let exists = doc
        .get("workspace")
        .and_then(Item::as_table)
        .and_then(|w| w.get("package"))
        .and_then(Item::as_table)
        .is_some_and(|p| p.contains_key("version"));
    if !exists {
        bail!("[workspace.package] version not found");
    }
    doc["workspace"]["package"]["version"] = toml_edit::value(version);
    Ok(doc.to_string())
}

/// `capsule-vision/pyproject.toml`'s `[project] version`.
fn set_pyproject_version(input: &str, version: &str) -> Result<String> {
    let mut doc = input
        .parse::<toml_edit::DocumentMut>()
        .context("parsing pyproject.toml")?;
    let exists = doc
        .get("project")
        .and_then(Item::as_table)
        .is_some_and(|p| p.contains_key("version"));
    if !exists {
        bail!("[project] version not found");
    }
    doc["project"]["version"] = toml_edit::value(version);
    Ok(doc.to_string())
}

/// The top-level `"version"` field of a `package.json` (formatting preserved).
fn set_package_json_version(input: &str, version: &str) -> Result<String> {
    replace_value(
        input,
        r#"(?m)^(?P<pre>\s*"version"\s*:\s*")[^"]*(?P<post>")"#,
        version,
        "\"version\" field",
    )
}

/// Tuist's `MARKETING_VERSION` build setting in `Project.swift` — the iOS app version.
fn set_marketing_version(input: &str, version: &str) -> Result<String> {
    replace_value(
        input,
        r#"(?m)^(?P<pre>\s*"MARKETING_VERSION"\s*:\s*")[^"]*(?P<post>")"#,
        version,
        "MARKETING_VERSION setting",
    )
}

/// `capsule.versionName` (set to `version`) and `capsule.versionCode` (incremented —
/// it's a monotonic Android build number) in `gradle.properties`.
fn set_gradle_properties_version(input: &str, version: &str) -> Result<String> {
    let name_re = Regex::new(r"(?m)^capsule\.versionName=.*$").expect("static regex is valid");
    if !name_re.is_match(input) {
        bail!("capsule.versionName not found");
    }
    let with_name = name_re
        .replace(input, |_: &Captures| {
            format!("capsule.versionName={version}")
        })
        .into_owned();

    let code_re = Regex::new(r"(?m)^capsule\.versionCode=(?P<code>\d+)[ \t]*$")
        .expect("static regex is valid");
    let next = code_re
        .captures(&with_name)
        .context("capsule.versionCode not found")?
        .name("code")
        .map(|m| m.as_str())
        .unwrap_or_default()
        .parse::<u64>()
        .context("capsule.versionCode is not an integer")?
        + 1;
    Ok(code_re
        .replace(&with_name, |_: &Captures| {
            format!("capsule.versionCode={next}")
        })
        .into_owned())
}

/// Replace the value captured between named groups `pre` and `post` of the first match
/// of `pattern` with `version`, erroring (via `what`) when nothing matches.
fn replace_value(input: &str, pattern: &str, version: &str, what: &str) -> Result<String> {
    let re = Regex::new(pattern).expect("static regex is valid");
    if !re.is_match(input) {
        bail!("{what} not found");
    }
    Ok(re
        .replace(input, |caps: &Captures| {
            format!("{}{version}{}", &caps["pre"], &caps["post"])
        })
        .into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_workspace_version_updates_and_preserves_rest() {
        let input = "[workspace.package]\nversion = \"0.1.0\"\nedition = \"2024\"\n";
        let out = set_cargo_workspace_version(input, "0.2.0").unwrap();
        assert!(out.contains("version = \"0.2.0\""));
        assert!(
            out.contains("edition = \"2024\""),
            "unrelated keys preserved"
        );
    }

    #[test]
    fn cargo_missing_version_errors() {
        assert!(set_cargo_workspace_version("[workspace]\nmembers = []\n", "0.2.0").is_err());
    }

    #[test]
    fn pyproject_version_updates() {
        let input = "[project]\nname = \"capsule-vision\"\nversion = \"0.1.0\"\n";
        let out = set_pyproject_version(input, "1.2.3").unwrap();
        assert!(out.contains("version = \"1.2.3\""));
        assert!(out.contains("name = \"capsule-vision\""));
    }

    #[test]
    fn package_json_version_updates_field_and_keeps_formatting() {
        let input =
            "{\n  \"name\": \"capsule-web\",\n  \"version\": \"0.1.0\",\n  \"private\": true\n}\n";
        let out = set_package_json_version(input, "0.2.0").unwrap();
        assert!(out.contains("\"version\": \"0.2.0\""));
        assert!(out.contains("\"name\": \"capsule-web\""));
        assert!(out.contains("\"private\": true"));
    }

    #[test]
    fn gradle_bumps_name_and_increments_code() {
        let input = "capsule.versionName=0.1.0\ncapsule.versionCode=7\n";
        let out = set_gradle_properties_version(input, "0.2.0").unwrap();
        assert!(out.contains("capsule.versionName=0.2.0"));
        assert!(
            out.contains("capsule.versionCode=8"),
            "versionCode is monotonic"
        );
    }

    #[test]
    fn marketing_version_updates() {
        let input = "        \"MARKETING_VERSION\": \"0.1.0\",\n";
        let out = set_marketing_version(input, "3.4.5").unwrap();
        assert_eq!(out, "        \"MARKETING_VERSION\": \"3.4.5\",\n");
    }

    #[test]
    fn missing_targets_error() {
        assert!(set_gradle_properties_version("nope=1\n", "0.2.0").is_err());
        assert!(set_marketing_version("no setting here\n", "0.2.0").is_err());
        assert!(set_package_json_version("{}\n", "0.2.0").is_err());
        assert!(set_pyproject_version("[tool]\nx = 1\n", "0.2.0").is_err());
    }
}
