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
use capsule_library::lifecycle::{DerivativeStatus, Workspace, unanchored_sidecars};
use capsule_sdk::cache_budget::DesktopConnectionClass;
use cli::{
    AuthCommands, CacheCommands, Cli, Commands, EditCommands, EditLibraryArgs, ImportProviderArg,
    LabelArgs, LabelCommands, LibraryCommands, PeerCommands, RegionCommands, RepairCommands,
    ShareCommands, SyncCommands, TrashCommands,
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
pub mod edit;
pub mod i18n;
pub mod import_report;
pub mod import_run;
pub mod oidc;
pub mod peer;
pub mod peering_state;
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

/// The line printed when a library holds sidecars no provenance chain anchors — the assets
/// they name are hidden from every verb — or `None` when it holds none.
#[must_use]
fn unanchored_notice(unanchored: usize) -> Option<String> {
    match unanchored {
        0 => None,
        1 => Some(
            "1 sidecar in this library has no provenance chain, so its asset is hidden.".to_owned(),
        ),
        count => Some(format!(
            "{count} sidecars in this library have no provenance chain, so their assets are \
             hidden."
        )),
    }
}

/// The `capsule library info` row for the unanchored-sidecar count, plus the
/// [`unanchored_notice`] when there are any. The count comes from the keyless scan, since
/// `info` opens the library without a passphrase.
#[must_use]
fn library_info_unanchored(unanchored: usize) -> (String, Option<String>) {
    (
        format!("Unanchored sidecars: {unanchored}"),
        unanchored_notice(unanchored),
    )
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
                json,
                ..
            } => auth_login_oidc_device(json).await?,
            AuthCommands::Login {
                oidc: true,
                no_browser,
                json,
                ..
            } => auth_login_oidc(no_browser, json).await?,
            AuthCommands::Login {
                email,
                password_stdin,
                json,
                ..
            } => auth_login(email, password_stdin, json).await?,
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
            AuthCommands::Logout { json } => auth_logout(json).await?,
            AuthCommands::Status { json: true } => {
                // A machine reader gets a failure as a non-zero exit, not a line on stdout.
                let auth_status = status::AuthStatus::check(&session_store()?)
                    .map_err(|e| eyre!("Error checking auth status: {e}"))?;
                println!("{}", serde_json::to_string_pretty(&auth_status)?);
            }
            AuthCommands::Status { json: false } => {
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
                let unanchored = unanchored_sidecars(&path).len();
                tracing::debug!(
                    library = %path.display(),
                    unanchored,
                    "library info: unanchored-sidecar count"
                );
                let (count_line, hint) = library_info_unanchored(unanchored);
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
            LibraryCommands::List {
                library,
                view,
                passphrase_stdin,
                json,
            } => {
                use tui::library::catalogue;
                let ws = open_workspace(&library, passphrase_stdin)?;
                let listing = catalogue::listing(&catalogue::collect(&ws), view.into());
                // The workspace holds the library's lock; the listing is owned values.
                drop(ws);
                tracing::info!(library = %library.display(), ?view, json, "library list");
                if json {
                    println!("{}", serde_json::to_string_pretty(&listing)?);
                } else {
                    print!("{}", catalogue::render_listing(&listing));
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
                    return push_workspace(
                        &mut ws,
                        push_options(false, false, staged),
                        Vec::new(),
                        false,
                    )
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
                let pushed = push_workspace(
                    &mut ws,
                    push_options(false, false, staged),
                    Vec::new(),
                    false,
                )
                .await;
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
            asset,
            json,
        } => {
            let mut ws = open_workspace(&library, passphrase_stdin)?;
            push_workspace(&mut ws, push_options(dry_run, force, staged), asset, json).await?;
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
            restore_workspace(&ws, &restore_options(scope, out), json).await?;
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

        // ── Trash ─────────────────────────────────────────────────────────
        Commands::Trash { command } => {
            let (library, passphrase_stdin, json, request) = match command {
                TrashCommands::Delete {
                    library,
                    asset,
                    retain_days,
                    passphrase_stdin,
                    json,
                } => (
                    library,
                    passphrase_stdin,
                    json,
                    remote::TrashRequest {
                        action: remote::TrashAction::Delete,
                        assets: asset,
                        retain_days,
                    },
                ),
                TrashCommands::Restore {
                    library,
                    asset,
                    passphrase_stdin,
                    json,
                } => (
                    library,
                    passphrase_stdin,
                    json,
                    remote::TrashRequest {
                        action: remote::TrashAction::TrashRestore,
                        assets: asset,
                        retain_days: cull::DEFAULT_RETAIN_DAYS,
                    },
                ),
            };
            let mut ws = open_workspace(&library, passphrase_stdin)?;
            trash_workspace(&mut ws, &request, json).await?;
        }

        // ── Show ──────────────────────────────────────────────────────────
        Commands::Show {
            asset,
            library,
            passphrase_stdin,
            json,
        } => {
            let ws = open_workspace(&library, passphrase_stdin)?;
            let failed =
                |error: &show::ShowError| eyre!("Show failed: {}", show::describe_error(error));
            let id = show::resolve(&ws, &asset).map_err(|error| failed(&error))?;
            let missing = || failed(&show::ShowError::UnknownAsset(id.to_string()));
            if json {
                let report = show::report(&ws, &id).ok_or_else(missing)?;
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                let view = show::collect(&ws, &id).ok_or_else(missing)?;
                print!("{}", show::render(&view));
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

        // ── Label, Region, Edit ───────────────────────────────────────────
        Commands::Label { command } => {
            let (args, add) = match command {
                LabelCommands::Add(args) => (args, true),
                LabelCommands::Remove(args) => (args, false),
            };
            let LabelArgs {
                label,
                target,
                region,
                library,
            } = args;
            edit_library(&library, |ws| {
                let target = match (target.asset, target.stack, target.album, region) {
                    (Some(asset), _, _, Some(region_id)) => edit::LabelTarget::Region {
                        asset_id: show::resolve(ws, &asset)?,
                        region_id,
                    },
                    (Some(asset), _, _, None) => {
                        edit::LabelTarget::Asset(show::resolve(ws, &asset)?)
                    }
                    (None, Some(stack), _, _) => edit::LabelTarget::Stack(stack),
                    (None, None, Some(album), _) => edit::LabelTarget::Album(album),
                    (None, None, None, _) => {
                        unreachable!("clap requires one of --asset, --stack and --album")
                    }
                };
                Ok(if add {
                    edit::Edit::AddLabel { target, label }
                } else {
                    edit::Edit::RemoveLabel { target, label }
                })
            })?;
        }
        Commands::Region { command } => match command {
            RegionCommands::Add {
                asset,
                circle,
                rect,
                label,
                library,
            } => {
                let shape = circle
                    .or(rect)
                    .ok_or_else(|| eyre!("give --circle or --rect"))?;
                edit_library(&library, |ws| {
                    Ok(edit::Edit::AddRegion {
                        asset_id: show::resolve(ws, &asset)?,
                        shape,
                        labels: label,
                    })
                })?;
            }
            RegionCommands::Delete {
                region,
                asset,
                library,
            } => {
                edit_library(&library, |ws| {
                    Ok(edit::Edit::DeleteRegion {
                        asset_id: show::resolve(ws, &asset)?,
                        region_id: region,
                    })
                })?;
            }
        },
        Commands::Edit { command } => match command {
            EditCommands::CaptureTime {
                time,
                asset,
                library,
            } => {
                edit_library(&library, |ws| {
                    Ok(edit::Edit::SetCaptureTime {
                        asset_id: show::resolve(ws, &asset)?,
                        capture: time,
                    })
                })?;
            }
            EditCommands::Location {
                asset,
                lat,
                lon,
                datum,
                clear,
                library,
            } => {
                edit_library(&library, |ws| {
                    let asset_id = show::resolve(ws, &asset)?;
                    Ok(match (clear, lat, lon) {
                        (true, _, _) => edit::Edit::ClearLocation { asset_id },
                        (false, Some(lat), Some(lon)) => edit::Edit::SetLocation {
                            asset_id,
                            lat,
                            lon,
                            datum,
                        },
                        (false, _, _) => {
                            unreachable!("clap requires --lat and --lon unless --clear")
                        }
                    })
                })?;
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
                json,
            } => {
                let scope = match (album, asset) {
                    (_, Some(asset)) => capsule_core::sharing::ShareScope::Asset(asset),
                    (Some(album), None) => capsule_core::sharing::ShareScope::Album(album),
                    (None, None) => {
                        unreachable!("clap requires --album unless --asset is present")
                    }
                };
                share_create(ShareCreateRun {
                    library: &library,
                    scope,
                    expires_at,
                    passphrase_stdin,
                    protect,
                    link_passphrase_stdin,
                    json,
                })
                .await?;
            }
            ShareCommands::Revoke { opaque_id, json } => share_revoke(&opaque_id, json).await?,
        },

        // ── Peer ──────────────────────────────────────────────────────────
        Commands::Peer { command } => match command {
            PeerCommands::Delegate {
                library,
                to,
                asset,
                out,
                peer,
                deadline_hours,
                passphrase_stdin,
                json,
            } => {
                let mut options = match (peer, out, to) {
                    (Some(peer), _, to) => peer::DelegateOptions::over_peer(peer, to, asset),
                    (None, Some(out), Some(to)) => peer::DelegateOptions::new(to, asset, out),
                    _ => {
                        return Err(eyre!(
                            "Name where the hand-offs go: --out DIR with --to DEVICE_ID, or \
                             --peer HOST:PORT"
                        ));
                    }
                };
                options.deadline_ms = deadline_hours.saturating_mul(60 * 60 * 1000);
                let ws = open_workspace(&library, passphrase_stdin)?;
                peer_delegate(&ws, &options, json).await?;
            }
            PeerCommands::Accept {
                library,
                from,
                listen,
                listen_timeout,
                staged,
                passphrase_stdin,
                json,
            } => {
                let ws = open_workspace(&library, passphrase_stdin)?;
                let push = push_options(false, false, staged);
                let listen = match listen {
                    Some(address) => Some(
                        peer_listen(address, std::time::Duration::from_secs(listen_timeout))
                            .await?,
                    ),
                    None => None,
                };
                let options = peer::AcceptOptions {
                    from,
                    policy: push.policy,
                    connection: push.connection,
                    listen,
                };
                peer_accept(&ws, &options, json).await?;
            }
            PeerCommands::Status {
                library,
                ack,
                passphrase_stdin,
                json,
            } => {
                let ws = open_workspace(&library, passphrase_stdin)?;
                peer_status(&ws, &ack, json).await?;
            }
            PeerCommands::Discard {
                library,
                hand_off,
                passphrase_stdin,
                json,
            } => {
                let ws = open_workspace(&library, passphrase_stdin)?;
                peer_discard(&ws, &hand_off, json)?;
            }
            PeerCommands::Offer {
                library,
                peer,
                to,
                asset,
                passphrase_stdin,
                json,
            } => {
                let options = peer::OfferOptions {
                    peer,
                    to,
                    assets: asset.into_iter().collect(),
                };
                let ws = open_workspace(&library, passphrase_stdin)?;
                peer_offer(&ws, &options, json).await?;
            }
            PeerCommands::Pull {
                library,
                listen,
                listen_timeout,
                passphrase_stdin,
                json,
            } => {
                let mut ws = open_workspace(&library, passphrase_stdin)?;
                peer_pull(
                    &mut ws,
                    listen,
                    std::time::Duration::from_secs(listen_timeout),
                    json,
                )
                .await?;
            }
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
            json,
        } => {
            sync(SyncRun {
                dry_run,
                from_start: force,
                library: library.as_deref(),
                passphrase_stdin,
                json,
            })
            .await?;
        }

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
///
/// With `json`, stdout carries exactly one document, [`LoginReport`]; the progress lines and the
/// second-factor notice go to stderr, beside the prompts.
async fn auth_login(email: Option<String>, password_stdin: bool, json: bool) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;

    let email = flag_or_prompt(email, EMAIL_PROMPT.to_owned())?;
    let password = read_password(
        password_stdin.then_some("--password-stdin"),
        PASSWORD_PROMPT.to_owned(),
    )?;

    progress(json, "Logging in to Capsule…".green());
    let step = match remote::auth_login(&remote, &store, &email, &password).await {
        Ok(step) => step,
        Err(error) => return Err(login_failed(&describe_remote_error(&error))),
    };

    let second_factor = finish_second_factor(&remote, &store, step, json).await?;

    if json {
        let report = LoginReport {
            signed_in: true,
            email,
            second_factor,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", format!("Logged in as {email}.").green());
    }
    Ok(())
}

/// What `capsule auth login --json` prints once the session is stored.
#[derive(Debug, serde::Serialize)]
struct LoginReport {
    /// Always true: a sign-in that did not finish exits non-zero and prints no document.
    signed_in: bool,
    /// The account the session belongs to.
    email: String,
    /// Whether the sign-in asked for an authenticator code.
    second_factor: bool,
}

/// What `capsule auth logout --json` prints.
#[derive(Debug, serde::Serialize)]
struct LogoutReport {
    /// Whether a session was stored and is now gone; false when there was nothing to sign out.
    signed_out: bool,
}

/// What `capsule share revoke --json` prints.
#[derive(Debug, serde::Serialize)]
struct RevokeReport {
    /// The opaque id of the link that no longer opens.
    opaque_id: String,
    /// Always true: a refused revoke exits non-zero and prints no document.
    revoked: bool,
}

/// What `capsule sync --json` prints: the drain, and the re-announce a `--library` run made
/// after the server had rebuilt the library's index.
#[derive(Debug, serde::Serialize)]
struct SyncReport {
    #[serde(flatten)]
    sync: remote::SyncSummary,
    /// The re-announce report, when the drain met a rebuilt index and `--library` was given.
    reannounce: Option<reannounce::ReannounceSummary>,
}

/// A progress line for a person: on stdout normally, on stderr under `--json`, which owns
/// stdout for its one document.
fn progress(json: bool, line: impl std::fmt::Display) {
    if json {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
}

/// Finish a sign-in that answered a second-factor challenge, by asking for the code.
///
/// The first factor verified and the sign-in may not be finished. The CLI is
/// interactive, so it asks — a client that could not would have to report a second factor as
/// a failure, which is what the SDK's `LoginOutcome::into_session` is for and what this is
/// deliberately not. Shared by the password and the identity-provider sign-ins, which honour the
/// same factor.
///
/// Returns whether a code was asked for. With `json` the notice goes to stderr.
async fn finish_second_factor(
    remote: &remote::RemoteConfig,
    store: &session::SessionStore,
    step: remote::LoginStep,
    json: bool,
) -> Result<bool> {
    let remote::LoginStep::SecondFactorRequired { mfa_token } = step else {
        return Ok(false);
    };
    progress(
        json,
        "This account has two-factor authentication switched on.".yellow(),
    );
    let code = flag_or_prompt(None, "Authenticator code".to_owned())?;
    if let Err(error) = remote::auth_verify_totp(remote, store, &mfa_token, code.trim()).await {
        return Err(login_failed(&describe_remote_error(&error)));
    }
    Ok(true)
}

/// Provider-authored text with its control characters dropped, so a provider cannot write
/// escape sequences into the terminal: the one filter the CLI's prints and the TUI's account
/// screen both pass that text through.
pub(crate) fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// The line a browser sign-in prints above the authorization address.
pub(crate) const OIDC_OPEN_ADDRESS: &str = "To sign in, open this address in a browser:";

/// The line a browser sign-in prints when no browser could be opened for it.
pub(crate) const OIDC_NO_BROWSER: &str =
    "No browser could be opened from here. Open the address above in any browser.";

/// The line a device sign-in prints above the verification address.
pub(crate) const DEVICE_OPEN_ADDRESS: &str = "On any device with a browser, open this address:";

/// The line a device sign-in prints above the address that already carries the code.
pub(crate) const DEVICE_OPEN_COMPLETE: &str =
    "Or open this address, which already carries the code:";

/// The line a device sign-in prints carrying the code to enter.
pub(crate) fn device_code_line(user_code: &str) -> String {
    format!("and enter this code: {user_code}")
}

/// The line a browser sign-in prints while it waits, in whole minutes rounded up.
pub(crate) fn oidc_waiting_line(waited: std::time::Duration) -> String {
    match waited.as_secs().div_ceil(60) {
        1 => "Waiting up to 1 minute for the sign-in to finish…".to_owned(),
        count => format!("Waiting up to {count} minutes for the sign-in to finish…"),
    }
}

/// The line a device sign-in prints while it waits, in whole minutes rounded up.
pub(crate) fn device_waiting_line(waited: std::time::Duration) -> String {
    match waited.as_secs().div_ceil(60) {
        1 => "Waiting up to 1 minute for you to approve the sign-in…".to_owned(),
        count => format!("Waiting up to {count} minutes for you to approve the sign-in…"),
    }
}

/// The line an identity-provider sign-in prints when it succeeds.
const OIDC_SUCCESS: &str = "Signed in through your identity provider.";

/// The pages the loopback listener shows the browser: shared by `capsule auth login --oidc` and
/// the TUI's account screen, both of which finish the sign-in in the terminal.
pub(crate) fn oidc_callback_pages() -> oidc::CallbackPages {
    oidc::CallbackPages {
        received: "Capsule received your sign-in. Return to the terminal, which finishes signing \
                   you in."
            .to_owned(),
        refused: "The sign-in did not complete. Return to the terminal to see why.".to_owned(),
    }
}

/// What `capsule auth login --oidc [--device] --json` prints once the session is stored.
#[derive(Debug, serde::Serialize)]
struct OidcLoginReport {
    /// Always true: a sign-in that did not finish exits non-zero and prints no document.
    signed_in: bool,
    /// How the identity provider was reached: `browser` (`--oidc`) or `device` (`--device`).
    flow: &'static str,
    /// Whether the sign-in asked for an authenticator code.
    second_factor: bool,
}

/// Finish an identity-provider sign-in that settled as `step`: the second factor, then the
/// success line, or under `json` the one [`OidcLoginReport`] on stdout.
async fn finish_oidc_login(
    remote: &remote::RemoteConfig,
    store: &session::SessionStore,
    step: remote::LoginStep,
    flow: &'static str,
    json: bool,
) -> Result<()> {
    let second_factor = finish_second_factor(remote, store, step, json).await?;
    if json {
        let report = OidcLoginReport {
            signed_in: true,
            flow,
            second_factor,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", OIDC_SUCCESS.green());
    }
    Ok(())
}

/// `capsule auth login --oidc`: sign in through the server's identity provider in a browser,
/// over a loopback listener, and persist the session ([`oidc`] owns the flow).
///
/// With `json`, the address and the progress lines go to stderr and stdout carries exactly one
/// document, [`OidcLoginReport`].
async fn auth_login_oidc(no_browser: bool, json: bool) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let ceremony = oidc::SdkCeremony::new(&remote, &store)
        .map_err(|error| login_failed(&describe_remote_error(&error)))?;

    progress(
        json,
        "Signing in to Capsule through your identity provider…".green(),
    );
    let show = |authorization_url: &str, waited: std::time::Duration| {
        progress(json, OIDC_OPEN_ADDRESS);
        progress(json, authorization_url);
        if !no_browser && !oidc::open_in_browser(authorization_url) {
            progress(json, OIDC_NO_BROWSER.yellow());
        }
        progress(json, oidc_waiting_line(waited));
    };
    let step = oidc::login(
        &ceremony,
        show,
        &oidc_callback_pages(),
        oidc::LISTENER_TIMEOUT,
    )
    .await
    .map_err(|error| login_failed(&describe_oidc_error(&error)))?;

    finish_oidc_login(&remote, &store, step, "browser", json).await
}

/// `capsule auth login --oidc --device`: sign in through the server's identity provider from
/// another device (RFC 8628, issue #635) — print the address and the code, poll until the person
/// approves, and persist the session ([`oidc::login_device`] owns the flow).
///
/// With `json`, the address, the code and the progress lines go to stderr and stdout carries
/// exactly one document, [`OidcLoginReport`].
async fn auth_login_oidc_device(json: bool) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let ceremony = oidc::SdkCeremony::new(&remote, &store)
        .map_err(|error| login_failed(&describe_remote_error(&error)))?;

    progress(
        json,
        "Signing in to Capsule through your identity provider, from another device…".green(),
    );
    let show = |pending: &capsule_sdk::auth::OidcDeviceAuthorization,
                waited: std::time::Duration| {
        // Provider-authored, printed to a terminal: control characters are dropped, as the
        // server already refused any code carrying one.
        progress(json, DEVICE_OPEN_ADDRESS);
        progress(json, printable(&pending.verification_uri));
        progress(
            json,
            device_code_line(&printable(&pending.user_code)).bold(),
        );
        if let Some(complete) = &pending.verification_uri_complete {
            progress(json, DEVICE_OPEN_COMPLETE);
            progress(json, printable(complete));
        }
        progress(json, device_waiting_line(waited));
    };
    let step = oidc::login_device(&ceremony, show, oidc::DEVICE_TIMEOUT)
        .await
        .map_err(|error| login_failed(&describe_oidc_error(&error)))?;

    finish_oidc_login(&remote, &store, step, "device", json).await
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
pub(crate) fn describe_oidc_error(error: &oidc::OidcLoginError) -> String {
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
///
/// With `json`, stdout carries exactly one document, [`LogoutReport`].
async fn auth_logout(json: bool) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;

    progress(json, "Logging out from Capsule…".yellow());
    match remote::auth_logout(&remote, &store).await {
        Ok(signed_out) if json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&LogoutReport { signed_out })?
            );
            Ok(())
        }
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

/// The flags of one `capsule sync` run.
#[derive(Debug, Clone, Copy)]
struct SyncRun<'a> {
    /// Persist nothing.
    dry_run: bool,
    /// Discard the saved cursor (`--force`).
    from_start: bool,
    /// The library to re-announce from after a rebuilt index.
    library: Option<&'a Path>,
    /// Read that library's passphrase from stdin.
    passphrase_stdin: bool,
    /// Print one JSON document on stdout.
    json: bool,
}

/// `capsule sync`: drain the feed over the SDK into the local store.
///
/// When the server answers with a stale cursor, the drain starts over from zero (#697), and
/// what the library holds that the server lost is re-announced (#781): by this run when
/// `library` names the library, and otherwise by the `capsule sync reannounce` it names.
async fn sync(run: SyncRun<'_>) -> Result<()> {
    let SyncRun {
        dry_run,
        from_start,
        library,
        passphrase_stdin,
        json,
    } = run;
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let db = db::init_sqlite()
        .await
        .map_err(|e| eyre!("Failed to open local database: {e}"))?;

    progress(
        json,
        format!("Syncing from {}…", remote.sync_endpoint).green(),
    );
    if dry_run {
        progress(json, "Dry run: no changes will be persisted.".yellow());
    }

    let outcome = remote::sync(
        &remote,
        &store,
        &db,
        remote::DEFAULT_SYNC_PAGE_SIZE,
        dry_run,
        from_start,
    )
    .await;
    if json {
        // One document on stdout, or a non-zero exit with nothing on it.
        let sync = outcome.map_err(|error| sync_run_error(&error))?;
        let reannounce = match library {
            Some(library) if sync.resynced => {
                let ws = open_workspace(library, passphrase_stdin)?;
                Some(
                    remote::reannounce(&remote, &store, &ws, dry_run)
                        .await
                        .map_err(|error| {
                            eyre!("Re-announce failed: {}", describe_remote_error(&error))
                        })?,
                )
            }
            _ => None,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&SyncReport { sync, reannounce })?
        );
        return Ok(());
    }
    match outcome {
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
        Err(error) => Err(sync_run_error(&error)),
    }
}

/// A sync that could not run — no session, an unreadable feed — as the error the command exits
/// with.
fn sync_run_error(error: &remote::RemoteError) -> eyre::Report {
    if matches!(error, remote::RemoteError::NotAuthenticated) {
        return eyre!(NOT_AUTHENTICATED);
    }
    eyre!("Sync failed: {}", describe_remote_error(error))
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
/// When the open found sidecars no provenance chain anchors, the [`unanchored_notice`] goes to
/// stderr, so every verb over such a library says why some assets are missing.
fn open_workspace(library: &Path, passphrase_stdin: bool) -> Result<Workspace> {
    let passphrase = read_password(
        passphrase_stdin.then_some("--passphrase-stdin"),
        "Library passphrase".to_string(),
    )?;
    let ws = open_workspace_with(library, &passphrase)?;
    if let Some(notice) = unanchored_notice(ws.unanchored_sidecars().len()) {
        eprintln!("{}", notice.yellow());
    }
    Ok(ws)
}

/// Open the library `args` names, build an edit against it with `build` (which resolves the
/// asset selector), write it, and print what it did: one JSON document with `--json`, else one
/// line.
fn edit_library(
    args: &EditLibraryArgs,
    build: impl FnOnce(&Workspace) -> std::result::Result<edit::Edit, edit::EditError>,
) -> Result<()> {
    let mut ws = open_workspace(&args.library, args.passphrase_stdin)?;
    let report = build(&ws).and_then(|edit| edit::apply(&mut ws, &edit));
    match report {
        Ok(report) if args.json => println!("{}", serde_json::to_string_pretty(&report)?),
        Ok(report) => println!("{}", edit::summary(&report).green()),
        Err(error) => return Err(eyre!("Edit failed: {}", edit::describe_error(&error))),
    }
    Ok(())
}

/// [`open_workspace`] with the passphrase already in hand and without the unanchored-sidecar
/// notice: the TUI's forms collect the passphrase themselves.
fn open_workspace_with(library: &Path, passphrase: &str) -> Result<Workspace> {
    let mut ws = as_capsule_cli(
        Workspace::open(library, passphrase.as_bytes(), DeviceTier::Normal.params())
            .map_err(|e| eyre!("Failed to open signed workspace: {e}"))?,
    );
    let default_album = ws.default_album_id();
    ws.ensure_album(default_album, "Imports")
        .map_err(|e| eyre!("Failed to resolve the default album: {e}"))?;
    Ok(ws)
}

/// How a restore of `scope` into `out` runs, on the connection class the desktop source reports
/// now. A restore is a foreground request for the originals, so the class sizes its range
/// windows but never defers it.
fn restore_options(scope: restore::RestoreScope, out: PathBuf) -> restore::RestoreOptions {
    restore::RestoreOptions {
        scope,
        out,
        connection: cache::transfer_plan(&DesktopConnectionClass::new()).class,
        range_window: restore::RESTORE_RANGE_WINDOW,
    }
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
///
/// A non-empty `only` pushes just those assets (`--asset`): the retry of a failed asset, which
/// re-imports nothing.
async fn push_workspace(
    workspace: &mut Workspace,
    options: remote::PushOptions,
    only: Vec<uuid::Uuid>,
    json: bool,
) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let only = (!only.is_empty()).then(|| remote::distinct(only));
    let assets = only
        .as_ref()
        .map_or_else(|| workspace.asset_ids().len(), Vec::len);
    // An asset handed to another device and still inside its deadline is that device's to
    // upload; a re-queued one is pushed from here.
    let delegated = peer::delegated_for_push(workspace.device_id());
    let run = || remote::PushRun {
        assets: only.clone(),
        delegated: delegated.clone(),
        ..remote::PushRun::default()
    };

    if json {
        let summary = if assets == 0 {
            remote::PushSummary {
                dry_run: options.dry_run,
                ..Default::default()
            }
        } else {
            remote::push_run(&remote, &store, workspace, options, run())
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

    match remote::push_run(&remote, &store, workspace, options, run()).await {
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
            } else if summary.is_no_op()
                && summary.failed_assets == 0
                && summary.delegated_assets == 0
            {
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

/// Run one `capsule trash delete|restore` over `workspace` and report it: one JSON document
/// under `--json`, a summary line and one line per failed asset otherwise. A run with any
/// failed asset exits non-zero.
async fn trash_workspace(
    workspace: &mut Workspace,
    request: &remote::TrashRequest,
    json: bool,
) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let summary = remote::trash(&remote, &store, workspace, request)
        .await
        .map_err(|error| {
            if matches!(error, remote::RemoteError::NotAuthenticated) {
                return eyre!(NOT_AUTHENTICATED);
            }
            eyre!("trash failed: {}", describe_remote_error(&error))
        })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!("{}", remote::trash_line(&summary).cyan());
        for outcome in summary.failures() {
            eprintln!("{}", remote::trash_failure_line(outcome).red());
        }
    }
    if summary.failed > 0 {
        return Err(eyre!(
            "trash failed: {} of {} assets were not applied",
            summary.failed,
            summary.outcomes.len()
        ));
    }
    Ok(())
}

/// The flags of one `capsule share create` run.
#[derive(Debug)]
struct ShareCreateRun<'a> {
    /// The library whose keys open the covered assets.
    library: &'a Path,
    /// What the link grants.
    scope: capsule_core::sharing::ShareScope,
    /// `--expires-at` as typed.
    expires_at: Option<String>,
    /// Read the library passphrase from stdin.
    passphrase_stdin: bool,
    /// Protect the link with a passphrase.
    protect: bool,
    /// Read that link passphrase from stdin.
    link_passphrase_stdin: bool,
    /// Print one JSON document on stdout.
    json: bool,
}

/// `capsule share create`: issue a view-only link over `scope` and print its URL.
///
/// With `json`, stdout carries exactly one document, the [`share::IssuedShare`].
async fn share_create(run: ShareCreateRun<'_>) -> Result<()> {
    let ShareCreateRun {
        library,
        scope,
        expires_at,
        passphrase_stdin,
        protect,
        link_passphrase_stdin,
        json,
    } = run;
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

    progress(
        json,
        format!("Issuing a share link on {}…", remote.sync_endpoint).green(),
    );
    match share::create(&remote, &store, &mut ws, &request, jiff::Timestamp::now()).await {
        Ok(issued) if json => {
            println!("{}", serde_json::to_string_pretty(&issued)?);
            Ok(())
        }
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
///
/// With `json`, stdout carries exactly one document, [`RevokeReport`].
async fn share_revoke(opaque_id: &str, json: bool) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    match share::revoke(&remote, &store, opaque_id).await {
        Ok(opaque_id) if json => {
            let report = RevokeReport {
                opaque_id,
                revoked: true,
            };
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
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

/// A whole-run `capsule peer` failure, as the line the process exits with.
fn peer_run_error(command: &str, error: &peer::PeerError) -> eyre::Report {
    match error {
        peer::PeerError::Remote(remote::RemoteError::NotAuthenticated) => eyre!(NOT_AUTHENTICATED),
        peer::PeerError::Remote(remote) => {
            eyre!("{command} failed: {}", describe_remote_error(remote))
        }
        other => eyre!("{command} failed: {other}"),
    }
}

/// The directory floor `workspace`'s device persists in the CLI database, with any floor an
/// earlier build kept in a file under `state` adopted into it. An unreadable store, or one that
/// has lost the floor while `state` survives, refuses the run rather than starting the floor over.
async fn peer_floor(
    workspace: &Workspace,
    state: &peer::PeerState,
    command: &str,
) -> Result<peering_state::DbDirectoryVersions> {
    let db = db::init_sqlite()
        .await
        .map_err(|e| eyre!("Failed to open local database: {e}"))?;
    let versions = peering_state::DbDirectoryVersions::open(db, workspace.device_id())
        .await
        .map_err(|error| eyre!("{command} failed: the directory floor: {error}"))?;
    versions
        .adopt_legacy_file(&state.legacy_floor_path())
        .await
        .map_err(|error| eyre!("{command} failed: the directory floor: {error}"))?;
    versions
        .refuse_lost_floor(state.root())
        .await
        .map_err(|error| eyre!("{command} failed: the directory floor: {error}"))?;
    Ok(versions)
}

/// `capsule peer delegate`: seal each asset for the other device and say where it went.
async fn peer_delegate(
    workspace: &Workspace,
    options: &peer::DelegateOptions,
    json: bool,
) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let state = peer::PeerState::for_device(workspace.device_id())
        .map_err(|error| peer_run_error("Hand-off", &error))?;
    let versions = peer_floor(workspace, &state, "Hand-off").await?;
    let summary = peer::delegate(
        &remote,
        &store,
        workspace,
        &state,
        &versions,
        &peer::SystemBootClock::default(),
        options,
    )
    .await
    .map_err(|error| peer_run_error("Hand-off", &error))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        let delegate = summary.delegate.map_or_else(
            || "the other device".to_owned(),
            |id| format!("device {id}"),
        );
        for handed in &summary.handed_off {
            let via = handed.file.as_deref().map_or_else(
                || "acknowledged over the peering channel".to_owned(),
                str::to_owned,
            );
            println!(
                "{}",
                format!(
                    "{}: handed to {delegate}, awaiting server ({via})",
                    handed.asset_id
                )
                .green()
            );
        }
        for failed in &summary.failed {
            println!(
                "{}",
                format!("{}: not handed off: {}", failed.subject, failed.message).red()
            );
        }
    }
    if summary.failed.is_empty() {
        Ok(())
    } else {
        Err(eyre!(
            "Hand-off failed for {} asset(s)",
            summary.failed.len()
        ))
    }
}

/// Bind `capsule peer accept --listen`, and say on stderr where it listens (stdout stays the
/// `--json` document).
async fn peer_listen(
    address: std::net::SocketAddr,
    timeout: std::time::Duration,
) -> Result<peer::PeerListener> {
    let listener = peer::PeerListener::bind(address, timeout)
        .await
        .map_err(|error| peer_run_error("Accept", &error))?;
    let bound = listener
        .local_addr()
        .map_err(|error| peer_run_error("Accept", &error))?;
    eprintln!(
        "Listening for a peer on {bound} for up to {}s",
        timeout.as_secs()
    );
    Ok(listener)
}

/// `capsule peer accept`: take the files, then upload what this device holds.
async fn peer_accept(
    workspace: &Workspace,
    options: &peer::AcceptOptions,
    json: bool,
) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let state = peer::PeerState::for_device(workspace.device_id())
        .map_err(|error| peer_run_error("Accept", &error))?;
    let versions = peer_floor(workspace, &state, "Accept").await?;
    let summary = peer::accept(&remote, &store, workspace, &state, &versions, options)
        .await
        .map_err(|error| peer_run_error("Accept", &error))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        for accepted in &summary.accepted {
            let line = match (&accepted.ack_file, accepted.peer, accepted.acknowledged) {
                (Some(ack_file), _, _) => format!(
                    "{}: taken; acknowledgement written to {ack_file}",
                    accepted.ack.asset_id
                ),
                (None, Some(peer), Some(false)) => format!(
                    "{}: taken from device {peer} over the peering channel and held for upload; \
                     the device did not take the acknowledgement",
                    accepted.ack.asset_id
                ),
                (None, peer, _) => format!(
                    "{}: taken from device {} over the peering channel; acknowledged",
                    accepted.ack.asset_id,
                    peer.map_or_else(|| "?".to_owned(), |id| id.to_string())
                ),
            };
            println!("{}", line.green());
        }
        for refused in &summary.refused {
            println!(
                "{}",
                format!("{}: refused: {}", refused.subject, refused.message).red()
            );
        }
        for upload in &summary.uploads {
            let line = match upload.outcome {
                "removed" => format!(
                    "{}: the server holds it; this device's copy is deleted",
                    upload.asset_id
                ),
                "kept" => format!(
                    "{}: uploaded what this connection allows; kept until the server holds it ({})",
                    upload.asset_id,
                    upload.kind.unwrap_or_default()
                ),
                "quarantined" => format!(
                    "{}: permanently refused; this device's copy is quarantined and kept, and \
                     is not retried (see `capsule peer status`; `capsule peer discard` deletes \
                     it): {}",
                    upload.hand_off,
                    upload.message.as_deref().unwrap_or_default()
                ),
                _ => format!(
                    "{}: upload failed; kept for the next run: {}",
                    upload.hand_off,
                    upload.message.as_deref().unwrap_or_default()
                ),
            };
            println!("{line}");
        }
    }
    if summary.has_failures() {
        Err(eyre!("Accept finished with refused or failed hand-offs"))
    } else {
        Ok(())
    }
}

/// `capsule peer status`: one line per handed-off asset.
async fn peer_status(workspace: &Workspace, acks: &[PathBuf], json: bool) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let state = peer::PeerState::for_device(workspace.device_id())
        .map_err(|error| peer_run_error("Status", &error))?;
    let versions = peer_floor(workspace, &state, "Status").await?;
    let summary = peer::status(
        &remote,
        &store,
        workspace,
        &state,
        &versions,
        &peer::SystemBootClock::default(),
        acks,
    )
    .await
    .map_err(|error| peer_run_error("Status", &error))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        if summary.delegations.is_empty() {
            println!("{}", "Nothing is handed to another device.".yellow());
        }
        for view in &summary.delegations {
            println!("{}: {}", view.asset_id, view.status);
        }
        for refused in &summary.refused_acks {
            println!(
                "{}",
                format!(
                    "{}: acknowledgement refused: {}",
                    refused.subject, refused.message
                )
                .red()
            );
        }
        for held in &summary.quarantined {
            println!(
                "{}",
                format!(
                    "{}: quarantined hand-off for asset {} ({}): {}; kept at {}",
                    held.hand_off, held.asset_id, held.kind, held.message, held.file
                )
                .yellow()
            );
        }
        for delete in &summary.deletes {
            let line = match delete.outcome {
                "held" => format!(
                    "{}: delete held until the server holds the asset",
                    delete.asset_id
                )
                .yellow(),
                "sent" => format!(
                    "{}: {} sent",
                    delete.asset_id,
                    delete.action.as_deref().unwrap_or("delete")
                )
                .green(),
                "refused" => format!(
                    "{}: {} refused by the server, not retried; the library keeps it: {}",
                    delete.asset_id,
                    delete.action.as_deref().unwrap_or("delete"),
                    delete.message.as_deref().unwrap_or("no detail")
                )
                .red(),
                _ => format!(
                    "{}: {} not applied, kept for the next run: {}",
                    delete.asset_id,
                    delete.action.as_deref().unwrap_or("delete"),
                    delete.message.as_deref().unwrap_or("no detail")
                )
                .red(),
            };
            println!("{line}");
        }
    }
    if summary.refused_acks.is_empty() {
        Ok(())
    } else {
        Err(eyre!("Some acknowledgements were refused"))
    }
}

/// `capsule peer discard`: delete one quarantined hand-off.
fn peer_discard(workspace: &Workspace, hand_off: &str, json: bool) -> Result<()> {
    let state = peer::PeerState::for_device(workspace.device_id())
        .map_err(|error| peer_run_error("Discard", &error))?;
    let discarded =
        peer::discard(&state, hand_off).map_err(|error| peer_run_error("Discard", &error))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&discarded)?);
    } else {
        println!(
            "{}",
            format!(
                "{}: quarantined hand-off for asset {} deleted",
                discarded.hand_off, discarded.asset_id
            )
            .green()
        );
    }
    Ok(())
}

/// `capsule peer offer`: send this library's assets to the device at `--peer`.
async fn peer_offer(workspace: &Workspace, options: &peer::OfferOptions, json: bool) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let state = peer::PeerState::for_device(workspace.device_id())
        .map_err(|error| peer_run_error("Offer", &error))?;
    let versions = peer_floor(workspace, &state, "Offer").await?;
    let summary = peer::offer(&remote, &store, workspace, &versions, options)
        .await
        .map_err(|error| peer_run_error("Offer", &error))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        if let (Some(receiver), Some(artifact)) = (summary.receiver, &summary.artifact) {
            println!(
                "{}",
                format!(
                    "Offered {} asset(s) to device {receiver} as {artifact} ({} bytes); it \
                     pulled {} bytes",
                    summary.assets.len(),
                    summary.bytes,
                    summary.served_bytes
                )
                .green()
            );
        }
        for failed in &summary.failed {
            println!(
                "{}",
                format!("{}: not offered: {}", failed.subject, failed.message).red()
            );
        }
    }
    if summary.failed.is_empty() {
        Ok(())
    } else {
        Err(eyre!(
            "Offer finished with {} failure(s)",
            summary.failed.len()
        ))
    }
}

/// `capsule peer pull`: take what the device that dials in offers, against the tombstones this
/// device persisted.
async fn peer_pull(
    workspace: &mut Workspace,
    listen: std::net::SocketAddr,
    timeout: std::time::Duration,
    json: bool,
) -> Result<()> {
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let state = peer::PeerState::for_device(workspace.device_id())
        .map_err(|error| peer_run_error("Pull", &error))?;
    let db = db::init_sqlite()
        .await
        .map_err(|e| eyre!("Failed to open local database: {e}"))?;
    // Read before anything binds or dials: a set that cannot be read refuses the run.
    peer::ingest_tombstones(&db)
        .await
        .map_err(|error| peer_run_error("Pull", &error))?;
    let versions = peer_floor(workspace, &state, "Pull").await?;
    let listener = peer::PeerListener::bind(listen, timeout)
        .await
        .map_err(|error| peer_run_error("Pull", &error))?;
    let bound = listener
        .local_addr()
        .map_err(|error| peer_run_error("Pull", &error))?;
    eprintln!(
        "Listening for a peer on {bound} for up to {}s",
        timeout.as_secs()
    );
    let summary = peer::pull(&remote, &store, workspace, &db, &versions, &listener)
        .await
        .map_err(|error| peer_run_error("Pull", &error))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        let peer = summary
            .peer
            .map_or_else(|| "the peer".to_owned(), |id| format!("device {id}"));
        println!(
            "{}",
            format!(
                "From {peer}: {} applied, {} already held, {} refused as purged here, {} \
                 quarantined as stale, {} incomplete",
                summary.applied.len(),
                summary.identical.len(),
                summary.tombstoned.len(),
                summary.quarantined.len(),
                summary.incomplete.len()
            )
            .green()
        );
        for failed in summary.failed.iter().chain(&summary.refused) {
            println!(
                "{}",
                format!("{}: {}", failed.subject, failed.message).red()
            );
        }
    }
    if summary.has_failures() {
        Err(eyre!(
            "Pull finished with {} failure(s)",
            summary.failed.len()
        ))
    } else {
        Ok(())
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
mod library_info_tests;

#[cfg(test)]
mod escrow_tests;

#[cfg(test)]
mod oidc_login_tests;

#[cfg(test)]
mod stdin_secret_tests;

#[cfg(test)]
mod client_build_wiring_tests;
