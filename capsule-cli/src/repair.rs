//! `capsule repair capture-time` — recover capture timestamps stamped with import time.
//!
//! Before the parser fix, `extract_exif` could never parse a well-formed `DateTimeOriginal`, so
//! every import wrote its own clock as the asset's capture time — **inside the signed sidecar**,
//! and as the `media/{YYYY}/{YYYY-MM}` shard. Fixing the parser did not fix the data: the wrong
//! value is under signature, a rebuild reconstructs from those same sidecars and would faithfully
//! preserve it, and nothing else ever goes back to the original to ask. This pass does.
//!
//! ## The rule
//!
//! For every managed asset, re-read the original's EXIF and resolve it exactly as the
//! importer does ([`resolve_timezone`] over [`extract_exif`]):
//!
//! - **No resolvable instant** — no EXIF, no `DateTimeOriginal`, or a floating one with
//!   neither an `OffsetTimeOriginal` nor a fix to anchor it — is **skipped**. Nothing is
//!   recoverable, and nothing is known to be broken: the importer would write the same value
//!   today. (A floating time is not treated as UTC here for the same reason the importer does
//!   not: that would be a guess signed as a fact.)
//! - **An instant equal to the recorded one** is skipped: the asset is correct.
//! - **An instant that differs** is *affected*. The comparison is against the sidecar's
//!   **as-imported** capture timestamp only — never against import time, and never against a
//!   user's correction — so an asset genuinely imported the second it was taken is not
//!   reported as broken, and a deliberate correction is not mistaken for a parser fault.
//! - **An original that cannot be read** is reported as unreadable, never as "no EXIF".
//! - **An asset in trash** is skipped and counted as its own category before its original is
//!   read: appending an irreversible signed record to an asset the user has decided to
//!   discard is not a repair, and a restored asset is picked up by the next run.
//!
//! By construction the pass is a no-op on a library imported after the parser fix: that importer
//! already wrote the resolved instant wherever one existed, and where none existed this pass
//! skips. Assets whose capture time came from a Takeout record (`CaptureSource::Folded`) have
//! no EXIF instant of their own and are left alone.
//!
//! ## The write
//!
//! Dry run is the default; `--apply` issues one signed `metadata-update` per affected asset
//! through [`Workspace::correct_imported_capture_timestamp`], each an independent write — an
//! interrupted run leaves every completed asset correct and a re-run skips them, because their
//! EXIF now agrees. The media bundle is **not** relocated: the month directory is only the
//! shard fixed at import, which the design treats as expected drift after a capture
//! correction rather than as a fault.
//!
//! ## A user's correction
//!
//! The repair re-records the **as-imported** value — what the import should have read from
//! the file — and nothing else. A capture time the user set through
//! [`Workspace::set_capture_timestamp`] lives in its own register beside that value and stays
//! the effective instant, so the repair neither reports it as broken nor overwrites it.

use std::path::Path;

use capsule_library::exif::{extract_exif, resolve_timezone};
use capsule_library::lifecycle::Workspace;
use colored::Colorize as _;
use jiff::Timestamp;
use thiserror::Error;
use uuid::Uuid;

/// One `capsule repair capture-time` invocation, independent of the argument parser.
#[derive(Debug, Clone, Copy, Default)]
pub struct RepairRequest {
    /// Write the corrections. Without it the pass only reports.
    pub apply: bool,
    /// Correct at most this many affected assets in one `--apply` run. Detection still covers
    /// the whole library; the dry-run report is unaffected.
    pub limit: Option<usize>,
}

/// What re-reading one original's EXIF says about its recorded capture time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The recorded value disagrees with the instant recovered from EXIF.
    Affected {
        /// The sidecar's capture timestamp, parsed; `None` when it does not parse.
        recorded: Option<Timestamp>,
        /// The instant the original's EXIF resolves to.
        recovered: Timestamp,
    },
    /// The recorded value equals the recovered instant.
    Agrees,
    /// The original carries no instant the importer could have resolved: nothing to recover.
    NoInstant,
    /// The original could not be read at all.
    Unreadable(String),
}

/// An affected asset: what the sidecar says, and what its original says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Affected {
    /// The asset.
    pub asset_id: Uuid,
    /// The sidecar's capture timestamp as written.
    pub recorded_text: String,
    /// The sidecar's capture timestamp, parsed; `None` when it does not parse.
    pub recorded: Option<Timestamp>,
    /// The instant the original's EXIF resolves to.
    pub recovered: Timestamp,
}

impl Affected {
    /// Seconds from the recorded instant to the recovered one, when the recorded one parses.
    #[must_use]
    pub fn delta_seconds(&self) -> Option<i64> {
        self.recorded
            .map(|recorded| self.recovered.as_second() - recorded.as_second())
    }
}

/// The verdicts over a whole library, affected assets in ascending asset-id order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detection {
    /// How many assets were examined.
    pub scanned: usize,
    /// Assets whose recorded capture time disagrees with their EXIF.
    pub affected: Vec<Affected>,
    /// Assets whose recorded capture time equals their EXIF instant.
    pub agrees: usize,
    /// Assets with no recoverable instant.
    pub no_instant: usize,
    /// Assets whose original could not be read, with the reason.
    pub unreadable: Vec<(Uuid, String)>,
    /// Assets in trash, skipped without reading their originals.
    pub trashed: Vec<Uuid>,
}

/// What one invocation did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepairSummary {
    /// The detection the run acted on.
    pub detection: Detection,
    /// Whether `--apply` was given.
    pub applied: bool,
    /// The assets corrected, in the order they were written. Empty on a dry run.
    pub corrected: Vec<Uuid>,
    /// Whether `--limit` stopped the run before every affected asset was corrected.
    pub limit_reached: bool,
}

/// Why a repair could not complete.
#[derive(Debug, Error)]
pub enum RepairError {
    /// The lifecycle refused a correction. The run stops at the first refusal rather than
    /// continuing over a library whose sealing may be broken; everything corrected before it
    /// stays corrected.
    #[error("correcting {asset_id}: {detail}")]
    Lifecycle {
        /// The asset the write was for.
        asset_id: Uuid,
        /// The lifecycle's own description.
        detail: String,
    },
}

/// Compare one asset's recorded capture timestamp against what its original's EXIF resolves
/// to, applying the importer's own resolution.
#[tracing::instrument(skip_all, fields(original = %original.display()))]
pub fn detect_one(recorded: &str, original: &Path) -> Verdict {
    // Readability is checked before EXIF is asked for, because `extract_exif` reports "no
    // EXIF container" as an empty extract and an unreadable file must not be confused with it.
    if let Err(error) = std::fs::File::open(original) {
        tracing::warn!(%error, "repair: original unreadable");
        return Verdict::Unreadable(error.to_string());
    }
    let exif = match extract_exif(original) {
        Ok(exif) => exif,
        Err(error) => {
            tracing::warn!(%error, "repair: original unreadable while extracting EXIF");
            return Verdict::Unreadable(error.to_string());
        }
    };
    let Some(secs) = resolve_timezone(&exif).capture_utc else {
        tracing::debug!("repair: no resolvable EXIF instant; nothing to recover");
        return Verdict::NoInstant;
    };
    let Ok(recovered) = Timestamp::from_second(secs) else {
        tracing::warn!(
            secs,
            "repair: EXIF instant out of range; treated as unrecoverable"
        );
        return Verdict::NoInstant;
    };
    let parsed = recorded.parse::<Timestamp>().ok();
    if parsed.is_some_and(|recorded| recorded.as_second() == secs) {
        return Verdict::Agrees;
    }
    tracing::debug!(recorded, %recovered, "repair: recorded capture time disagrees with EXIF");
    Verdict::Affected {
        recorded: parsed,
        recovered,
    }
}

/// Examine every managed asset of `ws`.
#[tracing::instrument(skip_all, fields(assets = ws.asset_ids().len()))]
pub fn detect(ws: &Workspace) -> Detection {
    let mut ids = ws.asset_ids();
    ids.sort_unstable();
    let mut detection = Detection::default();
    for id in ids {
        let Some(asset) = ws.asset(&id) else {
            continue;
        };
        let Some(original) = ws.original_path(&id) else {
            continue;
        };
        detection.scanned += 1;
        if ws.is_trashed(&id) {
            tracing::debug!(asset_id = %id, "repair: asset in trash; skipped");
            detection.trashed.push(id);
            continue;
        }
        let recorded_text = asset.sidecar.capture_timestamp.clone();
        match detect_one(&recorded_text, &original) {
            Verdict::Affected {
                recorded,
                recovered,
            } => detection.affected.push(Affected {
                asset_id: id,
                recorded_text,
                recorded,
                recovered,
            }),
            Verdict::Agrees => detection.agrees += 1,
            Verdict::NoInstant => detection.no_instant += 1,
            Verdict::Unreadable(reason) => detection.unreadable.push((id, reason)),
        }
    }
    tracing::info!(
        scanned = detection.scanned,
        affected = detection.affected.len(),
        agrees = detection.agrees,
        no_instant = detection.no_instant,
        unreadable = detection.unreadable.len(),
        trashed = detection.trashed.len(),
        "repair: capture-time detection complete"
    );
    detection
}

/// Correct the affected assets, at most `limit` of them, each as its own signed write.
#[tracing::instrument(skip_all, fields(affected = affected.len(), limit))]
pub fn apply(
    ws: &mut Workspace,
    affected: &[Affected],
    limit: Option<usize>,
) -> Result<Vec<Uuid>, RepairError> {
    let budget = limit.unwrap_or(affected.len());
    let mut corrected = Vec::new();
    for item in affected.iter().take(budget) {
        ws.correct_imported_capture_timestamp(&item.asset_id, item.recovered)
            .map_err(|error| RepairError::Lifecycle {
                asset_id: item.asset_id,
                detail: error.to_string(),
            })?;
        corrected.push(item.asset_id);
    }
    Ok(corrected)
}

/// Detect, then (with `--apply`) correct.
pub fn run(ws: &mut Workspace, request: RepairRequest) -> Result<RepairSummary, RepairError> {
    let detection = detect(ws);
    let mut summary = RepairSummary {
        applied: request.apply,
        ..Default::default()
    };
    if request.apply {
        summary.corrected = apply(ws, &detection.affected, request.limit)?;
        summary.limit_reached = summary.corrected.len() < detection.affected.len();
    }
    summary.detection = detection;
    Ok(summary)
}

/// A human-readable reason for a [`RepairError`], for the failure line.
pub fn describe_error(error: &RepairError) -> String {
    match error {
        RepairError::Lifecycle { asset_id, detail } => format!(
            "Correcting {asset_id} failed: {detail}. Assets corrected before it stay corrected; \
             nothing after it was touched."
        ),
    }
}

/// The line a run whose every timestamp agrees prints.
const NOTHING_TO_REPAIR: &str =
    "Every capture timestamp agrees with its original's EXIF; nothing to repair.";

/// The closing note a run that corrected anything prints.
const DRIFT_NOTICE: &str = "Corrected assets stay in the month directory they were imported \
                            into; that drift between directory and capture date is expected \
                            after a correction and is not a fault.";

/// Render the run as the lines `capsule repair capture-time` prints.
#[must_use]
pub fn render(request: RepairRequest, summary: &RepairSummary) -> String {
    let detection = &summary.detection;
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };

    line(
        format!(
            "Checked {} asset(s) against the EXIF capture time of their originals.",
            detection.scanned
        )
        .cyan()
        .to_string(),
    );
    if !request.apply {
        line(
            "Dry run: nothing was written. Re-run with --apply to correct the affected sidecars."
                .yellow()
                .to_string(),
        );
    }

    for item in &detection.affected {
        let (asset_id, recorded, recovered) = (item.asset_id, &item.recorded_text, item.recovered);
        line(match item.delta_seconds() {
            Some(delta) => format!(
                "  {asset_id}: recorded {recorded}, EXIF says {recovered} (off by {delta} s)"
            ),
            None => format!(
                "  {asset_id}: recorded {recorded} (not a timestamp), EXIF says {recovered}"
            ),
        });
    }
    for (asset_id, reason) in &detection.unreadable {
        line(
            format!("  {asset_id}: original could not be read ({reason}); skipped")
                .red()
                .to_string(),
        );
    }
    for asset_id in &summary.corrected {
        line(format!("  {asset_id}: corrected").green().to_string());
    }
    if summary.limit_reached {
        line(
            format!(
                "Stopped after {} correction(s) because of --limit; re-run to continue.",
                summary.corrected.len()
            )
            .yellow()
            .to_string(),
        );
    }

    if !detection.trashed.is_empty() {
        line(
            format!(
                "  Skipped {} asset(s) in trash; restore an asset first if it should be repaired.",
                detection.trashed.len()
            )
            .yellow()
            .to_string(),
        );
    }

    if detection.affected.is_empty() && detection.unreadable.is_empty() {
        line(NOTHING_TO_REPAIR.green().to_string());
    } else {
        line(format!(
            "Capture-time repair: {} affected, {} corrected, {} already correct, {} without a \
             recoverable EXIF instant, {} unreadable, {} in trash.",
            detection.affected.len(),
            summary.corrected.len(),
            detection.agrees,
            detection.no_instant,
            detection.unreadable.len(),
            detection.trashed.len(),
        ));
    }
    if !summary.corrected.is_empty() {
        line(DRIFT_NOTICE.dimmed().to_string());
    }
    out
}

#[cfg(test)]
mod tests;
