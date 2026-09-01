//! Deterministic OpenAPI **3.1** schema dump (slice `S-D8`).
//!
//! Serializes the server's salvo-oapi document to `capsule-sdk/openapi.json` — the committed
//! source-of-truth the SDK's `spargen` build step generates the typed REST client from. It
//! builds [`capsule_api::openapi_router`], the state-free mirror of the live router, so the
//! dump needs **no** database, Valkey, key material, disk, or network and is byte-stable
//! across runs. That is what lets `--check` run in the Rust check gate (`openapi-check`) as a
//! drift guard, exactly mirroring `i18n` / `i18n-check`.
//!
//! Usage:
//! - `gen_openapi [FILE]` writes the schema (default `capsule-sdk/openapi.json`).
//! - `gen_openapi --check [FILE]` fails if the committed schema is stale, writing nothing.

use std::path::PathBuf;

use capsule_api::{create_openapi_spec, openapi_router};
use clap::Parser;
use color_eyre::eyre::{Context, Result, bail};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Output path for the OpenAPI 3.1 schema (relative to the repo root).
    #[arg(value_name = "FILE", default_value = "capsule-sdk/openapi.json")]
    output: PathBuf,

    /// Verify the committed schema is up to date instead of writing it (CI drift gate).
    #[arg(long)]
    check: bool,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    // Merge the state-free route tree into the base document. No I/O, no live services.
    let doc = create_openapi_spec().merge_router(&openapi_router());
    let mut json =
        serde_json::to_string_pretty(&doc).wrap_err("serializing the OpenAPI document to JSON")?;
    json.push('\n');

    if cli.check {
        let committed = std::fs::read_to_string(&cli.output).wrap_err_with(|| {
            format!("cannot read committed schema at {}", cli.output.display())
        })?;
        if committed != json {
            bail!(
                "OpenAPI schema at {} is out of sync with the server; run `mise run openapi` and \
                 commit the result",
                cli.output.display()
            );
        }
        println!("OpenAPI schema is up to date: {}", cli.output.display());
    } else {
        if let Some(parent) = cli.output.parent() {
            std::fs::create_dir_all(parent)
                .wrap_err_with(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&cli.output, &json)
            .wrap_err_with(|| format!("writing {}", cli.output.display()))?;
        println!("Wrote OpenAPI 3.1 schema: {}", cli.output.display());
    }

    Ok(())
}
