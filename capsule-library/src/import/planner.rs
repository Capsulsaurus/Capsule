use std::path::Path;

use capsule_core::domain::ImportMode;
use capsule_core::import::default_album::{
    DefaultAlbumContext, ResolvedAlbum, resolve_default_album,
};
use capsule_core::import::scan::{ImportCandidate, ScanResult};
use capsule_core::import::upload::{
    StagedStreamingConflict, UploadPolicy, ensure_streaming_compatible,
};

use crate::db::DatabaseDriver;
use crate::library::{LibraryError, available_bytes};

/// Configuration for an import run.
#[derive(Debug, Clone)]
pub struct ImportConfig {
    pub import_mode: ImportMode,
    /// Where this import should land, as the **inputs to the resolution order** rather than
    /// a raw destination id (SSoT: organization § The Default Album). The planner runs
    /// [`resolve_default_album`] over it and records the fired rule on the plan; the
    /// executor binds the library's derived de facto album (rule 3) before running the same
    /// single resolution. Default is unbound — "the user filed this nowhere".
    pub album: DefaultAlbumContext,
    /// If true, import even if a file with the same SHA-256 hash already exists.
    pub force_reimport_duplicates: bool,
    /// The per-device upload policy (staged uploads). `Full` (the
    /// default) opens every bundle session eagerly; `Staged` opens them in tier
    /// order gated by the connection class, and is **mutually exclusive** with a
    /// streaming import — the planner rejects the combination at confirmation
    /// ([`ImportActionPlan::confirm_upload_policy`]) and the streaming executor
    /// refuses it by construction.
    pub upload_policy: UploadPolicy,
}

impl Default for ImportConfig {
    fn default() -> Self {
        Self {
            import_mode: ImportMode::Copy,
            album: DefaultAlbumContext::default(),
            force_reimport_duplicates: false,
            upload_policy: UploadPolicy::Full,
        }
    }
}

/// Decision for a single candidate.
#[derive(Debug, Clone)]
pub enum ImportDecision {
    Import,
    SkipDuplicate { existing_uuid: String },
    SkipError(String),
}

/// What the plan decided, counted. There is no "unsupported" bucket, by design: a file is
/// imported, already held, or could not be read. Neither its format nor whether this build has
/// a codec for it can keep it out of the library (issue #695).
#[derive(Debug, Default, Clone)]
pub struct PlanCounts {
    pub to_import: usize,
    pub duplicates: usize,
    pub errors: usize,
    /// Total bytes the plan will bring into the library: the summed on-disk size of every
    /// source file of every `Import` candidate (skips contribute nothing). This is the
    /// `total_size` the confirmation UI shows and the free-space probe compares against to set
    /// [`ImportActionPlan::streaming_recommended`]. Missing/unreadable sizes count as zero — the
    /// same file already reports as a `SkipError`, so it is not double-charged as import bytes.
    pub total_size: u64,
}

/// Output of Phase 2 (plan).
#[derive(Debug)]
pub struct ImportActionPlan {
    pub actions: Vec<(ImportCandidate, ImportDecision)>,
    pub counts: PlanCounts,
    /// The free-space probe's streaming recommendation, attached **at confirmation** by
    /// [`attach_streaming_recommendation`](Self::attach_streaming_recommendation) — never by the
    /// pure planner (the probe is I/O). `None` until attached; `Some(true)` means the library
    /// volume is near/over full for this plan's `total_size` and the user should confirm a
    /// [streaming import](crate::import::execute_streaming). Recording it on the plan (like the resolved
    /// destination `album_id`) keeps the planner deterministic.
    pub streaming_recommended: Option<bool>,
    /// The destination album and **which resolution rule chose it** (SSoT: organization
    /// § The Default Album), so a surprising destination is explainable after the fact.
    ///
    /// `None` when [`ImportConfig::album`] was still unbound at plan time — the library's
    /// derived de facto album (rule 3) is the workspace's to supply, and the executor binds
    /// it and runs the same [`resolve_default_album`] before writing anything. There is one
    /// resolution policy, never two.
    pub album: Option<ResolvedAlbum>,
}

impl ImportActionPlan {
    /// Per-`Import`-candidate source-byte sizes, largest first — the input to the streaming
    /// minimum-headroom check (the largest single asset must fully materialize locally). Skip
    /// decisions contribute nothing.
    pub fn import_candidate_sizes(&self) -> Vec<u64> {
        let mut sizes: Vec<u64> = self
            .actions
            .iter()
            .filter(|(_, d)| matches!(d, ImportDecision::Import))
            .map(|(c, _)| candidate_size(c))
            .collect();
        sizes.sort_unstable_by(|a, b| b.cmp(a));
        sizes
    }

    /// The largest single `Import` candidate's size, or `0` when nothing will be imported.
    pub fn largest_import_size(&self) -> u64 {
        self.import_candidate_sizes().into_iter().max().unwrap_or(0)
    }

    /// Attach the free-space probe's streaming recommendation **at confirmation**: probe
    /// `library_root`'s available bytes (I/O — never inside the pure planner) and set
    /// [`streaming_recommended`](Self::streaming_recommended) from the pure
    /// [`streaming_recommended`](crate::library::streaming_recommended) predicate over this
    /// plan's `total_size`. Returns the attached verdict.
    #[tracing::instrument(skip_all, fields(total_size = self.counts.total_size))]
    pub fn attach_streaming_recommendation(
        &mut self,
        library_root: &Path,
        headroom_margin: u64,
    ) -> Result<bool, LibraryError> {
        let available = available_bytes(library_root)?;
        Ok(self.set_streaming_recommendation(available, headroom_margin))
    }

    /// The pure half of [`attach_streaming_recommendation`](Self::attach_streaming_recommendation),
    /// taking an already-probed `available` so the auto-detect verdict is unit-testable without
    /// touching a real volume. Sets and returns [`streaming_recommended`](Self::streaming_recommended).
    pub fn set_streaming_recommendation(&mut self, available: u64, headroom_margin: u64) -> bool {
        let recommended = crate::library::streaming_recommended(
            self.counts.total_size,
            available,
            headroom_margin,
        );
        self.streaming_recommended = Some(recommended);
        recommended
    }

    /// **Confirmation-time upload-policy gate** (staged uploads).
    ///
    /// A [`UploadPolicy::Staged`] run and a streaming import are mutually exclusive
    /// per import (transfer upload doc): streaming exists to release local bytes
    /// quickly, staged defers exactly the T2 upload release depends on. This is the
    /// planner's single rejection point — call it at confirmation with the run's
    /// `policy` and whether a streaming import was chosen (`use_streaming`); a
    /// conflicting combination returns [`StagedStreamingConflict`] instead of ever
    /// entering the executor. Delegates to the pure [`ensure_streaming_compatible`]
    /// invariant so the rule lives in one place.
    pub fn confirm_upload_policy(
        &self,
        policy: UploadPolicy,
        use_streaming: bool,
    ) -> Result<(), StagedStreamingConflict> {
        ensure_streaming_compatible(policy, use_streaming)
    }
}

/// Phase 2 — decide what to do with each candidate from the scan.
///
/// SHA-256-hashes the primary member of each candidate and checks the DB for
/// duplicates. Returns an `ImportActionPlan` with per-candidate decisions.
pub fn plan(
    scan: &ScanResult,
    db: &DatabaseDriver,
    config: &ImportConfig,
) -> Result<ImportActionPlan, Box<dyn std::error::Error + Send + Sync>> {
    // Resolve the destination album before hashing anything, so a surprising destination is
    // explainable from the plan alone. An unbound context is not an error here: the executor
    // binds the library's derived de facto album and runs the same resolution.
    let album = resolve_default_album(&config.album).ok();

    let mut actions = Vec::new();
    let mut counts = PlanCounts::default();

    for candidate in &scan.candidates {
        let decision = decide(candidate, db, config)?;
        match &decision {
            ImportDecision::Import => {
                counts.to_import += 1;
                // total_size accumulates only what will actually be imported.
                counts.total_size = counts.total_size.saturating_add(candidate_size(candidate));
            }
            ImportDecision::SkipDuplicate { .. } => counts.duplicates += 1,
            ImportDecision::SkipError(_) => counts.errors += 1,
        }
        actions.push((candidate.clone(), decision));
    }

    Ok(ImportActionPlan {
        actions,
        counts,
        streaming_recommended: None,
        album,
    })
}

/// The summed on-disk size of every source file in `candidate`. A file whose metadata cannot be
/// read counts as zero — an unreadable file is already surfaced as a `SkipError`, so it never
/// inflates the import byte total. Shared with the [streaming executor](crate::import::streaming),
/// which names the largest candidate for the minimum-headroom error.
pub(crate) fn candidate_size(candidate: &ImportCandidate) -> u64 {
    candidate
        .source_paths
        .iter()
        .map(|p| std::fs::metadata(p).map_or(0, |m| m.len()))
        .sum()
}

fn decide(
    candidate: &ImportCandidate,
    db: &DatabaseDriver,
    config: &ImportConfig,
) -> Result<ImportDecision, Box<dyn std::error::Error + Send + Sync>> {
    // Hash the primary file (first member with Primary role, or source_paths[0])
    let primary_path = candidate.primary_path();

    let hash = match hash_file(primary_path) {
        Ok(h) => h,
        Err(e) => {
            return Ok(ImportDecision::SkipError(format!(
                "failed to hash {}: {e}",
                primary_path.display()
            )));
        }
    };

    if !config.force_reimport_duplicates
        && let Some(existing) = db.find_by_hash(&hash)?
    {
        return Ok(ImportDecision::SkipDuplicate {
            existing_uuid: existing.uuid,
        });
    }

    Ok(ImportDecision::Import)
}

fn hash_file(path: &Path) -> Result<String, std::io::Error> {
    Ok(crate::utils::hash_file(path)?.to_hex())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
