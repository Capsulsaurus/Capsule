//! One `capsule import` run with nothing printed: the import the TUI's import screen runs.
//!
//! It is the same pipeline the subcommand drives — the source read (the crate's
//! `read_import_source`: the filesystem scanner, or a provider's source adapter), the pure planner, and the one executor — over a workspace the caller opened,
//! and it ends in the same [`ImportReport`] `capsule import --json` prints. It imports in copy
//! mode only: a move deletes sources once the server holds them, which belongs to
//! `capsule import --move --push`.

use std::path::PathBuf;

use capsule_core::domain::ImportMode;
use capsule_library::import::{
    CancellationToken, DefaultAlbumContext, ImportConfig, execute_with_source_metadata, plan,
};
use capsule_library::lifecycle::Workspace;
use eyre::{Result, eyre};

use crate::cli::ImportProviderArg;
use crate::import_report::{self, ImportReport};

/// Import `paths`, read as `provider`'s export (or as plain files and folders), into
/// `workspace`, copying every source, and report every file's outcome.
///
/// A run with nothing new to import reports the plan's own skips (duplicates, unreadable
/// sources), exactly as `capsule import --json` does, and writes nothing.
///
/// # Errors
///
/// The sources could not be read, planned or executed — the cases that fail `capsule import`.
/// One unreadable file is not one of them: it is that file's outcome in the report.
pub(crate) fn import_sources(
    workspace: &mut Workspace,
    provider: Option<ImportProviderArg>,
    paths: &[PathBuf],
) -> Result<ImportReport> {
    tracing::info!(sources = paths.len(), ?provider, "import: starting");
    let (scan_result, source_metadata) = crate::read_import_source(provider, paths, false)?;
    let (candidates, files) = (scan_result.candidates.len(), scan_result.total_files());
    let config = ImportConfig {
        import_mode: ImportMode::Copy,
        force_reimport_duplicates: false,
        album: DefaultAlbumContext::derived(workspace.default_album_id()),
        ..Default::default()
    };
    let plan_result = plan(&scan_result, workspace.db(), &config)
        .map_err(|e| eyre!("Planning the import failed: {e}"))?;
    let counts = &plan_result.counts;
    tracing::debug!(
        candidates,
        files,
        to_import = counts.to_import,
        duplicates = counts.duplicates,
        errors = counts.errors,
        "import: planned"
    );
    let report = if counts.to_import == 0 {
        let skipped = import_report::skipped_by_plan(&plan_result);
        ImportReport::new(candidates, files, counts, &skipped)
    } else {
        let summary = execute_with_source_metadata(
            &plan_result,
            workspace,
            &config,
            &source_metadata,
            |_| {},
            &CancellationToken::new(),
        )
        .map_err(|e| eyre!("The import failed: {e}"))?;
        ImportReport::new(candidates, files, counts, &summary)
    };
    tracing::info!(
        imported = report.imported,
        duplicates = report.duplicates,
        errors = report.errors,
        "import: finished"
    );
    Ok(report)
}

#[cfg(test)]
mod tests;
