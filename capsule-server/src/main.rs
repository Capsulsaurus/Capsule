//! The `capsule-server` binary: a thin shim over the [`capsule_server`] library, which owns the
//! subcommand implementations.
//!
//! This binary installs error reporting and dispatches, in the shape `capsule-cli/src/main.rs`
//! already has. Everything else — parsing, the log stream, the configuration, the composition
//! root and each subcommand's body — is in the library, so `tests/binary.rs` asserts against the
//! same code this runs and a unit test can drive `boot::assemble` without a subprocess.
//!
//! Replaces the `gen_openapi` `[[bin]]`, which was this crate's only executable: the document
//! dump is now `capsule-server gen-openapi`, one subcommand of the one binary. See
//! [`capsule_server::cli`] for why four executables were rejected.

use color_eyre::eyre::Result;

#[tokio::main]
async fn main() -> Result<std::process::ExitCode> {
    color_eyre::install()?;
    capsule_server::cli::run().await
}
