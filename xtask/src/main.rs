//! Repo-maintenance tasks for the Capsule workspace.
//!
//! Commands:
//!
//! - `set-version <X.Y.Z>` writes a single repo-wide version string into every
//!   package's source of truth so a release bump stays in sync across Rust, web,
//!   docs, Android, and iOS. Each per-format editor is a pure
//!   `&str -> Result<String>` function so it can be unit-tested without disk I/O.
//! - `i18n [--check]` compiles the canonical `locales/` catalogs into each
//!   platform's native localization format (see [`i18n`]). `--check` verifies the
//!   committed files are up to date instead of writing them.
//! - `i18n-guard` scans the web/SwiftUI/Compose surfaces for hardcoded user-facing
//!   string literals and fails on any that is not a catalog key or allowlisted (the
//!   i18n regression gate; see [`i18n_guard`]).
//! - `translate-readme [--check | --extract]` regenerates the translated
//!   `README.<lang>.md` files from the repo-root `README.md` (see
//!   [`translate_readme`]). `--check` is the CI drift gate; `--extract` scaffolds a
//!   locale's translation-data file.
//! - `architecture-check` rejects implicit workspace packages, retired dependencies,
//!   buildable review-only code, stale component references, and any reach for the
//!   filesystem, the environment or a database from the stateless `capsule-core` (see
//!   [`architecture`]).
//! - `test-layout-check` rejects inline test modules under any workspace member's `src/`: unit
//!   tests live in a sibling `tests.rs` (see [`test_layout`]).
//! - `wasm-surface [--check]` digests the public half of the `capsule_wasm.d.ts` wasm-bindgen
//!   emits into the committed `capsule-wasm/wasm-surface.d.ts` the docs build reads (see
//!   [`wasm_surface`]). `--check` is the web gate's drift check.
//! - `bindings-surface [--check]` asserts the generated `capsule_sdk` Swift and Kotlin sources
//!   carry every declaration the native clients consume and dumps them into the committed
//!   `capsule-sdk/bindings-surface.json` (see [`bindings_surface`]). `mise run gen-bindings`
//!   runs it after generating the sources.
//! - `bench-apps <import | compare | record>` normalizes the apps' responsiveness parity
//!   benchmark reports, records per-device baselines and fails on a regression past one (see
//!   [`bench_apps`]).
//! - `screenshot-seed` / `screenshot-compose` are the platform-independent halves of the
//!   README hero screenshot pipeline driven by `mise run screenshot-ios` (see [`screenshot`]).

mod architecture;
mod bench_apps;
mod bindings_surface;
mod drop_kat;
mod i18n;
mod i18n_guard;
mod screenshot;
mod share_kat;
mod test_layout;
mod translate_readme;
mod wasm_surface;

use std::fs;
use std::path::{Path, PathBuf};

use eyre::{Context, ContextCompat, Result, bail};
use regex::{Captures, Regex};
use semver::Version;
use toml_edit::Item;

const USAGE: &str = "usage: xtask <set-version <X.Y.Z> | i18n [--check] | i18n-guard | translate-readme [--check | --extract] | share-kat [<out-path>] | drop-kat [<out-path>] | wasm-surface [--check] | bindings-surface [--check] | architecture-check | test-layout-check | bench-apps <import | compare | record> | screenshot-seed [--anchor YYYY-MM-DD] | screenshot-compose --input <png> --output <png>>";

/// Default output path (repo-relative) for the share-link KAT fixture the bun test consumes.
const SHARE_KAT_OUT: &str = "capsule-web/src/generated/share-kat.json";

/// Default output path (repo-relative) for the guest-drop KAT fixture the bun test consumes.
const DROP_KAT_OUT: &str = "capsule-web/src/generated/drop-kat.json";

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
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
        Some("i18n-guard") => i18n_guard::run(&repo_root()),
        Some("architecture-check") => architecture::run(&repo_root()),
        Some("test-layout-check") => test_layout::run(&repo_root()),
        Some("translate-readme") => {
            let mode = match args.next().as_deref() {
                None => translate_readme::Mode::Generate { api: false },
                Some("--api") => translate_readme::Mode::Generate { api: true },
                Some("--check") => translate_readme::Mode::Check,
                Some("--extract") => translate_readme::Mode::Extract,
                Some(other) => bail!(
                    "unknown flag `{other}`; usage: xtask translate-readme [--check | --extract | --api]"
                ),
            };
            translate_readme::run(&repo_root(), mode)
        }
        Some("share-kat") => {
            let out = args.next().unwrap_or_else(|| SHARE_KAT_OUT.to_string());
            share_kat::run(&repo_root(), &out)
        }
        Some("drop-kat") => {
            let out = args.next().unwrap_or_else(|| DROP_KAT_OUT.to_string());
            drop_kat::run(&repo_root(), &out)
        }
        Some("wasm-surface") => wasm_surface::run(&repo_root(), check_flag(args.next())?),
        Some("bindings-surface") => bindings_surface::run(&repo_root(), check_flag(args.next())?),
        Some("bench-apps") => bench_apps::run(args),
        Some("screenshot-seed") => screenshot::run_seed(&repo_root(), args),
        Some("screenshot-compose") => screenshot::run_compose(args),
        Some(other) => bail!("unknown command `{other}`; {USAGE}"),
        None => bail!("{USAGE}"),
    }
}

/// `--check` or nothing; anything else is a typo that must not silently mean "write".
fn check_flag(arg: Option<String>) -> Result<bool> {
    match arg.as_deref() {
        None => Ok(false),
        Some("--check") => Ok(true),
        Some(other) => bail!("unknown flag `{other}`; expected `--check` or nothing"),
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
mod tests;
