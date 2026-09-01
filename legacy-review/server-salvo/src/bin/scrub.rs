//! `capsule-scrub` — the operator-invokable server-side integrity scrub (slice `S-C14`).
//!
//! One shot, no daemon: a binary an operator crons (or a scheduled job runs) to verify a
//! frozen (or live-quiesced) Postgres index against its content-addressed blob store. It is
//! **read-only by design** — it classifies and reports, and mutates nothing, so it can never
//! itself become the deletion bug it exists to catch (repair stays with the GC path, the
//! index rebuild, and operator action).
//!
//! It runs the six maintenance-doc checks (row⇄blob presence, deep byte re-hash, custody
//! chain agreement, mirrored-fact agreement, debris/quarantine inventory), logs every finding
//! structured, prints the per-class counts, and **exits non-zero when any finding is present**
//! — the signal an operator alerts on. `--deep` adds the heavy per-blob re-hash.
//!
//! The scrub logic lives in `capsule-api-service::scrub`; this binary is only the CLI +
//! wiring. Configuration mirrors the server's own: `DATABASE_URL` and `UPLOAD_DIR`.

use std::path::PathBuf;

use clap::Parser;
use eyre::{Result, eyre};
use sea_orm::Database;
use service::scrub::{FindingClass, IntegrityScrub};
use tracing::{error, info};

/// The integrity-scrub CLI. Reads the database URL from `DATABASE_URL` and the blob store root
/// from `UPLOAD_DIR` (default `./uploads`), matching the server's own configuration.
#[derive(Debug, Parser)]
#[command(
    name = "capsule-scrub",
    about = "Capsule read-only server integrity scrub (Postgres⇄blob-store) (S-C14)"
)]
struct Args {
    /// Also re-hash every blob's bytes (the heavy byte-integrity / bit-rot check). Off by
    /// default — it is rolling, throttled I/O, like client content validation.
    #[arg(long)]
    deep: bool,
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

    info!(
        deep = args.deep,
        upload_dir = %upload_dir.display(),
        "capsule-scrub: starting read-only integrity scrub"
    );

    let report = IntegrityScrub::new(upload_dir).run(&db, args.deep).await?;

    // Roll-up to stdout for the operator (findings are also logged structured, one per line).
    info!(
        total = report.total(),
        scanned_blobs = report.scanned_blobs,
        scanned_references = report.scanned_references,
        deep = report.deep,
        dangling_reference = report.count(FindingClass::DanglingReference),
        orphan_blob = report.count(FindingClass::OrphanBlob),
        corrupt_blob = report.count(FindingClass::CorruptBlob),
        chain_break = report.count(FindingClass::ChainBreak),
        mirrored_fact_mismatch = report.count(FindingClass::MirroredFactMismatch),
        incoming_debris = report.count(FindingClass::IncomingDebris),
        quarantine = report.count(FindingClass::Quarantine),
        "capsule-scrub: integrity scrub finished"
    );

    if report.is_clean() {
        info!("capsule-scrub: clean — no integrity findings");
        Ok(())
    } else {
        // A non-zero finding count is the operator's alert signal. The scrub mutated nothing;
        // repair is the operator's decision via the paths that own it.
        error!(
            total = report.total(),
            "capsule-scrub: INTEGRITY FINDINGS present — see structured findings above; exiting non-zero"
        );
        std::process::exit(1);
    }
}
