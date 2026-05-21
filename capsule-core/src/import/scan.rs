use std::path::PathBuf;

use crate::domain::{DetectionMethod, MemberRole, StackType};
use crate::metadata::AssetType;

/// One logical "unit" of import — either a standalone file or a set of
/// related files (RAW+JPEG pair, Live Photo HEIC+MOV, etc.).
#[derive(Debug, Clone)]
pub struct ImportCandidate {
    /// All source file paths belonging to this candidate.
    pub source_paths: Vec<PathBuf>,
    /// Detected media type (of the primary member).
    pub detected_type: AssetType,
    /// Stack type, present when the candidate forms a multi-file stack.
    pub stack_type: Option<StackType>,
    /// How the stack relationship was detected.
    pub detection_method: Option<DetectionMethod>,
    /// Unique key for this stack group within its source directory.
    pub detection_key: Option<String>,
    /// Ordered list of (path, member role) for every file in the candidate.
    pub members: Vec<(PathBuf, MemberRole)>,
}

impl ImportCandidate {
    /// Returns the primary path (first `Primary` role member, or `source_paths[0]`).
    pub fn primary_path(&self) -> &PathBuf {
        self.members
            .iter()
            .find(|(_, r)| *r == MemberRole::Primary)
            .map(|(p, _)| p)
            .unwrap_or(&self.source_paths[0])
    }
}

/// Output of Phase 1 (scan).
#[derive(Debug, Default)]
pub struct ScanResult {
    pub candidates: Vec<ImportCandidate>,
}

impl ScanResult {
    pub fn total_files(&self) -> usize {
        self.candidates.iter().map(|c| c.source_paths.len()).sum()
    }
}
