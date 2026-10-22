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
use capsule_core::import::{
    CancellationToken, DefaultAlbumContext, ImportConfig, ImportOutcome, ImportProgressEvent,
    ScanResult, SourceAdapter, SourceMetadataIndex, TakeoutAdapter, UploadPolicy,
    execute_with_source_metadata, plan, scan_paths as scan_files,
};
use capsule_core::library::{Library, LibraryError, init_library, open_library, rebuild_index};
use capsule_core::lifecycle::{Workspace, unmigrated_sidecars};
use capsule_core::metadata::FileMetadata;
use capsule_sdk::net::ConnectionClass;
use cli::{
    AuthCommands, Cli, Commands, ImportProviderArg, LibraryCommands, RepairCommands, ShareCommands,
};
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
pub mod migrate;
pub mod oidc;
pub mod remote;
pub mod repair;
pub mod restore;
pub mod session;
pub mod share;
pub mod show;
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

/// The `capsule library info` row for the unmigrated-sidecar count, plus the pointer at
/// `capsule library migrate` — `None` when nothing is left to migrate. The count comes from
/// the keyless scan, since `info` opens the library without a passphrase.
#[must_use]
fn library_info_unmigrated(bundle: &Bundle, unmigrated: usize) -> (String, Option<String>) {
    let count = [("count", Value::Int(as_count(unmigrated)))];
    let row = bundle.format(keys::LIBRARY_INFO_UNMIGRATED_SIDECARS, &count);
    let hint = (unmigrated > 0).then(|| bundle.format(keys::LIBRARY_INFO_MIGRATE_HINT, &count));
    (row, hint)
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
/// [source adapter]: capsule_core::import::TakeoutAdapter
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
///
/// The parser is built from the derive and then localized (`S-I8`): every `--help` string is
/// resolved through the bundle negotiated from the process locale before a single argument is
/// read, so usage errors and help pages speak the user's language. A locale with no catalog
/// entry for a string falls back to the derive's English rather than printing a key.
pub async fn run() -> Result<()> {
    let command = cli::help::localize(
        <Cli as clap::CommandFactory>::command(),
        &i18n::cli_bundle(),
    );
    // Kept for error formatting: a derive/matches mismatch is reported the way clap itself
    // reports one — with the (localized) command's usage — rather than as a bare message.
    let mut for_errors = command.clone();
    let mut matches = command.get_matches();
    let cli = match <Cli as clap::FromArgMatches>::from_arg_matches_mut(&mut matches) {
        Ok(cli) => cli,
        Err(error) => error.format(&mut for_errors).exit(),
    };
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
                let (count_line, hint) = library_info_unmigrated(&i18n::cli_bundle(), unmigrated);
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
                let bundle = i18n::cli_bundle();
                let mut ws = open_workspace_quiet(&path, passphrase_stdin)?;
                println!(
                    "{}",
                    bundle
                        .format(
                            keys::LIBRARY_MIGRATE_IN_PROGRESS,
                            &[
                                (
                                    "count",
                                    Value::Int(as_count(ws.unmigrated_sidecars().len()))
                                ),
                                ("library", Value::Str(&path.to_string_lossy())),
                            ],
                        )
                        .cyan()
                );
                match migrate::run(&mut ws, retain_days) {
                    Ok(report) => print!(
                        "{}",
                        migrate::render(&bundle, &report, ws.unmigrated_sidecars().len())
                    ),
                    Err(error) => {
                        return Err(eyre!(
                            "{}",
                            bundle.format(
                                keys::LIBRARY_MIGRATE_FAILED,
                                &[("reason", Value::Str(&error.to_string()))]
                            )
                        ));
                    }
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
                    return push_workspace(&ws, push_options(false, false, staged), false).await;
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
                push_workspace(&ws, push_options(false, false, staged), false).await?;
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
            let ws = open_workspace(&library, passphrase_stdin)?;
            push_workspace(&ws, push_options(dry_run, force, staged), json).await?;
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
                // As for push: the CLI has no link-quality probe, so it reports the class a
                // desktop or server run is on.
                connection: ConnectionClass::Unmetered,
                range_window: restore::RESTORE_RANGE_WINDOW,
            };
            restore_workspace(&ws, &options, json).await?;
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

        // ── Show ──────────────────────────────────────────────────────────
        Commands::Show {
            asset,
            library,
            passphrase_stdin,
        } => {
            let bundle = i18n::cli_bundle();
            let ws = open_workspace(&library, passphrase_stdin)?;
            let view = show::resolve(&ws, &asset).and_then(|id| {
                show::collect(&ws, &id).ok_or_else(|| show::ShowError::UnknownAsset(id.to_string()))
            });
            match view {
                Ok(view) => print!("{}", show::render(&bundle, &view)),
                Err(error) => {
                    let reason = show::describe_error(&bundle, &error);
                    return Err(eyre!(
                        "{}",
                        bundle.format(keys::SHOW_FAILED, &[("reason", Value::Str(&reason))])
                    ));
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
                let bundle = i18n::cli_bundle();
                // `--limit` bounds what `--apply` writes; alone it would silently do nothing.
                if limit.is_some() && !apply {
                    return Err(eyre!(
                        "{}",
                        bundle.format(keys::REPAIR_CAPTURE_TIME_LIMIT_REQUIRES_APPLY, &[])
                    ));
                }
                let request = repair::RepairRequest {
                    apply,
                    limit: limit.map(|n| usize::try_from(n).unwrap_or(usize::MAX)),
                };
                let mut ws = open_workspace(&library, passphrase_stdin)?;
                match repair::run(&mut ws, request) {
                    Ok(summary) => print!("{}", repair::render(&bundle, request, &summary)),
                    Err(error) => {
                        let reason = repair::describe_error(&bundle, &error);
                        return Err(eyre!(
                            "{}",
                            bundle.format(keys::REPAIR_FAILED, &[("reason", Value::Str(&reason))])
                        ));
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
        return Err(eyre!(
            "{}",
            i18n::cli_bundle().format(keys::STDIN_EMPTY, &[("flag", Value::Str(flag))])
        ));
    }
    Ok(secret)
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
        password_stdin.then_some("--password-stdin"),
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
        password_stdin.then_some("--password-stdin"),
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

    finish_second_factor(&bundle, &remote, &store, step).await?;

    println!(
        "{}",
        bundle
            .format(keys::AUTH_LOGIN_SUCCESS, &[("email", Value::Str(&email))])
            .green()
    );
    Ok(())
}

/// Finish a sign-in that answered a second-factor challenge, by asking for the code.
///
/// The first factor verified and the sign-in may not be finished (`S-C55`). The CLI is
/// interactive, so it asks — a client that could not would have to report a second factor as
/// a failure, which is what the SDK's `LoginOutcome::into_session` is for and what this is
/// deliberately not. Shared by the password and the identity-provider sign-ins, which honour the
/// same factor.
async fn finish_second_factor(
    bundle: &capsule_i18n::Bundle,
    remote: &remote::RemoteConfig,
    store: &session::SessionStore,
    step: remote::LoginStep,
) -> Result<()> {
    if let remote::LoginStep::SecondFactorRequired { mfa_token } = step {
        println!(
            "{}",
            bundle
                .format(keys::AUTH_LOGIN_SECOND_FACTOR_REQUIRED, &[])
                .yellow()
        );
        let code = flag_or_prompt(None, bundle.format(keys::AUTH_LOGIN_TOTP_PROMPT, &[]))?;
        if let Err(error) = remote::auth_verify_totp(remote, store, &mfa_token, code.trim()).await {
            let reason = describe_remote_error(bundle, &error);
            return Err(eyre!(
                "{}",
                bundle.format(keys::AUTH_LOGIN_FAILED, &[("reason", Value::Str(&reason))])
            ));
        }
    }
    Ok(())
}

/// `capsule auth login --oidc`: sign in through the server's identity provider in a browser,
/// over a loopback listener, and persist the session ([`oidc`] owns the flow).
async fn auth_login_oidc(no_browser: bool) -> Result<()> {
    let bundle = i18n::cli_bundle();
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let ceremony = oidc::SdkCeremony::new(&remote, &store)
        .map_err(|error| login_failed(&bundle, &describe_remote_error(&bundle, &error)))?;
    let pages = oidc::CallbackPages {
        received: bundle.format(keys::AUTH_LOGIN_OIDC_PAGE_RECEIVED, &[]),
        refused: bundle.format(keys::AUTH_LOGIN_OIDC_PAGE_REFUSED, &[]),
    };

    println!(
        "{}",
        bundle
            .format(keys::AUTH_LOGIN_OIDC_IN_PROGRESS, &[])
            .green()
    );
    let show = |authorization_url: &str, waited: std::time::Duration| {
        println!("{}", bundle.format(keys::AUTH_LOGIN_OIDC_VISIT, &[]));
        println!("{authorization_url}");
        if !no_browser && !oidc::open_in_browser(authorization_url) {
            println!(
                "{}",
                bundle
                    .format(keys::AUTH_LOGIN_OIDC_BROWSER_UNAVAILABLE, &[])
                    .yellow()
            );
        }
        println!(
            "{}",
            bundle.format(
                keys::AUTH_LOGIN_OIDC_WAITING,
                &[(
                    "count",
                    Value::Int(i64::try_from(waited.as_secs().div_ceil(60)).unwrap_or(i64::MAX))
                )]
            )
        );
    };
    let step = oidc::login(&ceremony, show, &pages, oidc::LISTENER_TIMEOUT)
        .await
        .map_err(|error| login_failed(&bundle, &describe_oidc_error(&bundle, &error)))?;

    finish_second_factor(&bundle, &remote, &store, step).await?;
    println!(
        "{}",
        bundle.format(keys::AUTH_LOGIN_OIDC_SUCCESS, &[]).green()
    );
    Ok(())
}

/// `capsule auth login --oidc --device`: sign in through the server's identity provider from
/// another device (RFC 8628, issue #635) — print the address and the code, poll until the person
/// approves, and persist the session ([`oidc::login_device`] owns the flow).
async fn auth_login_oidc_device() -> Result<()> {
    let bundle = i18n::cli_bundle();
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    let ceremony = oidc::SdkCeremony::new(&remote, &store)
        .map_err(|error| login_failed(&bundle, &describe_remote_error(&bundle, &error)))?;

    println!(
        "{}",
        bundle
            .format(keys::AUTH_LOGIN_OIDC_DEVICE_IN_PROGRESS, &[])
            .green()
    );
    let show = |pending: &capsule_sdk::auth::OidcDeviceAuthorization,
                waited: std::time::Duration| {
        // Provider-authored, printed to a terminal: control characters are dropped, as the
        // server already refused any code carrying one.
        let printable = |text: &str| text.chars().filter(|c| !c.is_control()).collect::<String>();
        println!("{}", bundle.format(keys::AUTH_LOGIN_OIDC_DEVICE_VISIT, &[]));
        println!("{}", printable(&pending.verification_uri));
        println!(
            "{}",
            bundle
                .format(
                    keys::AUTH_LOGIN_OIDC_DEVICE_CODE,
                    &[("code", Value::Str(&printable(&pending.user_code)))]
                )
                .bold()
        );
        if let Some(complete) = &pending.verification_uri_complete {
            println!(
                "{}",
                bundle.format(keys::AUTH_LOGIN_OIDC_DEVICE_COMPLETE, &[])
            );
            println!("{}", printable(complete));
        }
        println!(
            "{}",
            bundle.format(
                keys::AUTH_LOGIN_OIDC_DEVICE_WAITING,
                &[(
                    "count",
                    Value::Int(i64::try_from(waited.as_secs().div_ceil(60)).unwrap_or(i64::MAX))
                )]
            )
        );
    };
    let step = oidc::login_device(&ceremony, show, oidc::DEVICE_TIMEOUT)
        .await
        .map_err(|error| login_failed(&bundle, &describe_oidc_error(&bundle, &error)))?;

    finish_second_factor(&bundle, &remote, &store, step).await?;
    println!(
        "{}",
        bundle.format(keys::AUTH_LOGIN_OIDC_SUCCESS, &[]).green()
    );
    Ok(())
}

/// The `capsule auth login` failure line, carrying `reason`.
fn login_failed(bundle: &capsule_i18n::Bundle, reason: &str) -> eyre::Report {
    eyre!(
        "{}",
        bundle.format(keys::AUTH_LOGIN_FAILED, &[("reason", Value::Str(reason))])
    )
}

/// A localized reason for an OIDC sign-in failure.
///
/// A refused loopback redirect gets the remedy rather than only the code: the server's operator
/// has to set `OIDC_ALLOW_LOOPBACK_REDIRECT=true`, which is off by default, and nothing the person
/// at the terminal does will change that.
fn describe_oidc_error(bundle: &capsule_i18n::Bundle, error: &oidc::OidcLoginError) -> String {
    match error {
        oidc::OidcLoginError::Remote(remote::RemoteError::Auth(
            capsule_sdk::auth::AuthError::OidcRedirectInvalid,
        )) => bundle.format(keys::AUTH_LOGIN_OIDC_LOOPBACK_REFUSED, &[]),
        oidc::OidcLoginError::Remote(error) => describe_remote_error(bundle, error),
        oidc::OidcLoginError::Listener(error) => bundle.format(
            keys::AUTH_LOGIN_OIDC_LISTENER_FAILED,
            &[("reason", Value::Str(&error.to_string()))],
        ),
        oidc::OidcLoginError::TimedOut { .. } => {
            bundle.format(keys::AUTH_LOGIN_OIDC_TIMED_OUT, &[])
        }
        oidc::OidcLoginError::DeviceTimedOut { .. } => {
            bundle.format(keys::AUTH_LOGIN_OIDC_DEVICE_TIMED_OUT, &[])
        }
        oidc::OidcLoginError::Denied { error, description } => {
            // Provider-authored text, printed to a terminal: control characters are dropped so
            // a provider cannot write escape sequences into it.
            let printable =
                |text: &str| text.chars().filter(|c| !c.is_control()).collect::<String>();
            let error = printable(error);
            match description {
                Some(description) => bundle.format(
                    keys::AUTH_LOGIN_OIDC_DENIED_DESCRIBED,
                    &[
                        ("error", Value::Str(&error)),
                        ("description", Value::Str(&printable(description))),
                    ],
                ),
                None => bundle.format(
                    keys::AUTH_LOGIN_OIDC_DENIED,
                    &[("error", Value::Str(&error))],
                ),
            }
        }
    }
}

/// `capsule auth escrow`: set recovery up for the library at `library`, as the signed-in
/// account (#624). [`remote::auth_escrow`] owns the flow; this is its terminal: the library
/// passphrase, the one showing of the minted secret, and the type-back gate.
async fn auth_escrow(library: &Path, passphrase_stdin: bool, replace: bool) -> Result<()> {
    let bundle = i18n::cli_bundle();
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;

    let passphrase = read_password(
        passphrase_stdin.then_some("--passphrase-stdin"),
        bundle.format(keys::AUTH_ESCROW_PASSPHRASE_PROMPT, &[]),
    )?;
    let mut workspace = as_capsule_cli(
        Workspace::open(library, passphrase.as_bytes(), DeviceTier::Normal.params()).map_err(
            |e| {
                let reason = e.to_string();
                eyre!(
                    "{}",
                    bundle.format(keys::AUTH_ESCROW_FAILED, &[("reason", Value::Str(&reason))])
                )
            },
        )?,
    );

    let path = library.display().to_string();
    println!(
        "{}",
        bundle
            .format(
                keys::AUTH_ESCROW_IN_PROGRESS,
                &[("path", Value::Str(&path))]
            )
            .green()
    );
    let confirm = |secret: &capsule_sdk::recovery::MintedSecret| {
        let hex = secret.to_hex();
        println!(
            "{}",
            bundle.format(
                keys::AUTH_ESCROW_SECRET_SHOWN,
                &[("secret", Value::Str(&hex))]
            )
        );
        match Input::<String>::new()
            .with_prompt(bundle.format(keys::AUTH_ESCROW_TYPEBACK_PROMPT, &[]))
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
                bundle
                    .format(keys::AUTH_ESCROW_SUCCESS, &[("path", Value::Str(&path))])
                    .green()
            );
            if setup.replaced {
                println!(
                    "{}",
                    bundle.format(keys::AUTH_ESCROW_REPLACED, &[]).yellow()
                );
            }
            Ok(())
        }
        Err(remote::RemoteError::NotAuthenticated) => Err(eyre!(
            "{}",
            bundle.format(keys::AUTH_NOT_AUTHENTICATED, &[])
        )),
        Err(error) => Err(eyre!("{}", describe_escrow_error(&bundle, &error))),
    }
}

/// The line `capsule auth escrow` prints for a refusal: the three it names itself (an escrow
/// already stored, the type-back gate, a library that already holds assets), and the
/// generic failure line wrapping any other reason.
fn describe_escrow_error(bundle: &Bundle, error: &remote::RemoteError) -> String {
    match error {
        remote::RemoteError::EscrowAlreadyStored => {
            bundle.format(keys::AUTH_ESCROW_ALREADY_STORED, &[])
        }
        remote::RemoteError::RecoverySecretNotConfirmed => {
            bundle.format(keys::AUTH_ESCROW_TYPEBACK_MISMATCH, &[])
        }
        remote::RemoteError::Lifecycle(capsule_core::lifecycle::LifecycleError::AccountRebind(
            _,
        )) => bundle.format(keys::AUTH_ESCROW_LIBRARY_HAS_ASSETS, &[]),
        other => {
            let reason = describe_remote_error(bundle, other);
            bundle.format(keys::AUTH_ESCROW_FAILED, &[("reason", Value::Str(&reason))])
        }
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
    let bundle = i18n::cli_bundle();
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;

    let secret = read_password(
        recovery_secret_stdin.then_some("--recovery-secret-stdin"),
        bundle.format(keys::AUTH_RECOVER_SECRET_PROMPT, &[]),
    )?;
    let passphrase = if passphrase_stdin {
        read_password(Some("--passphrase-stdin"), String::new())?
    } else {
        Password::new()
            .with_prompt(bundle.format(keys::AUTH_RECOVER_PASSPHRASE_PROMPT, &[]))
            .with_confirmation(
                bundle.format(keys::AUTH_RECOVER_PASSPHRASE_CONFIRM, &[]),
                bundle.format(keys::AUTH_RECOVER_PASSPHRASE_MISMATCH, &[]),
            )
            .interact()
            .map_err(|e| eyre!("Failed to read password: {e}"))?
    };

    let path = library.display().to_string();
    println!(
        "{}",
        bundle
            .format(
                keys::AUTH_RECOVER_IN_PROGRESS,
                &[("path", Value::Str(&path))]
            )
            .green()
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
            let device = workspace.device_id().to_string();
            println!(
                "{}",
                bundle
                    .format(
                        keys::AUTH_RECOVER_SUCCESS,
                        &[("path", Value::Str(&path)), ("device", Value::Str(&device))],
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
            let reason = match error {
                remote::RemoteError::WrongRecoverySecret => {
                    bundle.format(keys::AUTH_RECOVER_WRONG_SECRET, &[])
                }
                other => describe_remote_error(&bundle, &other),
            };
            Err(eyre!(
                "{}",
                bundle.format(
                    keys::AUTH_RECOVER_FAILED,
                    &[("reason", Value::Str(&reason))]
                )
            ))
        }
    }
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
///
/// When the open found unsigned pre-signed-path sidecars (`S-D24`), a notice naming their count
/// and `capsule library migrate` goes to stderr, so every verb over such a library says why
/// some assets are missing before the user runs the migration.
fn open_workspace(library: &Path, passphrase_stdin: bool) -> Result<Workspace> {
    let ws = open_workspace_quiet(library, passphrase_stdin)?;
    if let Some(notice) = migrate::unmigrated_notice(&i18n::cli_bundle(), ws.unmigrated_sidecars())
    {
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
///
/// An asset that fails does not stop the run; every failure is reported once the run is done
/// and the command exits non-zero. With `json`, a run that reaches its assets prints exactly one
/// JSON document on stdout — the [`remote::PushSummary`], each asset's outcome included — and
/// nothing else. A run-level failure (no session, no feed, no attestation keys) prints nothing
/// on stdout; its error goes to stderr and the command exits non-zero.
async fn push_workspace(
    workspace: &Workspace,
    options: remote::PushOptions,
    json: bool,
) -> Result<()> {
    let bundle = i18n::cli_bundle();
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
                .map_err(|error| push_run_error(&bundle, &error))?
        };
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return push_failures(&bundle, &summary);
    }

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
        Ok(summary) => {
            if summary.dry_run {
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
            } else if summary.is_no_op() && summary.failed_assets == 0 {
                println!("{}", bundle.format(keys::PUSH_UP_TO_DATE, &[]).cyan());
            } else {
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
            }
            if !summary.dry_run {
                println!("{}", remote::custody_line(&summary).cyan());
            }
            for failed in summary.failures() {
                eprintln!("{}", remote::failure_line(failed).red());
            }
            push_failures(&bundle, &summary)
        }
        Err(error) => Err(push_run_error(&bundle, &error)),
    }
}

/// A push that could not run at all — no session, no feed, no attestation keys — as the error
/// the command exits with.
fn push_run_error(bundle: &capsule_i18n::Bundle, error: &remote::RemoteError) -> eyre::Report {
    if matches!(error, remote::RemoteError::NotAuthenticated) {
        return eyre!("{}", bundle.format(keys::AUTH_NOT_AUTHENTICATED, &[]));
    }
    let reason = describe_remote_error(bundle, error);
    eyre!(
        "{}",
        bundle.format(keys::PUSH_FAILED, &[("reason", Value::Str(&reason))])
    )
}

/// The exit status of a finished push: an error naming how many assets failed when any did,
/// so the process exits non-zero after every asset has been tried and reported.
fn push_failures(bundle: &capsule_i18n::Bundle, summary: &remote::PushSummary) -> Result<()> {
    match remote::failed_run_reason(summary) {
        Some(reason) => Err(eyre!(
            "{}",
            bundle.format(keys::PUSH_FAILED, &[("reason", Value::Str(&reason))])
        )),
        None => Ok(()),
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
    let bundle = i18n::cli_bundle();
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
                return eyre!("{}", bundle.format(keys::AUTH_NOT_AUTHENTICATED, &[]));
            }
            eyre!("restore failed: {}", describe_remote_error(&bundle, &error))
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

/// `capsule share create`: issue a view-only link over `scope` and print its URL.
async fn share_create(
    library: &Path,
    scope: capsule_core::sharing::ShareScope,
    expires_at: Option<String>,
    passphrase_stdin: bool,
    protect: bool,
    link_passphrase_stdin: bool,
) -> Result<()> {
    let bundle = i18n::cli_bundle();
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
                .with_prompt(bundle.format(keys::SHARE_CREATE_LINK_PASSPHRASE_PROMPT, &[]))
                .with_confirmation(
                    bundle.format(keys::SHARE_CREATE_LINK_PASSPHRASE_CONFIRM, &[]),
                    bundle.format(keys::SHARE_CREATE_LINK_PASSPHRASE_MISMATCH, &[]),
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
        bundle
            .format(
                keys::SHARE_CREATE_IN_PROGRESS,
                &[("endpoint", Value::Str(&remote.sync_endpoint))],
            )
            .green()
    );
    match share::create(&remote, &store, &mut ws, &request, jiff::Timestamp::now()).await {
        Ok(issued) => {
            share::render_issued(&bundle, &issued);
            Ok(())
        }
        Err(error) => {
            let reason = share::describe_error(&bundle, &error);
            Err(eyre!(
                "{}",
                bundle.format(
                    keys::SHARE_CREATE_FAILED,
                    &[("reason", Value::Str(&reason))]
                )
            ))
        }
    }
}

/// `capsule share revoke`: stop the link `opaque_id` from opening.
async fn share_revoke(opaque_id: &str) -> Result<()> {
    let bundle = i18n::cli_bundle();
    let remote = remote::RemoteConfig::from_env();
    let store = session_store()?;
    match share::revoke(&remote, &store, opaque_id).await {
        Ok(id) => {
            println!(
                "{}",
                bundle
                    .format(
                        keys::SHARE_REVOKE_SUCCESS,
                        &[("opaque_id", Value::Str(&id))]
                    )
                    .green()
            );
            Ok(())
        }
        Err(error) => {
            let reason = share::describe_error(&bundle, &error);
            Err(eyre!(
                "{}",
                bundle.format(
                    keys::SHARE_REVOKE_FAILED,
                    &[("reason", Value::Str(&reason))]
                )
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
mod library_info_tests {
    use super::library_info_unmigrated;
    use crate::i18n::Bundle;

    fn en() -> Bundle {
        Bundle::for_locale("en")
    }

    /// An up-to-date library still shows the row, with a zero, and no migrate pointer.
    #[test]
    fn a_fully_signed_library_shows_zero_and_no_hint() {
        let (row, hint) = library_info_unmigrated(&en(), 0);
        assert_eq!(row, "Unmigrated sidecars: 0");
        assert_eq!(hint, None);
    }

    /// One unsigned sidecar takes the singular hint, still carrying the count (every arm of
    /// a `#` plural must, or a locale whose `one` covers other numbers drops it); the
    /// pointer names the verb verbatim.
    #[test]
    fn one_unmigrated_sidecar_points_at_the_migrate_verb() {
        let (row, hint) = library_info_unmigrated(&en(), 1);
        assert_eq!(row, "Unmigrated sidecars: 1");
        assert_eq!(
            hint.as_deref(),
            Some("Run `capsule library migrate` to admit the 1 unsigned sidecar.")
        );
    }

    /// Several take the plural hint, carrying the count.
    #[test]
    fn several_unmigrated_sidecars_take_the_plural_hint() {
        let (row, hint) = library_info_unmigrated(&en(), 3);
        assert_eq!(row, "Unmigrated sidecars: 3");
        assert_eq!(
            hint.as_deref(),
            Some("Run `capsule library migrate` to admit the 3 unsigned sidecars.")
        );
    }
}

#[cfg(test)]
mod escrow_tests {
    use capsule_core::lifecycle::LifecycleError;
    use capsule_sdk::recovery::{MintedSecret, RecoveryError};

    use super::{describe_escrow_error, typed_back_matches};
    use crate::i18n::{Bundle, keys};
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

    /// Each refusal the command names renders its own catalog line, and the rebind refusal
    /// tells the user to use a fresh library (#624 decision).
    #[test]
    fn each_named_refusal_has_its_own_line() {
        let bundle = Bundle::for_locale("en");
        let cases = [
            (
                RemoteError::EscrowAlreadyStored,
                keys::AUTH_ESCROW_ALREADY_STORED,
            ),
            (
                RemoteError::RecoverySecretNotConfirmed,
                keys::AUTH_ESCROW_TYPEBACK_MISMATCH,
            ),
            (
                RemoteError::Lifecycle(LifecycleError::AccountRebind("1 asset".into())),
                keys::AUTH_ESCROW_LIBRARY_HAS_ASSETS,
            ),
        ];
        for (error, key) in cases {
            assert_eq!(
                describe_escrow_error(&bundle, &error),
                bundle.format(key, &[])
            );
        }
        assert!(
            bundle
                .format(keys::AUTH_ESCROW_LIBRARY_HAS_ASSETS, &[])
                .contains("fresh library")
        );
    }

    /// Any other failure is wrapped in the generic line, localized by its code when it has one.
    #[test]
    fn other_failures_are_wrapped_in_the_failed_line() {
        let bundle = Bundle::for_locale("en");
        let line =
            describe_escrow_error(&bundle, &RemoteError::Recovery(RecoveryError::NotEnrolled));
        let reason = bundle.format(capsule_i18n::error_codes::ESCROW_NOT_STORED, &[]);
        assert_eq!(
            line,
            bundle.format(
                keys::AUTH_ESCROW_FAILED,
                &[("reason", crate::i18n::Value::Str(&reason))]
            )
        );
    }
}

#[cfg(test)]
mod oidc_login_tests {
    use capsule_sdk::auth::AuthError;
    use clap::Parser as _;

    use super::describe_oidc_error;
    use crate::cli::{AuthCommands, Cli, Commands};
    use crate::i18n::{Bundle, Value, keys};
    use crate::oidc::OidcLoginError;
    use crate::remote::RemoteError;

    /// `--oidc` is its own sign-in: it takes no password inputs, and `--no-browser` means
    /// nothing without it.
    #[test]
    fn the_oidc_flag_excludes_the_password_inputs() {
        let parsed = Cli::try_parse_from(["capsule", "auth", "login", "--oidc", "--no-browser"])
            .expect("--oidc --no-browser parses");
        assert!(matches!(
            parsed.command,
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
            parsed.command,
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
        let bundle = Bundle::for_locale("en");
        assert_eq!(
            describe_oidc_error(
                &bundle,
                &OidcLoginError::DeviceTimedOut {
                    waited: std::time::Duration::from_secs(600)
                }
            ),
            bundle.format(keys::AUTH_LOGIN_OIDC_DEVICE_TIMED_OUT, &[])
        );
        for (error, code) in [
            (
                AuthError::OidcDeviceDenied,
                capsule_i18n::error_codes::AUTH_OIDC_DEVICE_DENIED,
            ),
            (
                AuthError::OidcDeviceExpired,
                capsule_i18n::error_codes::AUTH_OIDC_DEVICE_EXPIRED,
            ),
            (
                AuthError::OidcDeviceUnsupported,
                capsule_i18n::error_codes::AUTH_OIDC_DEVICE_UNSUPPORTED,
            ),
        ] {
            assert_eq!(
                describe_oidc_error(&bundle, &OidcLoginError::Remote(RemoteError::Auth(error))),
                bundle.format(code, &[])
            );
        }
    }

    /// A refused loopback redirect names the server variable the operator has to set.
    #[test]
    fn a_refused_loopback_redirect_names_the_server_setting() {
        let bundle = Bundle::for_locale("en");
        let line = describe_oidc_error(
            &bundle,
            &OidcLoginError::Remote(RemoteError::Auth(AuthError::OidcRedirectInvalid)),
        );
        assert_eq!(
            line,
            bundle.format(keys::AUTH_LOGIN_OIDC_LOOPBACK_REFUSED, &[])
        );
        assert!(line.contains("OIDC_ALLOW_LOOPBACK_REDIRECT=true"), "{line}");
    }

    /// Each terminal failure renders its own line, and provider text cannot carry control
    /// characters into the terminal.
    #[test]
    fn each_oidc_failure_has_its_own_line() {
        let bundle = Bundle::for_locale("en");
        assert_eq!(
            describe_oidc_error(
                &bundle,
                &OidcLoginError::TimedOut {
                    waited: std::time::Duration::from_secs(300)
                }
            ),
            bundle.format(keys::AUTH_LOGIN_OIDC_TIMED_OUT, &[])
        );
        assert_eq!(
            describe_oidc_error(
                &bundle,
                &OidcLoginError::Denied {
                    error: "access_denied".to_owned(),
                    description: None,
                }
            ),
            bundle.format(
                keys::AUTH_LOGIN_OIDC_DENIED,
                &[("error", Value::Str("access_denied"))]
            )
        );
        let described = describe_oidc_error(
            &bundle,
            &OidcLoginError::Denied {
                error: "access_\u{1b}denied".to_owned(),
                description: Some("no\u{1b}[31m thanks".to_owned()),
            },
        );
        assert_eq!(
            described,
            bundle.format(
                keys::AUTH_LOGIN_OIDC_DENIED_DESCRIBED,
                &[
                    ("error", Value::Str("access_denied")),
                    ("description", Value::Str("no[31m thanks")),
                ]
            )
        );
        assert!(
            describe_oidc_error(
                &bundle,
                &OidcLoginError::Remote(RemoteError::Auth(AuthError::OidcNotConfigured))
            )
            .contains(&bundle.format(capsule_i18n::error_codes::AUTH_OIDC_NOT_CONFIGURED, &[]))
        );
    }

    /// The wait line pluralizes on whole minutes.
    #[test]
    fn the_wait_line_counts_minutes() {
        let bundle = Bundle::for_locale("en");
        assert!(
            bundle
                .format(keys::AUTH_LOGIN_OIDC_WAITING, &[("count", Value::Int(1))])
                .contains("1 minute ")
        );
        assert!(
            bundle
                .format(keys::AUTH_LOGIN_OIDC_WAITING, &[("count", Value::Int(5))])
                .contains("5 minutes ")
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
