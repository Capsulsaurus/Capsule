use std::path::PathBuf;

/// Outcome for a single imported file.
#[derive(Debug, Clone)]
pub enum ImportOutcome {
    Imported,
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
            .filter(|(_, o)| matches!(o, ImportOutcome::Imported))
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
