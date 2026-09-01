//! The Capsule CLI as a library: the command implementations and the dispatch the
//! thin `capsule` binary calls.
//!
//! The networked commands (`auth login/logout`, `sync`, `list` — slice `S-D5`)
//! live in [`remote`] and are driven entirely over `capsule-sdk`; [`session`] and
//! [`syncstore`] are their durable state. Splitting the logic into a library lets
//! the real-server round-trip drive these command functions directly rather than
//! spawning the binary.

use std::path::{Path, PathBuf};

use capitalize::Capitalize;
use capsule_core::crypto::primitives::DeviceTier;
use capsule_core::domain::ImportMode;
use capsule_core::import::scanner::scan as scan_files;
use capsule_core::import::upload::UploadPolicy;
use capsule_core::import::{
    CancellationToken, DefaultAlbumContext, ImportConfig, ImportOutcome, ImportProgressEvent,
    ScanResult, SourceAdapter, SourceMetadataIndex, TakeoutAdapter, execute_with_source_metadata,
    plan,
};
use capsule_core::library::{Library, LibraryError, init_library, open_library, rebuild_index};
use capsule_core::lifecycle::Workspace;
use capsule_core::metadata::FileMetadata;
use capsule_sdk::net::ConnectionClass;
use cli::{AuthCommands, Cli, Commands, ImportProviderArg, LibraryCommands};
use colored::*;
use dialoguer::{Confirm, Input, Password};
use eyre::{Result, eyre};

use crate::i18n::{Bundle, Value, keys};
use crate::utils::directories::{
    get_cache_dir, get_config_dir, get_data_dir, get_session_file_path,
};

pub mod cli;
pub mod cull;
pub mod db;
pub mod demo;
pub mod i18n;
pub mod remote;
pub mod session;
pub mod status;
pub mod syncstore;
pub mod utils;

/// The stable product id this CLI reports on every manifest it authors (S-D15). Combined with
/// this crate's own semver and capsule-core's build-embedded commit, it composes the
/// `client_version` grammar `capsule-cli/{semver}+{commit}[.dirty]`.
pub const CLIENT_ID: &str = "capsule-cli";

/// Tag a freshly built workspace with this CLI's build identity so every manifest and derivative
/// it produces carries `capsule-cli/{semver}+{commit}` (S-D15) rather than the bare
/// `capsule-core` default. Applied at every workspace construction site in the CLI.
#[must_use]
fn as_capsule_cli(ws: Workspace) -> Workspace {
    ws.with_client_id(CLIENT_ID, env!("CARGO_PKG_VERSION"))
}

/// Narrow a count for an ICU `{name}` argument. Counts here are bounded by the number of
/// files in one import, so the saturating arm is unreachable in practice — but it keeps the
/// conversion total rather than panicking on a value no user could produce.
#[must_use]
fn as_count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Resolve the persisted-session file path, erroring if the config directory
/// cannot be determined.
fn session_store() -> Result<session::SessionStore> {
    let path =
        get_session_file_path().ok_or_else(|| eyre!("Failed to resolve config directory"))?;
    Ok(session::SessionStore::new(path))
}

/// A localized, human-readable reason for a networked-command failure: the stable
/// `error.*` message when the error carries a code, else the English detail.
fn describe_remote_error(bundle: &capsule_i18n::Bundle, error: &remote::RemoteError) -> String {
    match error.error_code() {
        Some(code) => bundle.format(code, &[]),
        None => error.to_string(),
    }
}

/// Render an opaque feed id for display: UTF-8 when the bytes are valid text
/// (nanoids), else base64 (raw UUIDs).
fn display_id(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) if text.chars().all(|c| !c.is_control()) => text.to_string(),
        _ => {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(bytes)
        }
    }
}

/// Render the import's source paths for the one line that names them: a single path verbatim,
/// several parts comma-separated (a split export imported in one run).
fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Phase 1 of `capsule import`: turn the source paths into the [`ScanResult`] the pure planner
/// consumes, plus the folded exporter metadata the executor writes into each signed sidecar.
///
/// Both arms feed the *same* plan → execute path — there is one import implementation, not two.
/// Without `--provider` this is the filesystem scanner and an empty metadata index, exactly as
/// before. With `--provider takeout` the Google Takeout [source adapter] walks every named part
/// into one pool instead: it pairs each media file with its JSON sidecar (reconciling Google's
/// truncated names, `(1)` duplicate counters, `-edited` renditions, and sidecars that landed in
/// a different part), reads the per-album `metadata.json` manifests, and folds the exporter's
/// record under the pipeline's precedence rule before the planner sees anything. The planner and
/// executor are unchanged by the choice; only what they are fed differs.
///
/// [source adapter]: capsule_core::import::importers
fn read_import_source(
    bundle: &Bundle,
    provider: Option<ImportProviderArg>,
    paths: &[PathBuf],
) -> Result<(ScanResult, SourceMetadataIndex)> {
    match provider {
        None => {
            let scanned = scan_files(paths).map_err(|e| {
                eyre!(
                    "{}",
                    bundle.format(
                        keys::IMPORT_SCAN_FAILED,
                        &[("reason", Value::Str(&e.to_string()))],
                    )
                )
            })?;
            Ok((scanned, SourceMetadataIndex::empty()))
        }
        Some(provider @ ImportProviderArg::Takeout) => {
            println!(
                "{}",
                bundle
                    .format(
                        keys::IMPORT_PROVIDER_NOTICE,
                        &[("provider", Value::Str(provider.display_name()))],
                    )
                    .cyan()
            );
            let extracted = TakeoutAdapter::new().extract(paths).map_err(|e| {
                eyre!(
                    "{}",
                    bundle.format(
                        keys::IMPORT_EXTRACT_FAILED,
                        &[("reason", Value::Str(&e.to_string()))],
                    )
                )
            })?;
            let index = SourceMetadataIndex::from_extracted(&extracted);
            tracing::info!(
                provider = "takeout",
                parts = paths.len(),
                entries = extracted.entries.len(),
                covered_files = index.len(),
                "import: source adapter extraction complete"
            );
            Ok((extracted.to_scan_result(), index))
        }
    }
}

/// Parse the CLI arguments and dispatch the matching command.
pub async fn run() -> Result<()> {
    let cli = <Cli as clap::Parser>::parse();
    tracing::trace!("Parsed CLI arguments: {:#?}", cli);
    dispatch(cli).await
}

/// Dispatch an already-parsed `Cli`.
async fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        // ── Auth ──────────────────────────────────────────────────────────
        Commands::Auth { command } => match command {
            AuthCommands::Register {
                email,
                password_stdin,
            } => auth_register(email, password_stdin).await?,
            AuthCommands::Login {
                email,
                password_stdin,
            } => auth_login(email, password_stdin).await?,
            AuthCommands::Logout => auth_logout().await?,
            AuthCommands::Status => {
                println!("{}", "Checking authentication status...".blue());
                let store = session_store()?;
                match status::AuthStatus::check(&store) {
                    Ok(auth_status) => auth_status.display(),
                    Err(e) => println!("{}", format!("Error checking auth status: {e}").red()),
                }
            }
        },

        // ── Library ───────────────────────────────────────────────────────
        Commands::Library { command } => match command {
            LibraryCommands::Init { path, name } => {
                println!(
                    "{}",
                    format!("Creating library '{}' at {}...", name, path.display()).green()
                );
                let lib = init_library(&path, &name)
                    .map_err(|e| eyre!("Failed to create library: {e}"))?;
                println!(
                    "{}",
                    format!("Library created at {}", path.display()).green()
                );
                lib.close()
                    .map_err(|e| eyre!("Failed to close library: {e}"))?;
            }
            LibraryCommands::Info { path } => {
                let lib = open_library_or_err(&path)?;
                let cfg = lib.config();
                println!("{}", "Library info:".green());
                println!("  Name:            {}", cfg.library_name);
                println!("  Schema version:  {}", cfg.schema_version);
                println!("  Last opened:     {}", cfg.last_opened_at);
                println!(
                    "  Last scrubbed:   {}",
                    cfg.last_scrubbed_at
                        .map_or_else(|| "never".to_string(), |t| t.to_string())
                );
                lib.close()
                    .map_err(|e| eyre!("Failed to close library: {e}"))?;
            }
            LibraryCommands::Rebuild { path } => {
                println!(
                    "{}",
                    format!("Rebuilding index for {}...", path.display()).yellow()
                );
                let lib = open_library_or_err(&path)?;
                rebuild_index(&lib).map_err(|e| eyre!("Rebuild failed: {e}"))?;
                println!("{}", "Index rebuilt successfully.".green());
                lib.close()
                    .map_err(|e| eyre!("Failed to close library: {e}"))?;
            }
        },

        // ── Import ────────────────────────────────────────────────────────
        Commands::Import {
            paths,
            provider,
            library,
            r#move,
            force,
            passphrase_stdin,
            push,
            staged,
        } => {
            let bundle = i18n::cli_bundle();
            println!(
                "{}",
                bundle
                    .format(
                        keys::IMPORT_IN_PROGRESS,
                        &[
                            ("paths", Value::Str(&display_paths(&paths))),
                            ("library", Value::Str(&library.to_string_lossy())),
                        ],
                    )
                    .green()
            );

            let mut ws = open_workspace(&library, passphrase_stdin)?;

            // Phase 1: Scan (plain tree) or extract (third-party export, S-B11).
            println!("{}", bundle.format(keys::IMPORT_SCANNING, &[]).cyan());
            let (scan_result, source_metadata) = read_import_source(&bundle, provider, &paths)?;

            println!(
                "{}",
                bundle
                    .format(
                        keys::IMPORT_CANDIDATES_FOUND,
                        &[
                            (
                                "candidates",
                                Value::Int(as_count(scan_result.candidates.len()))
                            ),
                            ("files", Value::Int(as_count(scan_result.total_files()))),
                        ],
                    )
                    .green()
            );

            // Phase 2: Plan
            let config = ImportConfig {
                import_mode: if r#move {
                    ImportMode::Move
                } else {
                    ImportMode::Copy
                },
                force_reimport_duplicates: force,
                // No explicit pick and no owner pointer on the CLI yet: bind the library's
                // derived de facto album so the plan records the rule that fired (S-B12).
                album: DefaultAlbumContext::derived(ws.default_album_id()),
                ..Default::default()
            };

            let plan_result = plan(&scan_result, ws.db(), &config).map_err(|e| {
                eyre!(
                    "{}",
                    bundle.format(
                        keys::IMPORT_PLAN_FAILED,
                        &[("reason", Value::Str(&e.to_string()))],
                    )
                )
            })?;

            println!(
                "{}",
                bundle
                    .format(
                        keys::IMPORT_PLAN_SUMMARY,
                        &[
                            (
                                "to_import",
                                Value::Int(as_count(plan_result.counts.to_import))
                            ),
                            (
                                "duplicates",
                                Value::Int(as_count(plan_result.counts.duplicates)),
                            ),
                            (
                                "unsupported",
                                Value::Int(as_count(
                                    plan_result.counts.unsupported + plan_result.counts.errors,
                                )),
                            ),
                        ],
                    )
                    .cyan()
            );

            if plan_result.counts.to_import == 0 {
                println!(
                    "{}",
                    bundle.format(keys::IMPORT_NOTHING_TO_IMPORT, &[]).yellow()
                );
                // `--push` is still honored: an unchanged library is exactly the
                // re-runnable case push exists to make cheap.
                if push {
                    return push_workspace(&ws, push_options(false, false, staged)).await;
                }
                return Ok(());
            }

            // Phase 3: Execute
            println!("{}", bundle.format(keys::IMPORT_EXECUTING, &[]).cyan());
            let token = CancellationToken::new();

            // The one execute path: a plain filesystem import reaches it with an empty index
            // and behaves exactly as it always has; a `--provider` import reaches it with the
            // adapter's folded metadata, which lands inside each signed sidecar (`S-B10`).
            let summary = execute_with_source_metadata(
                &plan_result,
                &mut ws,
                &config,
                &source_metadata,
                |event| {
                    if let ImportProgressEvent::CandidateCompleted { outcomes, .. } = event {
                        for (path, outcome) in &outcomes {
                            let msg = format!("  {}", path.display());
                            match outcome {
                                ImportOutcome::Imported { .. } => {
                                    println!("{}", format!("✓ {msg}").green());
                                }
                                ImportOutcome::DuplicateSkipped { .. } => {
                                    println!(
                                        "{}",
                                        bundle
                                            .format(
                                                keys::IMPORT_OUTCOME_DUPLICATE,
                                                &[("path", Value::Str(&msg))],
                                            )
                                            .yellow()
                                    );
                                }
                                ImportOutcome::CorruptTransfer => {
                                    println!(
                                        "{}",
                                        bundle
                                            .format(
                                                keys::IMPORT_OUTCOME_CORRUPT_TRANSFER,
                                                &[("path", Value::Str(&msg))],
                                            )
                                            .red()
                                    );
                                }
                                ImportOutcome::CorruptUnreadable(e) => {
                                    println!(
                                        "{}",
                                        bundle
                                            .format(
                                                keys::IMPORT_OUTCOME_UNREADABLE,
                                                &[
                                                    ("path", Value::Str(&msg)),
                                                    ("reason", Value::Str(e)),
                                                ],
                                            )
                                            .red()
                                    );
                                }
                                _ => {
                                    println!("{}", format!("- {msg}").dimmed());
                                }
                            }
                        }
                    }
                },
                &token,
            )
            .map_err(|e| {
                eyre!(
                    "{}",
                    bundle.format(
                        keys::IMPORT_EXECUTE_FAILED,
                        &[("reason", Value::Str(&e.to_string()))],
                    )
                )
            })?;

            println!(
                "{}",
                bundle
                    .format(
                        keys::IMPORT_DONE,
                        &[
                            ("imported", Value::Int(as_count(summary.imported_count()))),
                            (
                                "duplicates",
                                Value::Int(as_count(summary.duplicate_count()))
                            ),
                            ("errors", Value::Int(as_count(summary.error_count()))),
                        ],
                    )
                    .green()
            );

            // `--push` is sugar only: import itself never touches the network (its
            // determinism suite depends on that), and the push is a separate pass over
            // the committed library — byte-for-byte the same one `capsule push` runs.
            if push {
                push_workspace(&ws, push_options(false, false, staged)).await?;
            }
        }

        // ── Push ──────────────────────────────────────────────────────────
        Commands::Push {
            library,
            passphrase_stdin,
            dry_run,
            force,
            staged,
        } => {
            let ws = open_workspace(&library, passphrase_stdin)?;
            push_workspace(&ws, push_options(dry_run, force, staged)).await?;
        }

        // ── Cull ──────────────────────────────────────────────────────────
        Commands::Cull {
            library,
            passphrase_stdin,
            pick,
            neutral,
            reject,
            filter,
            sweep,
            retain_days,
        } => {
            let request = cull::CullRequest {
                pick,
                neutral,
                reject,
                filter: filter.map(Into::into),
                sweep,
                retain_days,
            };
            let mut ws = open_workspace(&library, passphrase_stdin)?;
            let bundle = i18n::cli_bundle();
            match cull::apply(&mut ws, &request) {
                Ok(summary) => cull::render(&bundle, &request, &summary),
                Err(error) => {
                    let reason = cull::describe_error(&bundle, &error);
                    return Err(eyre!(
                        "{}",
                        bundle.format(keys::CULL_FAILED, &[("reason", Value::Str(&reason))])
                    ));
                }
            }
        }

        // ── Demo ──────────────────────────────────────────────────────────
        Commands::Demo { workdir, image } => {
            demo::run(workdir, image)?;
        }

        // ── Sync ──────────────────────────────────────────────────────────
        Commands::Sync { force, dry_run } => sync(dry_run, force).await?,

        // ── Status ────────────────────────────────────────────────────────
        Commands::Status => {
            println!("{}", "Checking Capsule status...".blue());
            match status::StatusInfo::collect().await {
                Ok(status_info) => status_info.display(),
                Err(e) => println!("{}", format!("Error collecting status: {e}").red()),
            }
        }

        // ── List ──────────────────────────────────────────────────────────
        Commands::List { include_deleted } => list(include_deleted).await?,

        // ── Match ─────────────────────────────────────────────────────────
        Commands::Match { path } => {
            println!(
                "{}",
                format!(
                    "Matching metadata for file: {}",
                    path.to_string_lossy().blue()
                )
                .green()
            );

            if !path.exists() {
                return Err(eyre!("File does not exist: {}", path.to_string_lossy()));
            }
            if !path.is_file() {
                return Err(eyre!("Path is not a file: {}", path.to_string_lossy()));
            }

            match FileMetadata::from_file_path(&path).await {
                Ok(metadata) => {
                    println!("{}", "File metadata:".green());
                    println!("{metadata:#?}");
                }
                Err(e) => return Err(eyre!("Failed to get file metadata: {}", e)),
            }
        }

        // ── Reset ─────────────────────────────────────────────────────────
        Commands::Reset {
            config,
            data,
            cache,
            all,
        } => {
            if !config && !data && !cache && !all {
                return Err(eyre!(
                    "No directories specified for reset. Use --all or specify at least one of \
                     --config, --data, --cache."
                ));
            }

            println!("{}", "Resetting all local CLI data...".red());
            let config_dir = get_config_dir().ok_or(eyre!("Failed to get config directory"))?;
            let data_dir = get_data_dir().ok_or(eyre!("Failed to get data directory"))?;
            let cache_dir = get_cache_dir().ok_or(eyre!("Failed to get cache directory"))?;

            let mut paths_to_remove = Vec::new();
            if all {
                paths_to_remove.push(("config", config_dir));
                paths_to_remove.push(("data", data_dir));
                paths_to_remove.push(("cache", cache_dir));
            } else {
                if config {
                    paths_to_remove.push(("config", config_dir));
                }
                if data {
                    paths_to_remove.push(("data", data_dir));
                }
                if cache {
                    paths_to_remove.push(("cache", cache_dir));
                }
            }

            for (label, path) in paths_to_remove {
                if path.exists() {
                    assert!(
                        path.is_dir(),
                        "Path {} is not a directory",
                        path.to_string_lossy()
                    );
                    let prompt = format!(
                        "Are you sure you want to delete the {} directory?\n  Path: {}",
                        label,
                        path.to_string_lossy()
                    );
                    if Confirm::new()
                        .with_prompt(&prompt)
                        .default(false)
                        .interact()?
                    {
                        println!("{}", format!("Removing {label} directory...").yellow());
                        match std::fs::remove_dir_all(&path) {
                            Ok(()) => println!(
                                "{}",
                                format!("Successfully removed {label} directory").green()
                            ),
                            Err(e) => println!(
                                "{}",
                                format!("Failed to remove {label} directory: {e}").red()
                            ),
                        }
                    } else {
                        println!("{}", format!("Skipping {label} directory").cyan());
                    }
                } else {
                    println!(
                        "{}",
                        format!(
                            "{} directory {} does not exist, skipping...",
                            label.capitalize_first_only(),
                            path.to_string_lossy()
                        )
                        .yellow()
                    );
                }
            }
        }
    }

    Ok(())
}

// ─── Networked commands (S-D5) ───────────────────────────────────────────────

/// Read a value from a flag, else prompt for it. Prompting needs a terminal, so the flag is
/// what makes these commands usable from a script, a CI job, or a heredoc.
fn flag_or_prompt(value: Option<String>, prompt: String) -> Result<String> {
    match value {
        Some(v) => Ok(v),
        None => Input::new()
            .with_prompt(prompt)
            .interact_text()
            .map_err(|e| eyre!("Failed to read input: {e}")),
    }
}

/// Read the password from stdin (`--password-stdin`) or prompt for it.
///
/// The stdin form takes the first line and trims the trailing newline only, so a password
/// containing spaces survives; it is the same convention `docker login --password-stdin` uses.
fn read_password(from_stdin: bool, prompt: String) -> Result<String> {
    if from_stdin {
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line)
            .map_err(|e| eyre!("Failed to read password from stdin: {e}"))?;
        let password = line.trim_end_matches(['\n', '\r']).to_string();
        if password.is_empty() {
            return Err(eyre!("--password-stdin was given but stdin was empty"));
        }
        return Ok(password);
    }
    Password::new()
        .with_prompt(prompt)
        .interact()
        .map_err(|e| eyre!("Failed to read password: {e}"))
}

/// `capsule auth register`: create an account over the SDK and persist the session.
async fn auth_register(email: Option<String>, password_stdin: bool) -> Result<()> {
    let bundle = i18n::cli_bundle();
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;

    // An address and a password, and nothing else. The server takes nothing else (`S-C53`): a
    // display name is a fact about a person, and the profile surface that would hold one is
    // owed rather than assumed. Prompting for a value the server discards would be worse than
    // not asking.
    let email = flag_or_prompt(email, bundle.format(keys::AUTH_LOGIN_EMAIL_PROMPT, &[]))?;
    let password = read_password(
        password_stdin,
        bundle.format(keys::AUTH_LOGIN_PASSWORD_PROMPT, &[]),
    )?;

    println!(
        "{}",
        bundle.format(keys::AUTH_REGISTER_IN_PROGRESS, &[]).green()
    );
    match remote::auth_register(&remote, &store, &email, &password).await {
        Ok(()) => {
            println!(
                "{}",
                bundle
                    .format(
                        keys::AUTH_REGISTER_SUCCESS,
                        &[("email", Value::Str(&email))]
                    )
                    .green()
            );
            Ok(())
        }
        Err(error) => {
            let reason = describe_remote_error(&bundle, &error);
            Err(eyre!(
                "{}",
                bundle.format(
                    keys::AUTH_REGISTER_FAILED,
                    &[("reason", Value::Str(&reason))]
                )
            ))
        }
    }
}

/// `capsule auth login`: read credentials from flags or prompts, authenticate over the SDK,
/// and persist the session.
async fn auth_login(email: Option<String>, password_stdin: bool) -> Result<()> {
    let bundle = i18n::cli_bundle();
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;

    let email = flag_or_prompt(email, bundle.format(keys::AUTH_LOGIN_EMAIL_PROMPT, &[]))?;
    let password = read_password(
        password_stdin,
        bundle.format(keys::AUTH_LOGIN_PASSWORD_PROMPT, &[]),
    )?;

    println!(
        "{}",
        bundle.format(keys::AUTH_LOGIN_IN_PROGRESS, &[]).green()
    );
    let step = match remote::auth_login(&remote, &store, &email, &password).await {
        Ok(step) => step,
        Err(error) => {
            let reason = describe_remote_error(&bundle, &error);
            return Err(eyre!(
                "{}",
                bundle.format(keys::AUTH_LOGIN_FAILED, &[("reason", Value::Str(&reason))])
            ));
        }
    };

    // The password verified and the sign-in may not be finished (`S-C55`). The CLI is
    // interactive, so it asks — a client that could not would have to report a second factor as
    // a failure, which is what the SDK's `LoginOutcome::into_session` is for and what this is
    // deliberately not.
    if let remote::LoginStep::SecondFactorRequired { mfa_token } = step {
        println!(
            "{}",
            bundle
                .format(keys::AUTH_LOGIN_SECOND_FACTOR_REQUIRED, &[])
                .yellow()
        );
        let code = flag_or_prompt(None, bundle.format(keys::AUTH_LOGIN_TOTP_PROMPT, &[]))?;
        if let Err(error) = remote::auth_verify_totp(&remote, &store, &mfa_token, code.trim()).await
        {
            let reason = describe_remote_error(&bundle, &error);
            return Err(eyre!(
                "{}",
                bundle.format(keys::AUTH_LOGIN_FAILED, &[("reason", Value::Str(&reason))])
            ));
        }
    }

    println!(
        "{}",
        bundle
            .format(keys::AUTH_LOGIN_SUCCESS, &[("email", Value::Str(&email))])
            .green()
    );
    Ok(())
}

/// `capsule auth logout`: revoke the session over the SDK and clear local state.
async fn auth_logout() -> Result<()> {
    let bundle = i18n::cli_bundle();
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;

    println!(
        "{}",
        bundle.format(keys::AUTH_LOGOUT_IN_PROGRESS, &[]).yellow()
    );
    match remote::auth_logout(&remote, &store).await {
        Ok(true) => {
            println!("{}", bundle.format(keys::AUTH_LOGOUT_SUCCESS, &[]).green());
            Ok(())
        }
        Ok(false) => {
            println!(
                "{}",
                bundle.format(keys::AUTH_LOGOUT_NOT_LOGGED_IN, &[]).yellow()
            );
            Ok(())
        }
        Err(error) => Err(eyre!("{}", describe_remote_error(&bundle, &error))),
    }
}

/// `capsule sync`: drain the feed over the SDK into the local store.
async fn sync(dry_run: bool, from_start: bool) -> Result<()> {
    let bundle = i18n::cli_bundle();
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let db = db::init_sqlite()
        .await
        .map_err(|e| eyre!("Failed to open local database: {e}"))?;

    println!(
        "{}",
        bundle
            .format(
                keys::SYNC_IN_PROGRESS,
                &[("endpoint", Value::Str(&remote.sync_endpoint))],
            )
            .green()
    );
    if dry_run {
        println!("{}", bundle.format(keys::SYNC_DRY_RUN_NOTICE, &[]).yellow());
    }

    match remote::sync(
        &remote,
        &store,
        &db,
        remote::DEFAULT_SYNC_PAGE_SIZE,
        dry_run,
        from_start,
    )
    .await
    {
        Ok(summary) => {
            if summary.applied == 0 {
                println!("{}", bundle.format(keys::SYNC_UP_TO_DATE, &[]).cyan());
            } else {
                println!(
                    "{}",
                    bundle
                        .format(
                            keys::SYNC_COMPLETE,
                            &[
                                ("applied", Value::Int(summary.applied as i64)),
                                ("albums", Value::Int(summary.albums as i64)),
                                ("pages", Value::Int(summary.pages as i64)),
                            ],
                        )
                        .green()
                );
            }
            Ok(())
        }
        Err(remote::RemoteError::NotAuthenticated) => Err(eyre!(
            "{}",
            bundle.format(keys::AUTH_NOT_AUTHENTICATED, &[])
        )),
        Err(error) => {
            let reason = describe_remote_error(&bundle, &error);
            Err(eyre!(
                "{}",
                bundle.format(keys::SYNC_FAILED, &[("reason", Value::Str(&reason))])
            ))
        }
    }
}

/// Open a Capsule library as a signed workspace.
///
/// Imports land on the signed lifecycle path (signed sidecar + manifest + provenance +
/// derivatives), never the legacy unsigned sidecar (S-B2); a first open initializes the account
/// under the given passphrase. The default album is resolved-or-created (`S-A10`): album keys
/// are durable now, so a second run MUST resolve the album the first run minted rather than
/// replacing it — minting a fresh one per run is exactly what left a reopened library unable to
/// decrypt or extend its own prior imports (and would make a push unrepeatable).
fn open_workspace(library: &Path, passphrase_stdin: bool) -> Result<Workspace> {
    let passphrase = read_password(passphrase_stdin, "Library passphrase".to_string())?;
    let mut ws = as_capsule_cli(
        Workspace::open(library, passphrase.as_bytes(), DeviceTier::Normal.params())
            .map_err(|e| eyre!("Failed to open signed workspace: {e}"))?,
    );
    let default_album = ws.default_album_id();
    ws.ensure_album(default_album, "Imports")
        .map_err(|e| eyre!("Failed to resolve the default album: {e}"))?;
    Ok(ws)
}

/// The push configuration behind the CLI's three push flags. A `--staged` run opens the tier
/// sessions in ladder order gated by the connection class; the default `Full` policy opens them
/// all eagerly. The CLI has no link-quality probe, so it reports the honest default —
/// `Unmetered`, the class a desktop/server run is on.
fn push_options(dry_run: bool, force: bool, staged: bool) -> remote::PushOptions {
    remote::PushOptions {
        dry_run,
        force,
        policy: if staged {
            UploadPolicy::Staged
        } else {
            UploadPolicy::Full
        },
        connection: ConnectionClass::Unmetered,
    }
}

/// `capsule push`: upload every managed asset of `workspace` over the SDK.
///
/// Re-runnable by construction: the outstanding work is derived from server truth (a sync-feed
/// pull), so a second run against an unchanged library uploads nothing.
async fn push_workspace(workspace: &Workspace, options: remote::PushOptions) -> Result<()> {
    let bundle = i18n::cli_bundle();
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let assets = workspace.asset_ids().len();

    if assets == 0 {
        println!(
            "{}",
            bundle.format(keys::PUSH_NOTHING_TO_PUSH, &[]).yellow()
        );
        return Ok(());
    }

    println!(
        "{}",
        bundle
            .format(
                keys::PUSH_IN_PROGRESS,
                &[
                    ("assets", Value::Int(assets as i64)),
                    ("endpoint", Value::Str(&remote.upload_endpoint)),
                ],
            )
            .green()
    );
    if options.dry_run {
        println!("{}", bundle.format(keys::PUSH_DRY_RUN_NOTICE, &[]).yellow());
    }
    if options.force {
        println!("{}", bundle.format(keys::PUSH_FORCE_NOTICE, &[]).yellow());
    }

    match remote::push(&remote, &store, workspace, options).await {
        Ok(summary) if summary.dry_run => {
            println!(
                "{}",
                bundle
                    .format(
                        keys::PUSH_DRY_RUN_COMPLETE,
                        &[
                            ("planned", Value::Int(summary.uploaded_blobs as i64)),
                            ("assets", Value::Int(summary.pushed_assets as i64)),
                            ("bytes", Value::Int(summary.bytes as i64)),
                        ],
                    )
                    .cyan()
            );
            Ok(())
        }
        Ok(summary) if summary.is_no_op() => {
            println!("{}", bundle.format(keys::PUSH_UP_TO_DATE, &[]).cyan());
            Ok(())
        }
        Ok(summary) => {
            println!(
                "{}",
                bundle
                    .format(
                        keys::PUSH_COMPLETE,
                        &[
                            ("uploaded", Value::Int(summary.uploaded_blobs as i64)),
                            ("merged", Value::Int(summary.merged_blobs as i64)),
                            ("held", Value::Int(summary.already_held_blobs as i64)),
                            ("deferred", Value::Int(summary.deferred_blobs as i64)),
                            ("bytes", Value::Int(summary.bytes as i64)),
                        ],
                    )
                    .green()
            );
            Ok(())
        }
        Err(remote::RemoteError::NotAuthenticated) => Err(eyre!(
            "{}",
            bundle.format(keys::AUTH_NOT_AUTHENTICATED, &[])
        )),
        Err(error) => {
            let reason = describe_remote_error(&bundle, &error);
            Err(eyre!(
                "{}",
                bundle.format(keys::PUSH_FAILED, &[("reason", Value::Str(&reason))])
            ))
        }
    }
}

/// `capsule list`: query the sync-fed local store and render it.
async fn list(include_deleted: bool) -> Result<()> {
    let bundle = i18n::cli_bundle();
    let db = db::init_sqlite()
        .await
        .map_err(|e| eyre!("Failed to open local database: {e}"))?;

    let rows = match remote::list(&db, include_deleted).await {
        Ok(rows) => rows,
        Err(error) => {
            let reason = describe_remote_error(&bundle, &error);
            return Err(eyre!(
                "{}",
                bundle.format(keys::LIST_FAILED, &[("reason", Value::Str(&reason))])
            ));
        }
    };

    if rows.is_empty() {
        println!("{}", bundle.format(keys::LIST_EMPTY, &[]).yellow());
        return Ok(());
    }

    println!(
        "{}",
        bundle
            .format(
                keys::LIST_HEADER,
                &[("count", Value::Int(rows.len() as i64))]
            )
            .green()
    );
    for row in rows {
        let state_key = if row.original_held {
            keys::LIST_STATE_ACTIVE
        } else {
            keys::LIST_STATE_AWAITING_ORIGINAL
        };
        let state = bundle.format(state_key, &[]);
        let asset = display_id(&row.asset_id);
        let album = display_id(&row.album_id);
        println!(
            "  {}",
            bundle.format(
                keys::LIST_ROW,
                &[
                    ("asset_id", Value::Str(&asset)),
                    ("album_id", Value::Str(&album)),
                    ("seq", Value::Int(row.sync_seq as i64)),
                    ("state", Value::Str(&state)),
                ],
            )
        );
    }
    Ok(())
}

fn open_library_or_err(path: &Path) -> Result<Library> {
    open_library(path).map_err(|e| match e {
        LibraryError::CorruptVersion(msg) => {
            eyre!(
                "Library at {} has a corrupt version file: {}",
                path.display(),
                msg
            )
        }
        LibraryError::Locked { pid, hostname, .. } => eyre!(
            "Library at {} is locked by PID {} on {}. Is another Capsule instance running?",
            path.display(),
            pid,
            hostname
        ),
        LibraryError::VersionMismatch { found, expected } => {
            eyre!("Library version mismatch: found {found}, expected {expected}. Upgrade required.")
        }
        other => eyre!("Failed to open library at {}: {other}", path.display()),
    })
}

#[cfg(test)]
mod client_build_wiring_tests {
    use capsule_core::client_build::ClientVersion;
    use capsule_core::crypto::primitives::Argon2Params;
    use capsule_core::lifecycle::Workspace;

    use super::as_capsule_cli;

    /// S-D15: a workspace built the way the CLI builds one (`as_capsule_cli`, the same wrapper the
    /// `import`/`demo` paths use) stamps every manifest it authors with
    /// `capsule-cli/{semver}+{commit}` — proving the CLI reports itself, not the bare
    /// `capsule-core` default. Uses a fast Argon2 cost so the test never pays the production KDF.
    #[test]
    fn cli_workspace_stamps_capsule_cli_client_version() {
        let dir = std::env::temp_dir().join(format!("capsule-cli-s-d15-{}", nanoid::nanoid!()));
        let lib = dir.join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        let src = dir.join("photo.jpg");
        std::fs::write(&src, b"\xFF\xD8\xFF cli wiring bytes").unwrap();

        let mut ws = as_capsule_cli(
            Workspace::create_with_params(
                &lib,
                b"pw",
                Argon2Params {
                    mem_kib: 64,
                    t_cost: 1,
                    p_cost: 1,
                },
            )
            .unwrap(),
        );
        let album = ws.default_album_id();
        ws.create_album_with_id(album, "Imports").unwrap();
        let id = ws.import_asset(album, &src).unwrap();

        let cv = ws
            .asset(&id)
            .unwrap()
            .chain
            .records()
            .last()
            .unwrap()
            .manifest
            .core
            .client_version
            .clone();
        std::fs::remove_dir_all(&dir).ok();

        assert!(
            cv.starts_with("capsule-cli/"),
            "the CLI must author capsule-cli/..., got {cv}"
        );
        let parsed = ClientVersion::parse(&cv).expect("CLI client_version must parse the grammar");
        assert_eq!(parsed.client_id, "capsule-cli");
        assert_eq!(parsed.semver, env!("CARGO_PKG_VERSION"));
    }
}
