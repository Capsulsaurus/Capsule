//! The Capsule CLI as a library: the command implementations and the dispatch the
//! thin `capsule` binary calls.
//!
//! The networked commands (`auth login/logout`, `sync`, `list`)
//! live in [`remote`] and are driven entirely over `capsule-sdk`; [`session`] and
//! [`syncstore`] are their durable state. Splitting the logic into a library lets
//! the real-server round-trip drive these command functions directly rather than
//! spawning the binary.

use std::path::{Path, PathBuf};

use capitalize::Capitalize;
use capsule_core::crypto::primitives::DeviceTier;
use capsule_core::domain::ImportMode;
use capsule_library::file_metadata::FileMetadata;
use capsule_library::import::{
    CancellationToken, DefaultAlbumContext, ImportConfig, ImportOutcome, ImportProgressEvent,
    ScanResult, SourceAdapter, SourceMetadataIndex, TakeoutAdapter, execute_with_source_metadata,
    plan, scan_paths as scan_files,
};
use capsule_library::library::{Library, LibraryError, init_library, open_library, rebuild_index};
use capsule_library::lifecycle::{DerivativeStatus, Workspace, unmigrated_sidecars};
use capsule_sdk::cache_budget::DesktopConnectionClass;
use cli::{
    AuthCommands, CacheCommands, Cli, Commands, ImportProviderArg, LibraryCommands, RepairCommands,
    ShareCommands, SyncCommands,
};
use colored::*;
use dialoguer::{Confirm, Input, Password};
use eyre::{Result, eyre};

use crate::i18n::error_message;
use crate::utils::directories::{
    get_cache_dir, get_config_dir, get_data_dir, get_session_file_path,
};

pub mod cache;
pub mod cli;
pub mod cull;
pub mod db;
pub mod demo;
pub mod i18n;
pub mod import_report;
pub mod migrate;
pub mod oidc;
pub mod reannounce;
pub mod remote;
pub mod repair;
pub mod restore;
pub mod session;
pub mod share;
pub mod show;
pub mod status;
pub mod syncstore;
pub mod tui;
pub mod utils;
pub mod verify;

/// The stable product id this CLI reports on every manifest it authors. Combined with
/// this crate's own semver and capsule-core's build-embedded commit, it composes the
/// `client_version` grammar `capsule-cli/{semver}+{commit}[.dirty]`.
pub const CLIENT_ID: &str = "capsule-cli";

/// Tag a freshly built workspace with this CLI's build identity so every manifest and derivative
/// it produces carries `capsule-cli/{semver}+{commit}` rather than the bare
/// `capsule-core` default. Applied at every workspace construction site in the CLI.
#[must_use]
fn as_capsule_cli(ws: Workspace) -> Workspace {
    ws.with_client_id(CLIENT_ID, env!("CARGO_PKG_VERSION"))
}

/// The refusal every networked command gives before a session exists.
pub(crate) const NOT_AUTHENTICATED: &str = "Not authenticated. Run 'capsule auth login' first.";

/// The `capsule library info` row for the unmigrated-sidecar count, plus the pointer at
/// `capsule library migrate` — `None` when nothing is left to migrate. The count comes from
/// the keyless scan, since `info` opens the library without a passphrase.
#[must_use]
fn library_info_unmigrated(unmigrated: usize) -> (String, Option<String>) {
    let row = format!("Unmigrated sidecars: {unmigrated}");
    let hint = match unmigrated {
        0 => None,
        1 => Some("Run `capsule library migrate` to admit the 1 unsigned sidecar.".to_owned()),
        count => Some(format!(
            "Run `capsule library migrate` to admit the {count} unsigned sidecars."
        )),
    };
    (row, hint)
}

/// Resolve the persisted-session file path, erroring if the config directory
/// cannot be determined.
fn session_store() -> Result<session::SessionStore> {
    let path =
        get_session_file_path().ok_or_else(|| eyre!("Failed to resolve config directory"))?;
    Ok(session::SessionStore::new(path))
}

/// A human-readable reason for a networked-command failure: the English catalog message for
/// the stable `error.*` code when the error carries one, else the English detail.
fn describe_remote_error(error: &remote::RemoteError) -> String {
    match error.error_code() {
        Some(code) => error_message(code),
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
/// [source adapter]: capsule_library::import::TakeoutAdapter
fn read_import_source(
    provider: Option<ImportProviderArg>,
    paths: &[PathBuf],
    human: bool,
) -> Result<(ScanResult, SourceMetadataIndex)> {
    match provider {
        None => {
            let scanned =
                scan_files(paths).map_err(|e| eyre!("Scanning the source files failed: {e}"))?;
            Ok((scanned, SourceMetadataIndex::empty()))
        }
        Some(provider @ ImportProviderArg::Takeout) => {
            if human {
                println!(
                    "{}",
                    format!(
                        "Reading the source as a {} export.",
                        provider.display_name()
                    )
                    .cyan()
                );
            }
            let extracted = TakeoutAdapter::new()
                .extract(paths)
                .map_err(|e| eyre!("Reading the export failed: {e}"))?;
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

/// The per-file line `capsule import` prints as each file lands. An original imported without
/// its derivatives says so, so a gap is visible where it happens rather than only in the total.
fn progress_line(path: &Path, outcome: &ImportOutcome) -> ColoredString {
    let msg = format!("  {}", path.display());
    match outcome {
        ImportOutcome::Imported {
            derivatives: DerivativeStatus::DeferredNoCodec,
            ..
        } => format!("✓ {msg} (previews deferred: no codec in this build)").green(),
        ImportOutcome::Imported {
            derivatives: DerivativeStatus::DecodeFailed,
            ..
        } => format!("✓ {msg} (original kept; its previews did not decode)").yellow(),
        ImportOutcome::Imported { .. } => format!("✓ {msg}").green(),
        ImportOutcome::DuplicateSkipped { .. } => format!("= {msg} (duplicate)").yellow(),
        ImportOutcome::CorruptTransfer => format!("✗ {msg} (corrupt transfer)").red(),
        ImportOutcome::CorruptUnreadable(e) => format!("✗ {msg} (unreadable: {e})").red(),
        ImportOutcome::PermissionDenied(e) => format!("✗ {msg} (permission denied: {e})").red(),
        _ => format!("- {msg}").dimmed(),
    }
}

/// Parse the CLI arguments and dispatch the matching command.
///
/// The CLI is English-only (ADR-0011): help pages and usage errors are the derive's doc
/// comments, whatever the process locale.
///
/// Bare `capsule` on a terminal opens the TUI ([`tui`]); off a terminal it prints the help and
/// exits 2, as a missing subcommand always did.
pub async fn run() -> Result<()> {
    let cli: Cli = cli::parse(tui::is_interactive());
    tracing::trace!("Parsed CLI arguments: {:#?}", cli);
    match cli.command {
        Some(command) => dispatch(command).await,
        // `cli::parse` lets a bare invocation through only on a terminal.
        None => tui::run().await,
    }
}

/// Dispatch an already-parsed command.
async fn dispatch(command: Commands) -> Result<()> {
    match command {
        // ── Auth ──────────────────────────────────────────────────────────
        Commands::Auth { command } => match command {
            AuthCommands::Register {
                email,
                password_stdin,
            } => auth_register(email, password_stdin).await?,
            AuthCommands::Login {
                oidc: true,
                device: true,
                ..
            } => auth_login_oidc_device().await?,
            AuthCommands::Login {
                oidc: true,
                no_browser,
                ..
            } => auth_login_oidc(no_browser).await?,
            AuthCommands::Login {
                email,
                password_stdin,
                ..
            } => auth_login(email, password_stdin).await?,
            AuthCommands::Escrow {
                library,
                passphrase_stdin,
                replace,
            } => auth_escrow(&library, passphrase_stdin, replace).await?,
            AuthCommands::Recover {
                library,
                recovery_secret_stdin,
                passphrase_stdin,
            } => auth_recover(&library, recovery_secret_stdin, passphrase_stdin).await?,
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

        // ── Cache ─────────────────────────────────────────────────────────
        Commands::Cache { command } => match command {
            CacheCommands::Sweep { library, json } => cache_sweep_library(&library, json)?,
            CacheCommands::Budget { set, reset, json } => cache_budget(set, reset, json)?,
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
                let unmigrated = unmigrated_sidecars(&path).len();
                tracing::debug!(
                    library = %path.display(),
                    unmigrated,
                    "library info: unmigrated-sidecar count"
                );
                let (count_line, hint) = library_info_unmigrated(unmigrated);
                println!("  {count_line}");
                if let Some(hint) = hint {
                    println!("{}", hint.yellow());
                }
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
            LibraryCommands::Migrate {
                path,
                passphrase_stdin,
                retain_days,
            } => {
                let mut ws = open_workspace_quiet(&path, passphrase_stdin)?;
                println!(
                    "{}",
                    format!(
                        "Migrating the unsigned sidecars in {} ({} found)…",
                        path.to_string_lossy(),
                        ws.unmigrated_sidecars().len()
                    )
                    .cyan()
                );
                match migrate::run(&mut ws, retain_days) {
                    Ok(report) => print!(
                        "{}",
                        migrate::render(&report, ws.unmigrated_sidecars().len())
                    ),
                    Err(error) => return Err(eyre!("Migration failed: {error}")),
                }
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
            json,
        } => {
            // With `--json` stdout carries exactly one document, the report, so every progress
            // line below is for a person only.
            let human = !json;
            if human {
                println!(
                    "{}",
                    format!(
                        "Importing {} into library {}…",
                        display_paths(&paths),
                        library.to_string_lossy()
                    )
                    .green()
                );
            }

            let mut ws = open_workspace(&library, passphrase_stdin)?;

            // Phase 1: Scan (plain tree) or extract (third-party export).
            if human {
                println!("{}", "Scanning source files…".cyan());
            }
            let (scan_result, source_metadata) = read_import_source(provider, &paths, human)?;
            let (candidates, files) = (scan_result.candidates.len(), scan_result.total_files());
            if human {
                println!(
                    "{}",
                    format!("Found {candidates} candidate(s) ({files} file(s) total).").green()
                );
            }

            // Phase 2: Plan
            let config = ImportConfig {
                import_mode: if r#move {
                    ImportMode::Move
                } else {
                    ImportMode::Copy
                },
                force_reimport_duplicates: force,
                // No explicit pick and no owner pointer on the CLI yet: bind the library's
                // derived de facto album so the plan records the rule that fired.
                album: DefaultAlbumContext::derived(ws.default_album_id()),
                ..Default::default()
            };

            let plan_result = plan(&scan_result, ws.db(), &config)
                .map_err(|e| eyre!("Planning the import failed: {e}"))?;

            let counts = &plan_result.counts;
            if human {
                println!(
                    "{}",
                    format!(
                        "Plan: {} to import, {} duplicate(s) skipped, {} unreadable.",
                        counts.to_import, counts.duplicates, counts.errors
                    )
                    .cyan()
                );
            }

            // A Move run goes on to execute even with nothing new to import: its duplicates are
            // sources the library already holds, which the release below may now delete.
            if plan_result.counts.to_import == 0 && !r#move {
                // Nothing executes, so the plan's own skips are the run's outcomes: a source
                // that could not be read is reported, never an empty success.
                let skipped = import_report::skipped_by_plan(&plan_result);
                if json {
                    let report =
                        import_report::ImportReport::new(candidates, files, counts, &skipped);
                    println!("{}", serde_json::to_string_pretty(&report)?);
                    return Ok(());
                }
                for (path, outcome) in &skipped.outcomes {
                    println!("{}", progress_line(path, outcome));
                }
                println!("{}", "Nothing to import.".yellow());
                // `--push` is still honored: an unchanged library is exactly the
                // re-runnable case push exists to make cheap.
                if push {
                    return push_workspace(&mut ws, push_options(false, false, staged), false)
                        .await;
                }
                return Ok(());
            }

            // Phase 3: Execute
            if human {
                println!("{}", "Importing…".cyan());
            }
            let token = CancellationToken::new();

            // The one execute path: a plain filesystem import reaches it with an empty index
            // and behaves exactly as it always has; a `--provider` import reaches it with the
            // adapter's folded metadata, which lands inside each signed sidecar.
            let summary = execute_with_source_metadata(
                &plan_result,
                &mut ws,
                &config,
                &source_metadata,
                |event| {
                    if let ImportProgressEvent::CandidateCompleted { outcomes, .. } = event
                        && human
                    {
                        for (path, outcome) in &outcomes {
                            println!("{}", progress_line(path, outcome));
                        }
                    }
                },
                &token,
            )
            .map_err(|e| eyre!("The import failed: {e}"))?;

            let report = import_report::ImportReport::new(candidates, files, counts, &summary);
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{}", import_report::summary_line(&report).green());
            }

            // The executor never deletes a Move source: the local copy is not custody
            // (invariant 7). Each one waits for the server, below.
            let moved: Vec<remote::MovedSource> = if r#move {
                summary
                    .held_sources()
                    .into_iter()
                    .map(|(source, asset_id)| remote::MovedSource { source, asset_id })
                    .collect()
            } else {
                Vec::new()
            };

            // `--push` is sugar only: import itself never touches the network (its
            // determinism suite depends on that), and the push is a separate pass over
            // the committed library — byte-for-byte the same one `capsule push` runs.
            if push {
                let pushed =
                    push_workspace(&mut ws, push_options(false, false, staged), false).await;
                // Released whether or not every asset pushed: the gate re-asks the server
                // about each source's own asset, so one failed asset keeps only its source.
                let released = release_moved(&ws, &moved).await;
                pushed?;
                released?;
            } else if !moved.is_empty() {
                // On stderr under `--json`, which owns stdout.
                let line = remote::sources_kept_offline_line(moved.len());
                if human {
                    println!("{}", line.yellow());
                } else {
                    eprintln!("{line}");
                }
            }
        }

        // ── Push ──────────────────────────────────────────────────────────
        Commands::Push {
            library,
            passphrase_stdin,
            dry_run,
            force,
            staged,
            json,
        } => {
            let mut ws = open_workspace(&library, passphrase_stdin)?;
            push_workspace(&mut ws, push_options(dry_run, force, staged), json).await?;
        }

        // ── Restore ───────────────────────────────────────────────────────
        Commands::Restore {
            library,
            out,
            album,
            asset,
            passphrase_stdin,
            json,
        } => {
            let ws = open_workspace(&library, passphrase_stdin)?;
            let scope = match (album, asset) {
                (Some(album), _) => restore::RestoreScope::Album(album),
                (None, Some(asset)) => restore::RestoreScope::Asset(asset),
                (None, None) => restore::RestoreScope::Library,
            };
            let options = restore::RestoreOptions {
                scope,
                out,
                // Sampled through the same port as a push. A restore is a foreground request
                // for the originals, so the class sizes its range windows but never defers it.
                connection: cache::transfer_plan(&DesktopConnectionClass::new()).class,
                range_window: restore::RESTORE_RANGE_WINDOW,
            };
            restore_workspace(&ws, &options, json).await?;
        }

        // ── Verify ────────────────────────────────────────────────────────
        Commands::Verify {
            library,
            asset,
            signed,
            deep,
            passphrase_stdin,
            json,
        } => {
            let ws = open_workspace(&library, passphrase_stdin)?;
            let options = verify::VerifyOptions {
                assets: asset,
                signed,
                deep,
            };
            verify_workspace(&ws, &options, json).await?;
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
            match cull::apply(&mut ws, &request) {
                Ok(summary) => cull::render(&request, &summary),
                Err(error) => {
                    return Err(eyre!("Cull failed: {}", cull::describe_error(&error)));
                }
            }
        }

        // ── Show ──────────────────────────────────────────────────────────
        Commands::Show {
            asset,
            library,
            passphrase_stdin,
        } => {
            let ws = open_workspace(&library, passphrase_stdin)?;
            let view = show::resolve(&ws, &asset).and_then(|id| {
                show::collect(&ws, &id).ok_or_else(|| show::ShowError::UnknownAsset(id.to_string()))
            });
            match view {
                Ok(view) => print!("{}", show::render(&view)),
                Err(error) => {
                    return Err(eyre!("Show failed: {}", show::describe_error(&error)));
                }
            }
        }

        // ── Repair ────────────────────────────────────────────────────────
        Commands::Repair { command } => match command {
            RepairCommands::CaptureTime {
                library,
                passphrase_stdin,
                apply,
                limit,
            } => {
                // `--limit` bounds what `--apply` writes; alone it would silently do nothing.
                if limit.is_some() && !apply {
                    return Err(eyre!(
                        "--limit only bounds what --apply writes; give --apply as well, or drop \
                         --limit."
                    ));
                }
                let request = repair::RepairRequest {
                    apply,
                    limit: limit.map(|n| usize::try_from(n).unwrap_or(usize::MAX)),
                };
                let mut ws = open_workspace(&library, passphrase_stdin)?;
                match repair::run(&mut ws, request) {
                    Ok(summary) => print!("{}", repair::render(request, &summary)),
                    Err(error) => {
                        return Err(eyre!("Repair failed: {}", repair::describe_error(&error)));
                    }
                }
            }
        },

        // ── Share ─────────────────────────────────────────────────────────
        Commands::Share { command } => match command {
            ShareCommands::Create {
                library,
                album,
                asset,
                expires_at,
                passphrase_stdin,
                protect,
                link_passphrase_stdin,
            } => {
                let scope = match (album, asset) {
                    (_, Some(asset)) => capsule_core::sharing::ShareScope::Asset(asset),
                    (Some(album), None) => capsule_core::sharing::ShareScope::Album(album),
                    (None, None) => {
                        unreachable!("clap requires --album unless --asset is present")
                    }
                };
                share_create(
                    &library,
                    scope,
                    expires_at,
                    passphrase_stdin,
                    protect,
                    link_passphrase_stdin,
                )
                .await?;
            }
            ShareCommands::Revoke { opaque_id } => share_revoke(&opaque_id).await?,
        },

        // ── Demo ──────────────────────────────────────────────────────────
        Commands::Demo { workdir, image } => {
            demo::run(workdir, image)?;
        }

        // ── Sync ──────────────────────────────────────────────────────────
        Commands::Sync {
            command:
                Some(SyncCommands::Reannounce {
                    library,
                    passphrase_stdin,
                    dry_run,
                    json,
                }),
            ..
        } => {
            let ws = open_workspace(&library, passphrase_stdin)?;
            reannounce_workspace(&ws, dry_run, json).await?;
        }
        Commands::Sync {
            command: None,
            force,
            dry_run,
            library,
            passphrase_stdin,
        } => sync(dry_run, force, library.as_deref(), passphrase_stdin).await?,

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

// ─── Networked commands ───────────────────────────────────────────────

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

/// Read a secret from stdin when `stdin_flag` names the `--*-stdin` flag that asked for it,
/// or prompt for it.
///
/// The flag is passed through so an empty stdin is reported against the flag this command
/// actually has (`--password-stdin`, `--passphrase-stdin`, `--recovery-secret-stdin`).
fn read_password(stdin_flag: Option<&str>, prompt: String) -> Result<String> {
    if let Some(flag) = stdin_flag {
        return read_stdin_secret(&mut std::io::stdin().lock(), flag);
    }
    Password::new()
        .with_prompt(prompt)
        .interact()
        .map_err(|e| eyre!("Failed to read password: {e}"))
}

/// The stdin half of [`read_password`]: the first line of `reader`, with only the trailing
/// newline trimmed so a secret containing spaces survives (the convention
/// `docker login --password-stdin` uses). An empty line is refused, naming `flag`.
fn read_stdin_secret(reader: &mut impl std::io::BufRead, flag: &str) -> Result<String> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| eyre!("Failed to read password from stdin: {e}"))?;
    let secret = line.trim_end_matches(['\n', '\r']).to_string();
    if secret.is_empty() {
        return Err(eyre!("{flag} was given but stdin was empty"));
    }
    Ok(secret)
}

/// `capsule auth register`: create an account over the SDK and persist the session.
async fn auth_register(email: Option<String>, password_stdin: bool) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;

    // An address and a password, and nothing else. The server takes nothing else: a
    // display name is a fact about a person, and the profile surface that would hold one is
    // owed rather than assumed. Prompting for a value the server discards would be worse than
    // not asking.
    let email = flag_or_prompt(email, EMAIL_PROMPT.to_owned())?;
    let password = read_password(
        password_stdin.then_some("--password-stdin"),
        PASSWORD_PROMPT.to_owned(),
    )?;

    println!("{}", "Creating your Capsule account…".green());
    match remote::auth_register(&remote, &store, &email, &password).await {
        Ok(()) => {
            println!(
                "{}",
                format!("Account created. Signed in as {email}.").green()
            );
            Ok(())
        }
        Err(error) => Err(eyre!(
            "Could not create the account: {}",
            describe_remote_error(&error)
        )),
    }
}

/// The prompt for an account's email address.
const EMAIL_PROMPT: &str = "Email";

/// The prompt for an account's password.
const PASSWORD_PROMPT: &str = "Password";

/// `capsule auth login`: read credentials from flags or prompts, authenticate over the SDK,
/// and persist the session.
async fn auth_login(email: Option<String>, password_stdin: bool) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;

    let email = flag_or_prompt(email, EMAIL_PROMPT.to_owned())?;
    let password = read_password(
        password_stdin.then_some("--password-stdin"),
        PASSWORD_PROMPT.to_owned(),
    )?;

    println!("{}", "Logging in to Capsule…".green());
    let step = match remote::auth_login(&remote, &store, &email, &password).await {
        Ok(step) => step,
        Err(error) => return Err(login_failed(&describe_remote_error(&error))),
    };

    finish_second_factor(&remote, &store, step).await?;

    println!("{}", format!("Logged in as {email}.").green());
    Ok(())
}

/// Finish a sign-in that answered a second-factor challenge, by asking for the code.
///
/// The first factor verified and the sign-in may not be finished. The CLI is
/// interactive, so it asks — a client that could not would have to report a second factor as
/// a failure, which is what the SDK's `LoginOutcome::into_session` is for and what this is
/// deliberately not. Shared by the password and the identity-provider sign-ins, which honour the
/// same factor.
async fn finish_second_factor(
    remote: &remote::RemoteConfig,
    store: &session::SessionStore,
    step: remote::LoginStep,
) -> Result<()> {
    if let remote::LoginStep::SecondFactorRequired { mfa_token } = step {
        println!(
            "{}",
            "This account has two-factor authentication switched on.".yellow()
        );
        let code = flag_or_prompt(None, "Authenticator code".to_owned())?;
        if let Err(error) = remote::auth_verify_totp(remote, store, &mfa_token, code.trim()).await {
            return Err(login_failed(&describe_remote_error(&error)));
        }
    }
    Ok(())
}

/// The line a browser sign-in prints while it waits, in whole minutes rounded up.
fn oidc_waiting_line(waited: std::time::Duration) -> String {
    match waited.as_secs().div_ceil(60) {
        1 => "Waiting up to 1 minute for the sign-in to finish…".to_owned(),
        count => format!("Waiting up to {count} minutes for the sign-in to finish…"),
    }
}

/// The line a device sign-in prints while it waits, in whole minutes rounded up.
fn device_waiting_line(waited: std::time::Duration) -> String {
    match waited.as_secs().div_ceil(60) {
        1 => "Waiting up to 1 minute for you to approve the sign-in…".to_owned(),
        count => format!("Waiting up to {count} minutes for you to approve the sign-in…"),
    }
}

/// The line an identity-provider sign-in prints when it succeeds.
const OIDC_SUCCESS: &str = "Signed in through your identity provider.";

/// `capsule auth login --oidc`: sign in through the server's identity provider in a browser,
/// over a loopback listener, and persist the session ([`oidc`] owns the flow).
async fn auth_login_oidc(no_browser: bool) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let ceremony = oidc::SdkCeremony::new(&remote, &store)
        .map_err(|error| login_failed(&describe_remote_error(&error)))?;
    let pages = oidc::CallbackPages {
        received: "Capsule received your sign-in. Return to the terminal, which finishes signing \
                   you in."
            .to_owned(),
        refused: "The sign-in did not complete. Return to the terminal to see why.".to_owned(),
    };

    println!(
        "{}",
        "Signing in to Capsule through your identity provider…".green()
    );
    let show = |authorization_url: &str, waited: std::time::Duration| {
        println!("To sign in, open this address in a browser:");
        println!("{authorization_url}");
        if !no_browser && !oidc::open_in_browser(authorization_url) {
            println!(
                "{}",
                "No browser could be opened from here. Open the address above in any browser."
                    .yellow()
            );
        }
        println!("{}", oidc_waiting_line(waited));
    };
    let step = oidc::login(&ceremony, show, &pages, oidc::LISTENER_TIMEOUT)
        .await
        .map_err(|error| login_failed(&describe_oidc_error(&error)))?;

    finish_second_factor(&remote, &store, step).await?;
    println!("{}", OIDC_SUCCESS.green());
    Ok(())
}

/// `capsule auth login --oidc --device`: sign in through the server's identity provider from
/// another device (RFC 8628, issue #635) — print the address and the code, poll until the person
/// approves, and persist the session ([`oidc::login_device`] owns the flow).
async fn auth_login_oidc_device() -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let ceremony = oidc::SdkCeremony::new(&remote, &store)
        .map_err(|error| login_failed(&describe_remote_error(&error)))?;

    println!(
        "{}",
        "Signing in to Capsule through your identity provider, from another device…".green()
    );
    let show = |pending: &capsule_sdk::auth::OidcDeviceAuthorization,
                waited: std::time::Duration| {
        // Provider-authored, printed to a terminal: control characters are dropped, as the
        // server already refused any code carrying one.
        let printable = |text: &str| text.chars().filter(|c| !c.is_control()).collect::<String>();
        println!("On any device with a browser, open this address:");
        println!("{}", printable(&pending.verification_uri));
        println!(
            "{}",
            format!("and enter this code: {}", printable(&pending.user_code)).bold()
        );
        if let Some(complete) = &pending.verification_uri_complete {
            println!("Or open this address, which already carries the code:");
            println!("{}", printable(complete));
        }
        println!("{}", device_waiting_line(waited));
    };
    let step = oidc::login_device(&ceremony, show, oidc::DEVICE_TIMEOUT)
        .await
        .map_err(|error| login_failed(&describe_oidc_error(&error)))?;

    finish_second_factor(&remote, &store, step).await?;
    println!("{}", OIDC_SUCCESS.green());
    Ok(())
}

/// The `capsule auth login` failure line, carrying `reason`.
fn login_failed(reason: &str) -> eyre::Report {
    eyre!("Login failed: {reason}")
}

/// A human-readable reason for an OIDC sign-in failure.
///
/// A refused loopback redirect gets the remedy rather than only the code: the server's operator
/// has to set `OIDC_ALLOW_LOOPBACK_REDIRECT=true`, which is off by default, and nothing the person
/// at the terminal does will change that.
fn describe_oidc_error(error: &oidc::OidcLoginError) -> String {
    match error {
        oidc::OidcLoginError::Remote(remote::RemoteError::Auth(
            capsule_sdk::auth::AuthError::OidcRedirectInvalid,
        )) => "This server does not accept sign-ins from the command line through its identity \
               provider. Its operator can allow them by setting \
               OIDC_ALLOW_LOOPBACK_REDIRECT=true."
            .to_owned(),
        oidc::OidcLoginError::Remote(error) => describe_remote_error(error),
        oidc::OidcLoginError::Listener(error) => {
            format!("This computer could not listen for the sign-in to come back: {error}")
        }
        oidc::OidcLoginError::TimedOut { .. } => {
            "No sign-in came back in time. Run the command again to start a new one.".to_owned()
        }
        oidc::OidcLoginError::DeviceTimedOut { .. } => {
            "The sign-in was not approved in time. Run the command again to get a new code."
                .to_owned()
        }
        oidc::OidcLoginError::Denied { error, description } => {
            // Provider-authored text, printed to a terminal: control characters are dropped so
            // a provider cannot write escape sequences into it.
            let printable =
                |text: &str| text.chars().filter(|c| !c.is_control()).collect::<String>();
            let error = printable(error);
            match description {
                Some(description) => format!(
                    "The identity provider did not sign you in ({error}): {}",
                    printable(description)
                ),
                None => format!("The identity provider did not sign you in ({error})."),
            }
        }
    }
}

/// `capsule auth escrow`: set recovery up for the library at `library`, as the signed-in
/// account (#624). [`remote::auth_escrow`] owns the flow; this is its terminal: the library
/// passphrase, the one showing of the minted secret, and the type-back gate.
async fn auth_escrow(library: &Path, passphrase_stdin: bool, replace: bool) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;

    let passphrase = read_password(
        passphrase_stdin.then_some("--passphrase-stdin"),
        "Library passphrase".to_owned(),
    )?;
    let mut workspace = as_capsule_cli(
        Workspace::open(library, passphrase.as_bytes(), DeviceTier::Normal.params())
            .map_err(|e| eyre!("{}", escrow_failed(&e.to_string())))?,
    );

    let path = library.display().to_string();
    println!(
        "{}",
        format!("Setting up recovery for the library at {path}…").green()
    );
    let confirm = |secret: &capsule_sdk::recovery::MintedSecret| {
        let hex = secret.to_hex();
        println!(
            "Your recovery secret is shown once and never again. Write it down and keep it \
             somewhere safe:\n\n    {hex}\n"
        );
        match Input::<String>::new()
            .with_prompt("Type the recovery secret back to confirm you saved it")
            .interact_text()
        {
            Ok(typed) => typed_back_matches(secret, &typed),
            Err(error) => {
                tracing::warn!(%error, "escrow: the type-back prompt could not be read");
                false
            }
        }
    };
    match remote::auth_escrow(
        &remote,
        &store,
        &mut workspace,
        replace,
        DeviceTier::Normal,
        confirm,
    )
    .await
    {
        Ok(setup) => {
            println!(
                "{}",
                format!(
                    "Recovery is set up. The library at {path} is bound to your account, and \
                     'capsule auth recover' can restore your account on a new machine with the \
                     recovery secret you saved."
                )
                .green()
            );
            if setup.replaced {
                println!("{}", ESCROW_REPLACED.yellow());
            }
            Ok(())
        }
        Err(remote::RemoteError::NotAuthenticated) => Err(eyre!(NOT_AUTHENTICATED)),
        Err(error) => Err(eyre!("{}", describe_escrow_error(&error))),
    }
}

/// The warning `capsule auth escrow --replace` prints once it has replaced an earlier backup.
const ESCROW_REPLACED: &str = "This replaced the account's earlier recovery backup: the previous \
                               recovery secret no longer works. Re-export or destroy any backup \
                               made under it.";

/// The generic `capsule auth escrow` failure line, carrying `reason`.
fn escrow_failed(reason: &str) -> String {
    format!("Recovery setup failed: {reason}")
}

/// The line `capsule auth escrow` prints for a refusal: the three it names itself (an escrow
/// already stored, the type-back gate, a library that already holds assets), and the
/// generic failure line wrapping any other reason.
fn describe_escrow_error(error: &remote::RemoteError) -> String {
    match error {
        remote::RemoteError::EscrowAlreadyStored => {
            "This account already has recovery set up. Run the command again with --replace to \
             store a new recovery secret; the old one will stop working."
                .to_owned()
        }
        remote::RemoteError::RecoverySecretNotConfirmed => {
            "That does not match the recovery secret shown above. Nothing was stored; run the \
             command again for a new secret."
                .to_owned()
        }
        remote::RemoteError::Lifecycle(
            capsule_library::lifecycle::LifecycleError::AccountRebind(_),
        ) => "This library already holds assets created under its own account id, so it \
               cannot be bound to your server account. Run 'capsule auth escrow' on a fresh \
               library instead."
            .to_owned(),
        other => escrow_failed(&describe_remote_error(other)),
    }
}

/// The type-back gate's compare: `typed` is the hex the user was shown, surrounding
/// whitespace dropped and either case accepted; anything that is not 64 hex characters of
/// this secret fails.
fn typed_back_matches(secret: &capsule_sdk::recovery::MintedSecret, typed: &str) -> bool {
    hex::decode(typed.trim()).is_ok_and(|bytes| secret.matches(&bytes))
}

/// `capsule auth recover`: open a new library on this machine as the signed-in account, from
/// its escrow, its sealed identity and the recovery secret (#544).
///
/// The recovery secret is read first and the new library passphrase second, which is also
/// the order of the two stdin lines when both `--*-stdin` flags are given.
async fn auth_recover(
    library: &Path,
    recovery_secret_stdin: bool,
    passphrase_stdin: bool,
) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;

    let secret = read_password(
        recovery_secret_stdin.then_some("--recovery-secret-stdin"),
        "Recovery secret".to_owned(),
    )?;
    let passphrase = if passphrase_stdin {
        read_password(Some("--passphrase-stdin"), String::new())?
    } else {
        Password::new()
            .with_prompt("New library passphrase")
            .with_confirmation(
                "Repeat the library passphrase",
                "The passphrases do not match.",
            )
            .interact()
            .map_err(|e| eyre!("Failed to read password: {e}"))?
    };

    let path = library.display().to_string();
    println!(
        "{}",
        format!("Recovering your account into {path}…").green()
    );
    match remote::auth_recover(
        &remote,
        &store,
        library,
        &secret,
        passphrase.as_bytes(),
        DeviceTier::Normal.params(),
    )
    .await
    {
        Ok(workspace) => {
            let device = workspace.device_id();
            println!(
                "{}",
                format!(
                    "Recovered. The library at {path} opens as your account on this device \
                     ({device})."
                )
                .green()
            );
            Ok(())
        }
        Err(remote::RemoteError::NotAuthenticated) => Err(eyre!(NOT_AUTHENTICATED)),
        Err(error) => {
            let reason = match error {
                remote::RemoteError::WrongRecoverySecret => {
                    "That recovery secret does not open this account's recovery backup.".to_owned()
                }
                other => describe_remote_error(&other),
            };
            Err(eyre!("Recovery failed: {reason}"))
        }
    }
}

/// `capsule auth logout`: revoke the session over the SDK and clear local state.
async fn auth_logout() -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;

    println!("{}", "Logging out from Capsule…".yellow());
    match remote::auth_logout(&remote, &store).await {
        Ok(true) => {
            println!("{}", "Logged out.".green());
            Ok(())
        }
        Ok(false) => {
            println!("{}", "You are not logged in.".yellow());
            Ok(())
        }
        Err(error) => Err(eyre!("{}", describe_remote_error(&error))),
    }
}

/// `capsule sync`: drain the feed over the SDK into the local store.
///
/// When the server answers with a stale cursor, the drain starts over from zero (#697), and
/// what the library holds that the server lost is re-announced (#781): by this run when
/// `library` names the library, and otherwise by the `capsule sync reannounce` it names.
async fn sync(
    dry_run: bool,
    from_start: bool,
    library: Option<&Path>,
    passphrase_stdin: bool,
) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let db = db::init_sqlite()
        .await
        .map_err(|e| eyre!("Failed to open local database: {e}"))?;

    println!(
        "{}",
        format!("Syncing from {}…", remote.sync_endpoint).green()
    );
    if dry_run {
        println!("{}", "Dry run: no changes will be persisted.".yellow());
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
                println!("{}", "Already up to date.".cyan());
            } else {
                println!(
                    "{}",
                    format!(
                        "Sync complete: applied {} change(s) across {} album(s) in {} page(s).",
                        summary.applied, summary.albums, summary.pages
                    )
                    .green()
                );
            }
            if !summary.resynced {
                return Ok(());
            }
            println!(
                "{}",
                "The server rebuilt this library's index; resynced from the start.".yellow()
            );
            if let Some(library) = library {
                let ws = open_workspace(library, passphrase_stdin)?;
                return reannounce_workspace(&ws, dry_run, false).await;
            }
            println!(
                "{}",
                "Run `capsule sync reannounce --library PATH` to give it back what this \
                 library holds."
                    .yellow()
            );
            Ok(())
        }
        Err(remote::RemoteError::NotAuthenticated) => Err(eyre!(NOT_AUTHENTICATED)),
        Err(error) => Err(eyre!("Sync failed: {}", describe_remote_error(&error))),
    }
}

/// Open a Capsule library as a signed workspace.
///
/// Imports land on the signed lifecycle path (signed sidecar + manifest + provenance +
/// derivatives), never the legacy unsigned sidecar; a first open initializes the account
/// under the given passphrase. The default album is resolved-or-created: album keys
/// are durable now, so a second run MUST resolve the album the first run minted rather than
/// replacing it — minting a fresh one per run is exactly what left a reopened library unable to
/// decrypt or extend its own prior imports (and would make a push unrepeatable).
///
/// When the open found unsigned pre-signed-path sidecars, a notice naming their count
/// and `capsule library migrate` goes to stderr, so every verb over such a library says why
/// some assets are missing before the user runs the migration.
fn open_workspace(library: &Path, passphrase_stdin: bool) -> Result<Workspace> {
    let ws = open_workspace_quiet(library, passphrase_stdin)?;
    if let Some(notice) = migrate::unmigrated_notice(ws.unmigrated_sidecars()) {
        eprintln!("{}", notice.yellow());
    }
    Ok(ws)
}

/// [`open_workspace`] without the unmigrated-sidecar notice: for `capsule library migrate`,
/// which reports the same count as the work it is about to do.
fn open_workspace_quiet(library: &Path, passphrase_stdin: bool) -> Result<Workspace> {
    let passphrase = read_password(
        passphrase_stdin.then_some("--passphrase-stdin"),
        "Library passphrase".to_string(),
    )?;
    let mut ws = as_capsule_cli(
        Workspace::open(library, passphrase.as_bytes(), DeviceTier::Normal.params())
            .map_err(|e| eyre!("Failed to open signed workspace: {e}"))?,
    );
    let default_album = ws.default_album_id();
    ws.ensure_album(default_album, "Imports")
        .map_err(|e| eyre!("Failed to resolve the default album: {e}"))?;
    Ok(ws)
}

/// The push configuration behind the CLI's three push flags, on the connection class the
/// desktop source reports now. A `--staged` run opens the tier sessions in ladder order gated by
/// the class; the default `Full` policy opens them all eagerly unless the class defers bulk
/// transfers, which stages the push ([`cache::push_options`]). The desktop source has no OS
/// probe, so it reports `unmetered`.
fn push_options(dry_run: bool, force: bool, staged: bool) -> remote::PushOptions {
    cache::push_options(
        &cache::transfer_plan(&DesktopConnectionClass::new()),
        dry_run,
        force,
        staged,
    )
}

/// The CLI settings store, erroring if the config directory cannot be determined.
fn cache_settings() -> Result<cache::CacheSettingsStore> {
    let dir = get_config_dir().ok_or_else(|| eyre!("Failed to resolve config directory"))?;
    Ok(cache::CacheSettingsStore::new(
        dir.join(cache::SETTINGS_FILE_NAME),
    ))
}

/// The base cache budget from the user's setting, or the default.
fn base_cache_budget() -> Result<cache::BaseBudget> {
    cache_settings()?
        .base_budget()
        .map_err(|e| eyre!("Failed to read the cache budget setting: {e}"))
}

/// A byte count for a human: `2 GiB`.
fn human_bytes(bytes: u64) -> String {
    humansize::format_size(bytes, humansize::BINARY)
}

/// The plan lines `capsule cache budget` and `capsule cache sweep` print.
fn print_cache_plan(plan: &cache::PlanReport) {
    let source = match plan.base_budget_source {
        cache::BudgetSource::Default => "default",
        cache::BudgetSource::Setting => "set",
    };
    println!("  Connection class:  {}", plan.connection_class);
    println!(
        "  Base budget:       {} ({source})",
        human_bytes(plan.base_budget_bytes)
    );
    println!("  Cache budget:      {}", human_bytes(plan.budget_bytes));
    println!(
        "  Bulk transfers:    {}",
        if plan.defer_bulk {
            "deferred until an unmetered link"
        } else {
            "allowed"
        }
    );
}

/// `capsule cache sweep`: evict down to the budget the current class allows.
fn cache_sweep_library(library: &Path, json: bool) -> Result<()> {
    let base = base_cache_budget()?;
    let lib = open_library_or_err(library)?;
    let report = cache::sweep(DesktopConnectionClass::new(), base, |budget| {
        capsule_library::library::cache_sweep(&lib.db, budget)
    })
    .map_err(|e| eyre!("Cache sweep failed: {e}"))?;
    lib.close()
        .map_err(|e| eyre!("Failed to close library: {e}"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("{}", "Cache sweep:".green());
    print_cache_plan(&report.plan);
    let reclaimed = u64::try_from(report.bytes_reclaimed).unwrap_or_default();
    println!(
        "{}",
        format!(
            "Evicted {} cached representation(s), reclaiming {}.",
            report.evicted_count,
            human_bytes(reclaimed)
        )
        .cyan()
    );
    Ok(())
}

/// `capsule cache budget`: show the current plan, after storing or clearing the base budget.
fn cache_budget(set: Option<u64>, reset: bool, json: bool) -> Result<()> {
    if set.is_some() || reset {
        cache_settings()?
            .set_base_budget(set)
            .map_err(|e| eyre!("Failed to write the cache budget setting: {e}"))?;
    }
    let plan = cache::current_plan(DesktopConnectionClass::new(), base_cache_budget()?);
    if json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        return Ok(());
    }
    println!("{}", "Cache budget:".green());
    print_cache_plan(&plan);
    Ok(())
}

/// `capsule sync reannounce`: re-announce what `workspace` holds and the server's feed does not
/// list, and report each answer; `json` prints the run as one document on stdout.
///
/// A refused or missing-blobs answer is a result, not a failure: the run exits non-zero only
/// when it could not complete (no session, the feed unreadable, a batch refused whole).
async fn reannounce_workspace(workspace: &Workspace, dry_run: bool, json: bool) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let run_error = |error: &remote::RemoteError| {
        if matches!(error, remote::RemoteError::NotAuthenticated) {
            return eyre!(NOT_AUTHENTICATED);
        }
        eyre!("Re-announce failed: {}", describe_remote_error(error))
    };

    if json {
        let summary = remote::reannounce(&remote, &store, workspace, dry_run)
            .await
            .map_err(|error| run_error(&error))?;
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    println!(
        "{}",
        format!(
            "Re-announcing this library's manifests to {}…",
            remote.sync_endpoint
        )
        .green()
    );
    let summary = remote::reannounce(&remote, &store, workspace, dry_run)
        .await
        .map_err(|error| run_error(&error))?;
    if summary.dry_run {
        println!(
            "{}",
            format!(
                "Dry run: would re-announce {} of {} asset(s).",
                summary.reannounced, summary.held
            )
            .cyan()
        );
        return Ok(());
    }
    if summary.reannounced == 0 {
        println!(
            "{}",
            "The server lists everything this library holds.".cyan()
        );
        return Ok(());
    }
    println!(
        "{}",
        format!(
            "Re-announced {} asset(s): {} restored, {} already on the server, {} missing blobs, {} refused.",
            summary.reannounced,
            summary.restored,
            summary.already_indexed,
            summary.missing_blobs,
            summary.refused
        )
        .green()
    );
    if summary.missing_blobs > 0 {
        println!(
            "{}",
            "Run `capsule push` to upload what the server no longer holds.".yellow()
        );
    }
    for line in &summary.assets {
        if let Some(code) = &line.code {
            println!("  {}: {}", line.asset_id, error_message(code).red());
        }
    }
    Ok(())
}

/// `capsule push`: upload every managed asset of `workspace` over the SDK.
///
/// Re-runnable by construction: the outstanding work is derived from server truth (a sync-feed
/// pull), so a second run against an unchanged library uploads nothing.
///
/// An asset that fails does not stop the run; every failure is reported once the run is done
/// and the command exits non-zero. With `json`, a run that reaches its assets prints exactly one
/// JSON document on stdout — the [`remote::PushSummary`], each asset's outcome included — and
/// nothing else. A run-level failure (no session, no feed, no attestation keys) prints nothing
/// on stdout; its error goes to stderr and the command exits non-zero.
async fn push_workspace(
    workspace: &mut Workspace,
    options: remote::PushOptions,
    json: bool,
) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let assets = workspace.asset_ids().len();

    if json {
        let summary = if assets == 0 {
            remote::PushSummary {
                dry_run: options.dry_run,
                ..Default::default()
            }
        } else {
            remote::push(&remote, &store, workspace, options)
                .await
                .map_err(|error| push_run_error(&error))?
        };
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return push_failures(&summary);
    }

    if assets == 0 {
        println!("{}", "The library holds no assets to push.".yellow());
        return Ok(());
    }

    println!(
        "{}",
        format!("Pushing {assets} asset(s) to {}…", remote.upload_endpoint).green()
    );
    if options.dry_run {
        println!("{}", "Dry run: nothing will be uploaded.".yellow());
    }
    if options.force {
        println!(
            "{}",
            "Force: re-driving every blob, ignoring what the server already holds.".yellow()
        );
    }

    match remote::push(&remote, &store, workspace, options).await {
        Ok(summary) => {
            if summary.dry_run {
                println!(
                    "{}",
                    format!(
                        "Dry run: would upload {} blob(s) across {} asset(s) ({} bytes).",
                        summary.uploaded_blobs, summary.pushed_assets, summary.bytes
                    )
                    .cyan()
                );
            } else if summary.is_no_op() && summary.failed_assets == 0 {
                println!(
                    "{}",
                    "Everything in this library is already on the server.".cyan()
                );
            } else {
                println!(
                    "{}",
                    format!(
                        "Push complete: {} blob(s) uploaded, {} already stored, {} already held, \
                         {} deferred ({} bytes sent).",
                        summary.uploaded_blobs,
                        summary.merged_blobs,
                        summary.already_held_blobs,
                        summary.deferred_blobs,
                        summary.bytes
                    )
                    .green()
                );
            }
            if !summary.dry_run {
                println!("{}", remote::custody_line(&summary).cyan());
            }
            for failed in summary.failures() {
                eprintln!("{}", remote::failure_line(failed).red());
            }
            push_failures(&summary)
        }
        Err(error) => Err(push_run_error(&error)),
    }
}

/// A push that could not run at all — no session, no feed, no attestation keys — as the error
/// the command exits with.
fn push_run_error(error: &remote::RemoteError) -> eyre::Report {
    if matches!(error, remote::RemoteError::NotAuthenticated) {
        return eyre!(NOT_AUTHENTICATED);
    }
    eyre!("Push failed: {}", describe_remote_error(error))
}

/// The exit status of a finished push: an error naming how many assets failed when any did,
/// so the process exits non-zero after every asset has been tried and reported.
fn push_failures(summary: &remote::PushSummary) -> Result<()> {
    match remote::failed_run_reason(summary) {
        Some(reason) => Err(eyre!("Push failed: {reason}")),
        None => Ok(()),
    }
}

/// The Move half of `capsule import --move --push`: delete each of `moved` the server holds,
/// and report every source kept.
///
/// A failure of the whole release (no session, no attestation keys) keeps every source, says
/// so, and fails the command; a source kept because its asset is not confirmed yet is reported
/// and does not, since a re-run releases it.
async fn release_moved(workspace: &Workspace, moved: &[remote::MovedSource]) -> Result<()> {
    if moved.is_empty() {
        return Ok(());
    }
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    match remote::release_moved_sources(&remote, &store, workspace, moved).await {
        Ok(summary) => {
            println!("{}", remote::move_release_line(&summary).green());
            for outcome in summary.outcomes.iter().filter(|o| !o.released) {
                println!("{}", remote::kept_source_line(outcome).yellow());
            }
            Ok(())
        }
        Err(error) => {
            let line = format!(
                "Kept {} source files: the server's custody could not be checked: {error}",
                moved.len()
            );
            // Printed here as well as returned: a failed push is reported first and would
            // otherwise hide that every source was kept.
            println!("{}", line.yellow());
            Err(eyre!(
                "the --move release did not run; every source was kept"
            ))
        }
    }
}

/// `capsule restore`: restore the originals in `options.scope` and report the run.
///
/// With `json`, the whole run is one JSON document on stdout. Either way every failed asset is
/// reported and the command exits non-zero when any asset was not restored.
async fn restore_workspace(
    workspace: &Workspace,
    options: &restore::RestoreOptions,
    json: bool,
) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    if !json {
        println!(
            "{}",
            format!(
                "Restoring from {} into {}...",
                remote.sync_endpoint,
                options.out.display()
            )
            .green()
        );
    }
    let summary = remote::restore(&remote, &store, workspace, options)
        .await
        .map_err(|error| {
            if matches!(error, remote::RemoteError::NotAuthenticated) {
                return eyre!(NOT_AUTHENTICATED);
            }
            eyre!("restore failed: {}", describe_remote_error(&error))
        })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!("{}", restore::summary_line(&summary).cyan());
        for failed in summary.failures() {
            eprintln!("{}", restore::failure_line(failed).red());
        }
    }
    match restore::failed_run_reason(&summary) {
        Some(reason) => Err(eyre!("restore failed: {reason}")),
        None => Ok(()),
    }
}

/// `capsule verify`: ask the server about the assets in `options` and report the run.
///
/// With `json`, the whole run is one JSON document on stdout. Either way every asset that is not
/// durably held, or got no answer, is reported, and the command exits non-zero when any did.
/// A run-level failure (no session, no attestation keys) prints nothing on stdout.
async fn verify_workspace(
    workspace: &Workspace,
    options: &verify::VerifyOptions,
    json: bool,
) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    if !json {
        println!(
            "{}",
            format!(
                "Asking {} about {} assets{}...",
                remote.sync_endpoint,
                if options.assets.is_empty() {
                    workspace.asset_ids().len()
                } else {
                    options.assets.len()
                },
                if options.signed { " (signed)" } else { "" }
            )
            .green()
        );
    }
    let summary = remote::verify_storage(&remote, &store, workspace, options)
        .await
        .map_err(|error| {
            if matches!(error, remote::RemoteError::NotAuthenticated) {
                return eyre!(NOT_AUTHENTICATED);
            }
            eyre!("verify failed: {}", describe_remote_error(&error))
        })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!("{}", verify::summary_line(&summary).cyan());
        for problem in summary.problems() {
            eprintln!("{}", verify::problem_line(problem).red());
        }
    }
    match verify::failed_run_reason(&summary) {
        Some(reason) => Err(eyre!("verify failed: {reason}")),
        None => Ok(()),
    }
}

/// `capsule share create`: issue a view-only link over `scope` and print its URL.
async fn share_create(
    library: &Path,
    scope: capsule_core::sharing::ShareScope,
    expires_at: Option<String>,
    passphrase_stdin: bool,
    protect: bool,
    link_passphrase_stdin: bool,
) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let mut ws = open_workspace(library, passphrase_stdin)?;
    let passphrase = if !protect {
        None
    } else if link_passphrase_stdin {
        Some(read_password(
            Some("--link-passphrase-stdin"),
            String::new(),
        )?)
    } else {
        Some(
            Password::new()
                .with_prompt("Link passphrase (the recipient types it to open the link)")
                .with_confirmation(
                    "Repeat the link passphrase",
                    "The two link passphrases do not match.",
                )
                .interact()
                .map_err(|e| eyre!("Failed to read password: {e}"))?,
        )
    };
    let request = share::CreateRequest {
        scope,
        expires_at,
        passphrase,
    };

    println!(
        "{}",
        format!("Issuing a share link on {}…", remote.sync_endpoint).green()
    );
    match share::create(&remote, &store, &mut ws, &request, jiff::Timestamp::now()).await {
        Ok(issued) => {
            share::render_issued(&issued);
            Ok(())
        }
        Err(error) => Err(eyre!(
            "Share link not issued: {}",
            share::describe_error(&error)
        )),
    }
}

/// `capsule share revoke`: stop the link `opaque_id` from opening.
async fn share_revoke(opaque_id: &str) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    match share::revoke(&remote, &store, opaque_id).await {
        Ok(id) => {
            println!(
                "{}",
                format!("Share link {id} no longer opens for anyone.").green()
            );
            Ok(())
        }
        Err(error) => Err(eyre!(
            "Share link not revoked: {}",
            share::describe_error(&error)
        )),
    }
}

/// `capsule list`: query the sync-fed local store and render it.
async fn list(include_deleted: bool) -> Result<()> {
    let db = db::init_sqlite()
        .await
        .map_err(|e| eyre!("Failed to open local database: {e}"))?;

    let rows = match remote::list(&db, include_deleted).await {
        Ok(rows) => rows,
        Err(error) => {
            return Err(eyre!(
                "Failed to list the library: {}",
                describe_remote_error(&error)
            ));
        }
    };

    if rows.is_empty() {
        println!("{}", "No assets have synced yet.".yellow());
        return Ok(());
    }

    println!(
        "{}",
        format!("Synced library ({} asset(s)):", rows.len()).green()
    );
    for row in rows {
        let state = if row.original_held {
            "active"
        } else {
            "awaiting-original"
        };
        println!(
            "  {}  album={}  seq={}  {state}",
            display_id(&row.asset_id),
            display_id(&row.album_id),
            row.sync_seq
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
mod library_info_tests {
    use super::library_info_unmigrated;

    /// An up-to-date library still shows the row, with a zero, and no migrate pointer.
    #[test]
    fn a_fully_signed_library_shows_zero_and_no_hint() {
        let (row, hint) = library_info_unmigrated(0);
        assert_eq!(row, "Unmigrated sidecars: 0");
        assert_eq!(hint, None);
    }

    /// One unsigned sidecar takes the singular hint, still carrying the count; the pointer
    /// names the verb verbatim.
    #[test]
    fn one_unmigrated_sidecar_points_at_the_migrate_verb() {
        let (row, hint) = library_info_unmigrated(1);
        assert_eq!(row, "Unmigrated sidecars: 1");
        assert_eq!(
            hint.as_deref(),
            Some("Run `capsule library migrate` to admit the 1 unsigned sidecar.")
        );
    }

    /// Several take the plural hint, carrying the count.
    #[test]
    fn several_unmigrated_sidecars_take_the_plural_hint() {
        let (row, hint) = library_info_unmigrated(3);
        assert_eq!(row, "Unmigrated sidecars: 3");
        assert_eq!(
            hint.as_deref(),
            Some("Run `capsule library migrate` to admit the 3 unsigned sidecars.")
        );
    }
}

#[cfg(test)]
mod escrow_tests {
    use capsule_library::lifecycle::LifecycleError;
    use capsule_sdk::recovery::{MintedSecret, RecoveryError};

    use super::{describe_escrow_error, typed_back_matches};
    use crate::i18n::{error_codes, error_message};
    use crate::remote::RemoteError;

    /// The hex the user was shown passes, in either case and with the newline a terminal adds.
    #[test]
    fn the_type_back_gate_accepts_the_shown_hex() {
        let secret = MintedSecret::generate();
        let hex = secret.to_hex();
        assert!(typed_back_matches(&secret, &hex));
        assert!(typed_back_matches(
            &secret,
            &format!("  {}\n", hex.to_uppercase())
        ));
    }

    /// Another secret, a truncated copy, and text that is not hex all fail the gate.
    #[test]
    fn the_type_back_gate_refuses_anything_else() {
        let secret = MintedSecret::generate();
        let hex = secret.to_hex();
        assert!(!typed_back_matches(
            &secret,
            &MintedSecret::generate().to_hex()
        ));
        assert!(!typed_back_matches(&secret, &hex[..62]));
        assert!(!typed_back_matches(&secret, ""));
        assert!(!typed_back_matches(&secret, &"z".repeat(64)));
    }

    /// Each refusal the command names renders its own line, and the rebind refusal tells the
    /// user to use a fresh library (#624 decision).
    #[test]
    fn each_named_refusal_has_its_own_line() {
        let already = describe_escrow_error(&RemoteError::EscrowAlreadyStored);
        let mismatch = describe_escrow_error(&RemoteError::RecoverySecretNotConfirmed);
        let rebind = describe_escrow_error(&RemoteError::Lifecycle(LifecycleError::AccountRebind(
            "1 asset".into(),
        )));
        assert!(already.contains("--replace"), "{already}");
        assert!(mismatch.contains("does not match"), "{mismatch}");
        assert!(rebind.contains("fresh library"), "{rebind}");
        for line in [&already, &mismatch, &rebind] {
            assert!(!line.starts_with("Recovery setup failed"), "{line}");
        }
    }

    /// Any other failure is wrapped in the generic line, carrying the English catalog
    /// message for its code when it has one.
    #[test]
    fn other_failures_are_wrapped_in_the_failed_line() {
        let line = describe_escrow_error(&RemoteError::Recovery(RecoveryError::NotEnrolled));
        assert_eq!(
            line,
            format!(
                "Recovery setup failed: {}",
                error_message(error_codes::ESCROW_NOT_STORED)
            )
        );
    }
}

#[cfg(test)]
mod oidc_login_tests {
    use capsule_sdk::auth::AuthError;
    use clap::Parser as _;

    use super::{describe_oidc_error, device_waiting_line, oidc_waiting_line};
    use crate::cli::{AuthCommands, Cli, Commands};
    use crate::i18n::{error_codes, error_message};
    use crate::oidc::OidcLoginError;
    use crate::remote::RemoteError;

    /// `--oidc` is its own sign-in: it takes no password inputs, and `--no-browser` means
    /// nothing without it.
    #[test]
    fn the_oidc_flag_excludes_the_password_inputs() {
        let parsed = Cli::try_parse_from(["capsule", "auth", "login", "--oidc", "--no-browser"])
            .expect("--oidc --no-browser parses");
        assert!(matches!(
            parsed.command.expect("a subcommand was given"),
            Commands::Auth {
                command: AuthCommands::Login {
                    oidc: true,
                    no_browser: true,
                    ..
                }
            }
        ));
        for refused in [
            &["capsule", "auth", "login", "--oidc", "--email", "a@b.c"][..],
            &["capsule", "auth", "login", "--oidc", "--password-stdin"][..],
            &["capsule", "auth", "login", "--no-browser"][..],
        ] {
            assert!(Cli::try_parse_from(refused).is_err(), "{refused:?}");
        }
    }

    /// `--device` is an arm of `--oidc` and opens no browser, so `--no-browser` has nothing to
    /// add to it (issue #635).
    #[test]
    fn the_device_flag_rides_oidc_alone() {
        let parsed = Cli::try_parse_from(["capsule", "auth", "login", "--oidc", "--device"])
            .expect("--oidc --device parses");
        assert!(matches!(
            parsed.command.expect("a subcommand was given"),
            Commands::Auth {
                command: AuthCommands::Login {
                    oidc: true,
                    device: true,
                    no_browser: false,
                    ..
                }
            }
        ));
        for refused in [
            &["capsule", "auth", "login", "--device"][..],
            &[
                "capsule",
                "auth",
                "login",
                "--oidc",
                "--device",
                "--no-browser",
            ][..],
            &["capsule", "auth", "login", "--device", "--email", "a@b.c"][..],
        ] {
            assert!(Cli::try_parse_from(refused).is_err(), "{refused:?}");
        }
    }

    /// The device sign-in's own failures render their own lines: the timeout, and the server's
    /// denial, expiry and unsupported answers through their catalog codes.
    #[test]
    fn each_device_failure_has_its_own_line() {
        let timed_out = describe_oidc_error(&OidcLoginError::DeviceTimedOut {
            waited: std::time::Duration::from_secs(600),
        });
        assert!(timed_out.contains("not approved in time"), "{timed_out}");
        for (error, code) in [
            (
                AuthError::OidcDeviceDenied,
                error_codes::AUTH_OIDC_DEVICE_DENIED,
            ),
            (
                AuthError::OidcDeviceExpired,
                error_codes::AUTH_OIDC_DEVICE_EXPIRED,
            ),
            (
                AuthError::OidcDeviceUnsupported,
                error_codes::AUTH_OIDC_DEVICE_UNSUPPORTED,
            ),
        ] {
            assert_eq!(
                describe_oidc_error(&OidcLoginError::Remote(RemoteError::Auth(error))),
                error_message(code)
            );
        }
    }

    /// A refused loopback redirect names the server variable the operator has to set.
    #[test]
    fn a_refused_loopback_redirect_names_the_server_setting() {
        let line = describe_oidc_error(&OidcLoginError::Remote(RemoteError::Auth(
            AuthError::OidcRedirectInvalid,
        )));
        assert!(line.contains("OIDC_ALLOW_LOOPBACK_REDIRECT=true"), "{line}");
    }

    /// Each terminal failure renders its own line, and provider text cannot carry control
    /// characters into the terminal.
    #[test]
    fn each_oidc_failure_has_its_own_line() {
        let timed_out = describe_oidc_error(&OidcLoginError::TimedOut {
            waited: std::time::Duration::from_secs(300),
        });
        assert!(timed_out.contains("No sign-in came back"), "{timed_out}");
        assert_eq!(
            describe_oidc_error(&OidcLoginError::Denied {
                error: "access_denied".to_owned(),
                description: None,
            }),
            "The identity provider did not sign you in (access_denied)."
        );
        let described = describe_oidc_error(&OidcLoginError::Denied {
            error: "access_\u{1b}denied".to_owned(),
            description: Some("no\u{1b}[31m thanks".to_owned()),
        });
        assert_eq!(
            described,
            "The identity provider did not sign you in (access_denied): no[31m thanks"
        );
        assert!(
            describe_oidc_error(&OidcLoginError::Remote(RemoteError::Auth(
                AuthError::OidcNotConfigured
            )))
            .contains(&error_message(error_codes::AUTH_OIDC_NOT_CONFIGURED))
        );
    }

    /// The wait lines pluralize on whole minutes, rounded up.
    #[test]
    fn the_wait_lines_count_minutes() {
        let secs = std::time::Duration::from_secs;
        assert_eq!(
            oidc_waiting_line(secs(60)),
            "Waiting up to 1 minute for the sign-in to finish…"
        );
        assert_eq!(
            oidc_waiting_line(secs(241)),
            "Waiting up to 5 minutes for the sign-in to finish…"
        );
        assert_eq!(
            device_waiting_line(secs(30)),
            "Waiting up to 1 minute for you to approve the sign-in…"
        );
        assert_eq!(
            device_waiting_line(secs(600)),
            "Waiting up to 10 minutes for you to approve the sign-in…"
        );
    }
}

#[cfg(test)]
mod stdin_secret_tests {
    use super::read_stdin_secret;

    /// Only the trailing newline is trimmed, so inner and leading spaces survive.
    #[test]
    fn the_first_line_is_read_with_only_its_newline_trimmed() {
        let mut input = std::io::Cursor::new(" two words \r\nsecond line\n");
        let secret = read_stdin_secret(&mut input, "--password-stdin").unwrap();
        assert_eq!(secret, " two words ");
    }

    /// An empty line is refused against the flag the command was given, not a fixed one:
    /// `capsule auth recover` has no `--password-stdin` (#618 D2).
    #[test]
    fn an_empty_line_names_the_flag_that_asked_for_it() {
        for flag in ["--recovery-secret-stdin", "--passphrase-stdin"] {
            let mut input = std::io::Cursor::new("\n");
            let err = read_stdin_secret(&mut input, flag).unwrap_err().to_string();
            assert!(err.starts_with(flag), "{flag}: got {err}");
            assert!(!err.contains("--password-stdin"), "{flag}: got {err}");
        }
    }

    /// End of input with nothing read is the same refusal as an empty line.
    #[test]
    fn no_input_at_all_is_refused() {
        let mut input = std::io::Cursor::new("");
        let err = read_stdin_secret(&mut input, "--passphrase-stdin").unwrap_err();
        assert!(err.to_string().contains("--passphrase-stdin"));
    }
}

#[cfg(test)]
mod client_build_wiring_tests {
    use capsule_core::client_build::ClientVersion;
    use capsule_core::crypto::primitives::Argon2Params;
    use capsule_library::lifecycle::Workspace;

    use super::as_capsule_cli;

    /// A workspace built the way the CLI builds one (`as_capsule_cli`, the same wrapper the
    /// `import`/`demo` paths use) stamps every manifest it authors with
    /// `capsule-cli/{semver}+{commit}` — proving the CLI reports itself, not the bare
    /// `capsule-core` default. Uses a fast Argon2 cost so the test never pays the production KDF.
    #[test]
    fn cli_workspace_stamps_capsule_cli_client_version() {
        let dir = std::env::temp_dir().join(format!("capsule-cli-scratch-{}", nanoid::nanoid!()));
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
