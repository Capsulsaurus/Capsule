//! `capsule library migrate` — admit a pre-signed-path library's unsigned sidecars as signed
//! assets (issue `#456`).
//!
//! The verb itself is [`Workspace::migrate_unsigned_sidecars`]; this module is only the CLI's
//! half of it: the options the command line resolves ([`options`]), the rendering of
//! the [`UnsignedMigrationReport`] it returns ([`render`], [`describe_skip`]), and the notice
//! every workspace-opening verb prints while sidecars are still waiting ([`unmigrated_notice`]),
//! so the count is visible before the user runs the migration.
//!
//! The fallback album is the workspace's default album — the one `open_workspace` resolves at
//! open exactly as every other verb does — so a legacy record with no usable `album_id` lands
//! where an import would have put it.

use capsule_library::lifecycle::{
    LifecycleError, MigrationSkip, UnmigratedSidecar, UnsignedMigrationOptions,
    UnsignedMigrationReport, Workspace,
};
use colored::Colorize as _;

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
pub fn unmigrated_notice(unmigrated: &[UnmigratedSidecar]) -> Option<String> {
    match unmigrated.len() {
        0 => None,
        1 => Some(
            "1 sidecar in this library is unsigned, so its asset is hidden. Run `capsule library \
             migrate` to admit it."
                .to_owned(),
        ),
        count => Some(format!(
            "{count} sidecars in this library are unsigned, so their assets are hidden. Run \
             `capsule library migrate` to admit them."
        )),
    }
}

/// The line a run that found nothing to do prints.
const NOTHING_TO_MIGRATE: &str = "No unsigned sidecars to migrate.";

/// Why the migration refused one sidecar.
#[must_use]
pub fn describe_skip(skip: &MigrationSkip) -> String {
    match skip {
        MigrationSkip::InvalidAssetId(name) => format!("the file name {name} is not an asset id"),
        MigrationSkip::UnknownShape => {
            "the sidecar is neither a signed nor an old unsigned record".to_owned()
        }
        MigrationSkip::Undecodable(detail) => {
            format!("the old record does not decode: {detail}")
        }
        MigrationSkip::IdCollision(asset_id) => {
            format!("asset {asset_id} is already a signed asset in this library")
        }
        MigrationSkip::OriginalMissing(asset_id) => {
            format!("asset {asset_id}: no original media file beside the sidecar")
        }
        MigrationSkip::HashMismatch {
            asset_id,
            recorded,
            actual,
        } => format!(
            "asset {asset_id}: the original's SHA-256 is {actual}, but the old record says \
             {recorded}"
        ),
        MigrationSkip::QuarantineConflict(asset_id) => format!(
            "asset {asset_id}: the quarantine folder already holds a different copy of this \
             sidecar"
        ),
        MigrationSkip::OutsideMonthBucket { asset_id, dir } => format!(
            "asset {asset_id}: {} is not a media/YYYY/YYYY-MM folder",
            dir.to_string_lossy()
        ),
        MigrationSkip::UnusualExtension { asset_id, ext } => format!(
            "asset {asset_id}: the original's extension {ext} is not a lowercase single segment"
        ),
        MigrationSkip::AlbumReadOnly { asset_id, album_id } => format!(
            "asset {asset_id}: album {album_id} is read-only on this device, so its pending \
             delete could not be recorded"
        ),
        MigrationSkip::Stranded(asset_id) => {
            format!("asset {asset_id}: signed sidecar without a provenance chain cannot be resumed")
        }
    }
}

/// Render one run as the lines `capsule library migrate` prints: a row per migrated asset,
/// per asset moved to trash, per derived stack, and per refused sidecar with its reason, then
/// the summary. `remaining` is how many unanchored sidecars the closing rescan still found.
#[must_use]
pub fn render(report: &UnsignedMigrationReport, remaining: usize) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };

    for asset_id in &report.migrated {
        line(format!("Migrated {asset_id}").green().to_string());
    }
    for asset_id in &report.trashed {
        line(format!(
            "Moved {asset_id} to trash, as the old library had deleted it"
        ));
    }
    for stack_id in &report.stacks {
        line(format!(
            "Stack {stack_id} rebuilt from the old library's stack hints"
        ));
    }
    for (path, skip) in &report.skipped {
        let reason = describe_skip(skip);
        line(
            format!("Skipped {}: {reason}", path.to_string_lossy())
                .yellow()
                .to_string(),
        );
    }

    let nothing = report.migrated.is_empty()
        && report.trashed.is_empty()
        && report.skipped.is_empty()
        && remaining == 0;
    if nothing {
        line(NOTHING_TO_MIGRATE.green().to_string());
    } else {
        line(format!(
            "Migration: {} migrated, {} moved to trash, {} stack(s), {} skipped, {remaining} \
             still unmigrated.",
            report.migrated.len(),
            report.trashed.len(),
            report.stacks.len(),
            report.skipped.len(),
        ));
    }
    out
}

#[cfg(test)]
mod tests;
