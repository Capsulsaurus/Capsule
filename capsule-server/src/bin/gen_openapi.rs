//! Deterministic OpenAPI **3.2** document dump for the Kynos server (slice `S-C34`).
//!
//! Serializes [`capsule_server::openapi()`] to `capsule-server/openapi.json` and, with
//! `--check`, fails when the committed copy is stale. It is the drift guard for the rebuild's
//! central claim: that the description is derived from the types and cannot disagree with them.
//!
//! That claim is already enforced *inside* the crate — `assert_conformance` catches a response
//! the document did not predict, and `assert_declared_responses_covered` catches a promise no
//! test produced. Neither helps a **client**. A surface can be ported, the emitted document can
//! change shape, and nothing outside the crate notices until someone regenerates by hand. This
//! binary is what makes such a change fail.
//!
//! It needs no database, no Valkey, no key material, no disk and no network: `openapi()` builds
//! the router purely to describe it. That is what lets `--check` run in the Rust check gate,
//! exactly as `i18n-check` and the Salvo `openapi-check` do.
//!
//! **This is the SDK's contract** (`S-C59`). There was a second document — `capsule-sdk`
//! generated from `capsule-sdk/openapi.json`, the Salvo one — and the two were gated separately
//! while the port proceeded, because committing both as *the* contract at once would have left
//! no way to say which a client should believe. The Salvo copy is deleted and
//! `capsule-sdk/build.rs` reads what this binary writes. The changeover did drop the four
//! `spargen::OmitRule` narrowings that existed because the Salvo document was structurally
//! invalid in ways Kynos cannot express; the four in `build.rs` today are a different set, for
//! a media type spargen cannot classify.
//!
//! Usage:
//! - `gen_openapi [FILE]` writes the document (default `capsule-server/openapi.json`).
//! - `gen_openapi --check [FILE]` fails if the committed document is stale, writing nothing.

use std::path::PathBuf;

use clap::Parser;
use color_eyre::eyre::{Context, Result, bail};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Output path for the OpenAPI 3.2 document (relative to the repo root).
    #[arg(value_name = "FILE", default_value = "capsule-server/openapi.json")]
    output: PathBuf,

    /// Verify the committed document is up to date instead of writing it (CI drift gate).
    #[arg(long)]
    check: bool,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    let document = capsule_server::openapi()
        .map_err(|e| color_eyre::eyre::eyre!("describing the router: {e}"))?;
    // `to_json` is already pretty-printed; the trailing newline matches the Salvo dump so both
    // committed documents are ordinary text files rather than one-line blobs in a diff.
    let mut json = document
        .to_json()
        .wrap_err("serializing the OpenAPI document to JSON")?;
    json.push('\n');

    if cli.check {
        let committed = std::fs::read_to_string(&cli.output).wrap_err_with(|| {
            format!("cannot read committed document at {}", cli.output.display())
        })?;
        if committed != json {
            bail!(
                "OpenAPI document at {} is out of sync with the server; run \
                 `mise run openapi-kynos` and commit the result",
                cli.output.display()
            );
        }
        println!("OpenAPI document is up to date: {}", cli.output.display());
    } else {
        if let Some(parent) = cli.output.parent() {
            std::fs::create_dir_all(parent)
                .wrap_err_with(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&cli.output, &json)
            .wrap_err_with(|| format!("writing {}", cli.output.display()))?;
        println!("Wrote {}", cli.output.display());
    }

    Ok(())
}
