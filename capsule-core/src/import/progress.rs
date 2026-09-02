use std::path::PathBuf;

use crate::lifecycle::DerivativeStatus;

/// Outcome for a single imported file.
#[derive(Debug, Clone)]
pub enum ImportOutcome {
    /// The file was imported as a signed, encrypted, verifiable original.
    ///
    /// `derivatives` says whether a thumbnail/preview could be generated alongside it. It is
    /// **not** a success/failure flag: an asset whose format has no codec in this build is a
    /// fully successful import that happens to be missing its derivative (slice `S-B13`).
    Imported {
        derivatives: DerivativeStatus,
        /// How many `(tier, format)` pairs the tier table commits to and this build cannot
        /// encode. Orthogonal to `derivatives`: a `Decoded` asset with a renderable WebP
        /// thumbnail still reports the JXL master and the AVIF delivery variant as deferred, and
        /// that count is how the gap shrinks visibly as codecs land rather than silently.
        deferred_formats: u32,
    },
    DuplicateSkipped {
        existing_uuid: String,
    },
    Unsupported,
    CorruptUnreadable(String),
    CorruptTransfer,
    PermissionDenied(String),
    PartialStackImported {
        imported: Vec<String>,
        skipped: Vec<String>,
    },
    LivePhotoWithoutPair,
}

/// Progress events emitted during import execution.
#[derive(Debug)]
pub enum ImportProgressEvent {
    ImportStarted {
        total_candidates: u64,
        total_files: u64,
    },
    CandidateStarted {
        index: u64,
        total: u64,
        primary_path: PathBuf,
    },
    CandidateCompleted {
        index: u64,
        outcomes: Vec<(PathBuf, ImportOutcome)>,
    },
    ImportCompleted {
        summary: ImportExecutionSummary,
    },
}

/// Summary of a completed import run.
#[derive(Debug, Default)]
pub struct ImportExecutionSummary {
    pub outcomes: Vec<(PathBuf, ImportOutcome)>,
}

impl ImportExecutionSummary {
    pub fn imported_count(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|(_, o)| matches!(o, ImportOutcome::Imported { .. }))
            .count()
    }

    /// How many of the imported assets landed **without** thumbnail/preview derivatives because
    /// this build has no codec for their format (slice `S-B13`).
    ///
    /// These are counted by [`imported_count`](Self::imported_count) too — they are successful
    /// imports; the original is signed, encrypted and verifiable. This is the number a caller
    /// reports as "N imported without derivatives" so a HEIC-only library does not look like it
    /// silently lost its previews. Genuine decode failures of *supported* formats are excluded;
    /// see [`decode_failed_count`](Self::decode_failed_count).
    pub fn deferred_derivative_count(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|(_, o)| {
                matches!(
                    o,
                    ImportOutcome::Imported {
                        derivatives: DerivativeStatus::DeferredNoCodec,
                        ..
                    }
                )
            })
            .count()
    }

    /// The total number of `(tier, format)` pairs across the run that the tier table commits to
    /// and this build cannot encode (slice `S-B13`).
    ///
    /// **Not a failure count, and not comparable to
    /// [`deferred_derivative_count`](Self::deferred_derivative_count).** That one counts *assets*
    /// with no thumbnail at all; this one counts *format variants* missing from assets that do
    /// have one. A library of decodable JPEGs reports zero deferred derivatives and two deferred
    /// formats per asset — the JXL master and the AVIF delivery variant — which is exactly the
    /// number that should fall to zero as the encoders land, and the reason it is reported rather
    /// than left implicit in a doc.
    pub fn deferred_format_count(&self) -> usize {
        self.outcomes
            .iter()
            .map(|(_, o)| match o {
                ImportOutcome::Imported {
                    deferred_formats, ..
                } => *deferred_formats as usize,
                _ => 0,
            })
            .sum()
    }

    /// How many imported assets are in a format this build *does* support but whose bytes did
    /// not decode — unlike [`deferred_derivative_count`](Self::deferred_derivative_count) this
    /// is a real problem worth surfacing, not an expected gap. The original is still imported.
    pub fn decode_failed_count(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|(_, o)| {
                matches!(
                    o,
                    ImportOutcome::Imported {
                        derivatives: DerivativeStatus::DecodeFailed,
                        ..
                    }
                )
            })
            .count()
    }

    pub fn duplicate_count(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|(_, o)| matches!(o, ImportOutcome::DuplicateSkipped { .. }))
            .count()
    }

    pub fn error_count(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|(_, o)| {
                matches!(
                    o,
                    ImportOutcome::CorruptUnreadable(_)
                        | ImportOutcome::CorruptTransfer
                        | ImportOutcome::PermissionDenied(_)
                )
            })
            .count()
    }
}
