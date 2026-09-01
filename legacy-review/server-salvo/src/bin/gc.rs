//! `capsule-gc` — the operator-invokable garbage-collection worker (slice `S-C11`).
//!
//! One shot, no daemon: a binary an operator crons. It runs the keyless **retention purge**
//! (hard-purge soft-deleted assets whose signed `retention_until` has elapsed) and then the
//! two-phase **refcount mark-and-sweep** (reclaim unreferenced blob bytes past the GC grace
//! window, including finalization-crash orphans) over one deployment's blob store. `--dry-run`
//! reports what *would* be reclaimed without deleting anything.
//!
//! Both phases live in `capsule-api-service::gc`; this binary is only the CLI + wiring. Time
//! comes from the server's trusted system clock.

use std::path::PathBuf;

use clap::Parser;
use eyre::{Result, eyre};
use sea_orm::Database;
use service::gc::{GcWorker, RetentionPurgeWorker};
use tracing::info;

/// The GC worker CLI. Reads the database URL from `DATABASE_URL` and the blob store root from
/// `UPLOAD_DIR` (default `./uploads`), matching the server's own configuration.
#[derive(Debug, Parser)]
#[command(
    name = "capsule-gc",
    about = "Capsule blob GC + retention purge worker (S-C11)"
)]
struct Args {
    /// Report what would be reclaimed without deleting any bytes or rows.
    #[arg(long)]
    dry_run: bool,

    /// Run only the retention purge phase (skip the blob mark-and-sweep).
    #[arg(long, conflicts_with = "mark_sweep_only")]
    retention_only: bool,

    /// Run only the blob mark-and-sweep phase (skip the retention purge).
    #[arg(long, conflicts_with = "retention_only")]
    mark_sweep_only: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let database_url = std::env::var("DATABASE_URL")
        .map_err(|_| eyre!("DATABASE_URL must be set to the deployment's Postgres URL"))?;
    let upload_dir: PathBuf = std::env::var("UPLOAD_DIR")
        .unwrap_or_else(|_| "./uploads".to_string())
        .into();

    let db = Database::connect(&database_url).await?;

    if args.dry_run {
        info!("capsule-gc: DRY RUN — no bytes or rows will be deleted");
    }

    if !args.mark_sweep_only {
        let report = RetentionPurgeWorker::new()
            .purge_expired(&db, args.dry_run)
            .await?;
        info!(
            purged = report.purged.len(),
            refused = report.refused_in_window.len(),
            skipped = report.skipped_no_floor.len(),
            "capsule-gc: retention purge finished"
        );
    }

    if !args.retention_only {
        let report = GcWorker::new(upload_dir)
            .mark_and_sweep(&db, args.dry_run)
            .await?;
        info!(
            scanned = report.scanned,
            marked = report.marked,
            swept = report.swept,
            swept_bytes = report.swept_bytes,
            retained_in_grace = report.retained_in_grace,
            dangling = report.dangling_quarantined,
            "capsule-gc: mark-and-sweep finished"
        );
    }

    Ok(())
}
