//! The README hero screenshot pipeline's platform-independent halves. `mise run screenshot-ios`
//! (`mise-tasks/screenshot-ios`) drives them around the macOS-only simulator capture:
//!
//! 1. `xtask screenshot-seed [--anchor YYYY-MM-DD]` — fetch, verify and normalize the CC0 seed
//!    photos listed in `xtask/screenshot/seed.toml` into `target/screenshot/seed/` ([`seed`]).
//! 2. *(macOS)* seed a freshly erased simulator, launch the app, capture light and dark.
//! 3. `xtask screenshot-compose --input <capture.png> --output <hero.png>` — mask, shadow and pad
//!    the capture into the image the README embeds ([`compose`]).

mod capture_exif;
mod compose;
mod manifest;
mod seed;

use std::fs;
use std::path::{Path, PathBuf};

use eyre::{Context, ContextCompat, Result, bail};
use jiff::civil::Date;

/// The manifest, relative to the repo root.
const MANIFEST: &str = "xtask/screenshot/seed.toml";
/// Scratch space under the (git-ignored) Cargo target directory.
const TARGET: &str = "target/screenshot";
/// Timeline density the hero is captured at; `mise-tasks/screenshot-ios` writes the same value
/// into the app's `timeline.columnCount` default.
const HERO_COLUMNS: usize = 3;

/// `screenshot-seed [--anchor YYYY-MM-DD]`. Prints the seed digest on stdout for the caller.
pub(crate) fn run_seed(root: &Path, mut args: impl Iterator<Item = String>) -> Result<()> {
    let mut anchor = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--anchor" => {
                let raw = args.next().context("--anchor needs a YYYY-MM-DD date")?;
                anchor = Some(
                    raw.parse::<Date>()
                        .with_context(|| format!("`{raw}` is not a YYYY-MM-DD date"))?,
                );
            }
            other => bail!(
                "unknown argument `{other}`; usage: xtask screenshot-seed [--anchor YYYY-MM-DD]"
            ),
        }
    }
    // The simulator shares the host's clock and time zone, so "today" here is "today" there —
    // and the app titles sections against that real clock, not against the anchor.
    let today = jiff::Zoned::now().date();
    let anchor = anchor.unwrap_or(today);
    if anchor > today {
        bail!("--anchor {anchor} is in the future; the app would title those days as upcoming");
    }

    let path = root.join(MANIFEST);
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let manifest =
        manifest::Manifest::parse(&text).with_context(|| format!("invalid {MANIFEST}"))?;
    manifest.check_full_rows(HERO_COLUMNS)?;
    let stale = manifest.previous_year_photos(anchor, today)?;
    if !stale.is_empty() {
        let first = stale.first().map_or("", |p| p.id.as_str());
        tracing::warn!(
            count = stale.len(),
            first = %first,
            "some capture days fall in an earlier year; their section headers will carry a year suffix"
        );
    }
    let layout = seed::Layout::under(&root.join(TARGET));
    let digest = seed::run(&manifest, &layout, anchor, &seed::CurlFetcher)?;
    println!("{digest}");
    Ok(())
}

/// `screenshot-compose --input <capture.png> --output <hero.png>`.
pub(crate) fn run_compose(mut args: impl Iterator<Item = String>) -> Result<()> {
    const USAGE: &str = "usage: xtask screenshot-compose --input <capture.png> --output <hero.png>";
    let (mut input, mut output) = (None::<PathBuf>, None::<PathBuf>);
    while let Some(arg) = args.next() {
        let slot = match arg.as_str() {
            "--input" => &mut input,
            "--output" => &mut output,
            other => bail!("unknown argument `{other}`; {USAGE}"),
        };
        *slot = Some(args.next().context(USAGE)?.into());
    }
    compose::run(
        &input.context(USAGE)?,
        &output.context(USAGE)?,
        &compose::HERO,
    )
}
