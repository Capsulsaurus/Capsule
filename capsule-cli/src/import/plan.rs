// Stubs — Phase 9 will replace these with calls to capsule_core::import::{scan, plan, execute}.

use std::path::PathBuf;

use eyre::{Result, eyre};
use capsule_core::import::ImportActionPlan;
use capsule_core::import::scanner::scan;

/// Scan a file or directory and build an import action plan.
///
/// Phase 9 will replace this with proper scanner → planner → executor wiring.
#[allow(dead_code)]
pub async fn create_import_plan(source: PathBuf) -> Result<ImportActionPlan> {
    let paths = vec![source];
    let _scan_result = scan(&paths).map_err(|e| eyre!("scan failed: {e}"))?;

    let db = crate::db::init_sqlite().await?;
    // The db module uses SeaORM; capsule_core uses rusqlite. For Phase 9, the CLI will
    // open a capsule_core Library directly. For now, return an empty plan stub.
    let _ = db;

    Err(eyre!("not yet implemented — use Phase 9 CLI commands"))
}

/// Execute an import plan (stub).
#[allow(dead_code)]
pub fn plan_summary(plan: &ImportActionPlan) -> String {
    let to_import = plan.counts.to_import;
    let dups = plan.counts.duplicates;
    let skip = plan.counts.unsupported + plan.counts.errors;
    format!("to_import={to_import} duplicates={dups} skip={skip}")
}
