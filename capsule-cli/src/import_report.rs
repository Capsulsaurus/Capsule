//! What `capsule import` reports: the run's totals and each file's outcome, as the one JSON
//! document `--json` prints and as the closing line a person reads.
//!
//! The report exists so a gap is never silent (SSoT: thumbnails § Media Is Rawshift's). Every
//! file is kept whatever the build's codecs support, so the interesting number after an import
//! is not "how many were refused" (none are) but **how many arrived without derivatives**, and
//! why: `deferred` is the expected gap a later build backfills (a HEIC, a RAW, a video), while
//! `decode_failed` is a format this build decodes whose bytes did not.

use std::path::Path;

use capsule_library::import::{
    ImportActionPlan, ImportDecision, ImportExecutionSummary, ImportOutcome, PlanCounts,
};
use capsule_library::lifecycle::DerivativeStatus;
use serde::Serialize;

/// Where one source file ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileImportStatus {
    /// Imported as a signed, encrypted, verifiable original.
    Imported,
    /// The library already holds these bytes; `asset_id` names the asset that does.
    Duplicate,
    /// The file could not be read or committed; `error` says why.
    Failed,
    /// Left out for a reason that is neither a duplicate nor an error.
    Skipped,
}

/// Whether an imported original has its thumbnail and preview yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivativeReport {
    /// The derivatives this build can encode were generated.
    Generated,
    /// No codec in this build for the format; generated when a later build can.
    Deferred,
    /// A format this build decodes, whose bytes did not decode.
    DecodeFailed,
    /// Nothing to derive: a document, a sidecar or an unknown kind of file.
    NotApplicable,
}

impl From<DerivativeStatus> for DerivativeReport {
    fn from(status: DerivativeStatus) -> Self {
        match status {
            DerivativeStatus::Decoded => Self::Generated,
            DerivativeStatus::DeferredNoCodec => Self::Deferred,
            DerivativeStatus::DecodeFailed => Self::DecodeFailed,
            DerivativeStatus::NotAKnownStill => Self::NotApplicable,
        }
    }
}

/// One source file's line of the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileImportOutcome {
    /// The source path as given on the command line or found under it.
    pub path: String,
    /// Where the file ended up.
    pub status: FileImportStatus,
    /// The asset holding the file: the new one, or the existing one for a duplicate.
    pub asset_id: Option<String>,
    /// For an imported file, whether its derivatives exist yet.
    pub derivatives: Option<DerivativeReport>,
    /// For an imported file, the `(tier, format)` variants this build cannot encode.
    pub deferred_formats: u32,
    /// For a failed or skipped file, why.
    pub error: Option<String>,
}

/// The outcome of a `capsule import` run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// Candidates the scan found (a RAW+JPEG pair is one candidate of two files).
    pub candidates: usize,
    /// Source files the scan found.
    pub files: usize,
    /// Candidates the plan chose to import.
    pub planned: usize,
    /// Source bytes the plan chose to import.
    pub planned_bytes: u64,
    /// Files imported this run.
    pub imported: usize,
    /// Files the library already held.
    pub duplicates: usize,
    /// Files that could not be read or committed.
    pub errors: usize,
    /// Imported files whose derivatives are deferred until a build has their codec.
    pub derivatives_deferred: usize,
    /// Imported files in a decodable format whose bytes did not decode.
    pub derivatives_decode_failed: usize,
    /// `(tier, format)` variants across the run this build cannot encode.
    pub formats_deferred: usize,
    /// One entry per source file the run reached, sorted by path.
    pub outcomes: Vec<FileImportOutcome>,
}

impl ImportReport {
    /// The report for a run that scanned `candidates` candidates of `files` files, planned
    /// `counts`, and executed into `summary` (empty when nothing was executed).
    #[must_use]
    pub fn new(
        candidates: usize,
        files: usize,
        counts: &PlanCounts,
        summary: &ImportExecutionSummary,
    ) -> Self {
        let mut outcomes: Vec<FileImportOutcome> = summary
            .outcomes
            .iter()
            .map(|(path, outcome)| file_outcome(path, outcome))
            .collect();
        // Plan order follows the scanner's grouping, which is not stable between runs; a
        // report that diffs cleanly between two runs over the same tree is sorted.
        outcomes.sort_by(|a, b| a.path.cmp(&b.path));
        Self {
            candidates,
            files,
            planned: counts.to_import,
            planned_bytes: counts.total_size,
            imported: summary.imported_count(),
            duplicates: summary.duplicate_count(),
            errors: summary.error_count(),
            derivatives_deferred: summary.deferred_derivative_count(),
            derivatives_decode_failed: summary.decode_failed_count(),
            formats_deferred: summary.deferred_format_count(),
            outcomes,
        }
    }
}

fn file_outcome(path: &Path, outcome: &ImportOutcome) -> FileImportOutcome {
    let mut line = FileImportOutcome {
        path: path.display().to_string(),
        status: FileImportStatus::Skipped,
        asset_id: None,
        derivatives: None,
        deferred_formats: 0,
        error: None,
    };
    match outcome {
        ImportOutcome::Imported {
            asset_id,
            derivatives,
            deferred_formats,
        } => {
            line.status = FileImportStatus::Imported;
            line.asset_id = Some(asset_id.to_string());
            line.derivatives = Some((*derivatives).into());
            line.deferred_formats = *deferred_formats;
        }
        ImportOutcome::DuplicateSkipped { existing_uuid } => {
            line.status = FileImportStatus::Duplicate;
            line.asset_id = Some(existing_uuid.clone());
        }
        ImportOutcome::CorruptUnreadable(reason) | ImportOutcome::PermissionDenied(reason) => {
            line.status = FileImportStatus::Failed;
            line.error = Some(reason.clone());
        }
        ImportOutcome::CorruptTransfer => {
            line.status = FileImportStatus::Failed;
            line.error = Some("the bytes changed while they were read".to_owned());
        }
        ImportOutcome::PartialStackImported { skipped, .. } => {
            line.error = Some(format!("part of a stack; skipped {}", skipped.join(", ")));
        }
        ImportOutcome::LivePhotoWithoutPair => {
            line.error = Some("a Live Photo half without its pair".to_owned());
        }
    }
    line
}

/// The outcomes a plan decided without executing anything: each duplicate and each file it
/// could not read, exactly as the executor would have reported them. A run with nothing to
/// import reports from this, so an all-unreadable source still names every file it lost.
#[must_use]
pub fn skipped_by_plan(plan: &ImportActionPlan) -> ImportExecutionSummary {
    let outcomes = plan
        .actions
        .iter()
        .filter_map(|(candidate, decision)| {
            let path = candidate.primary_path().clone();
            match decision {
                ImportDecision::Import => None,
                ImportDecision::SkipDuplicate { existing_uuid } => Some((
                    path,
                    ImportOutcome::DuplicateSkipped {
                        existing_uuid: existing_uuid.clone(),
                    },
                )),
                ImportDecision::SkipError(reason) => {
                    Some((path, ImportOutcome::CorruptUnreadable(reason.clone())))
                }
            }
        })
        .collect();
    ImportExecutionSummary { outcomes }
}

/// The closing line of a human-readable run: what was imported, and every reason an imported
/// original is still missing its derivatives, so a library of HEICs or videos never looks like
/// it silently lost its previews.
#[must_use]
pub fn summary_line(report: &ImportReport) -> String {
    let mut parts = vec![format!(
        "Done: {} imported, {} duplicate(s), {} error(s).",
        report.imported, report.duplicates, report.errors
    )];
    if report.derivatives_deferred > 0 {
        parts.push(format!(
            "{} without previews until a build has their codec.",
            report.derivatives_deferred
        ));
    }
    if report.derivatives_decode_failed > 0 {
        parts.push(format!(
            "{} in a supported format did not decode; their originals are kept.",
            report.derivatives_decode_failed
        ));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests;
