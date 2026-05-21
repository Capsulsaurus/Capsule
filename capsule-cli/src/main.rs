use std::path::Path;

use capitalize::Capitalize;
use clap::Parser;
use cli::{AuthCommands, Cli, Commands, LibraryCommands};
use colored::*;
use dialoguer::Confirm;
use eyre::{Result, eyre};
use capsule_core::domain::ImportMode;
use capsule_core::import::scanner::scan as scan_files;
use capsule_core::import::{
    CancellationToken, ImportConfig, ImportOutcome, ImportProgressEvent, execute, plan,
};
use capsule_core::library::{Library, LibraryError, init_library, open_library, rebuild_index};
use capsule_core::metadata::FileMetadata;
use tracing::trace;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

use crate::utils::directories::{get_cache_dir, get_config_dir, get_data_dir};

mod cli;
mod config;
mod db;
mod import;
mod status;
mod utils;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| {
            if cfg!(debug_assertions) {
                EnvFilter::try_new("debug")
            } else {
                EnvFilter::try_new("info")
            }
        })
        .unwrap();
    let fmt_layer = fmt::layer()
        .pretty()
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_file(true)
        .with_line_number(true);
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .init();

    let cli = Cli::parse();
    trace!("Parsed CLI arguments: {:#?}", cli);

    match cli.command {
        // ── Auth ──────────────────────────────────────────────────────────
        Commands::Auth { command } => match command {
            AuthCommands::Login => {
                println!("{}", "Logging in to Capsule...".green());
                todo!("Implement login logic");
            }
            AuthCommands::Logout => {
                println!("{}", "Logging out from Capsule...".yellow());
                todo!("Implement logout logic");
            }
            AuthCommands::Status => {
                println!("{}", "Checking authentication status...".blue());
                match config::Config::from_default_path() {
                    Ok(config) => match status::AuthStatus::check(&config).await {
                        Ok(auth_status) => auth_status.display(),
                        Err(e) => println!("{}", format!("Error checking auth status: {e}").red()),
                    },
                    Err(e) => println!("{}", format!("Error loading config: {e}").red()),
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
                        .map(|t| t.to_string())
                        .unwrap_or_else(|| "never".to_string())
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
            path,
            library,
            r#move,
            force,
        } => {
            println!(
                "{}",
                format!(
                    "Importing {} into library {}...",
                    path.to_string_lossy().blue(),
                    library.to_string_lossy().blue()
                )
                .green()
            );

            let lib = open_library_or_err(&library)?;

            // Phase 1: Scan
            println!("{}", "Scanning source files...".cyan());
            let scan_result = scan_files(&[path]).map_err(|e| eyre!("Scan failed: {e}"))?;

            println!(
                "{}",
                format!(
                    "Found {} candidates ({} files total)",
                    scan_result.candidates.len(),
                    scan_result.total_files()
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
                target_album_id: None,
            };

            let plan_result =
                plan(&scan_result, &lib.db, &config).map_err(|e| eyre!("Planning failed: {e}"))?;

            println!(
                "{}",
                format!(
                    "Plan: {} to import, {} duplicates skipped, {} unsupported/errors",
                    plan_result.counts.to_import,
                    plan_result.counts.duplicates,
                    plan_result.counts.unsupported + plan_result.counts.errors,
                )
                .cyan()
            );

            if plan_result.counts.to_import == 0 {
                println!("{}", "Nothing to import.".yellow());
                lib.close()
                    .map_err(|e| eyre!("Failed to close library: {e}"))?;
                return Ok(());
            }

            // Phase 3: Execute
            println!("{}", "Importing...".cyan());
            let token = CancellationToken::new();

            let summary = execute(
                &plan_result,
                &lib,
                &config,
                |event| {
                    if let ImportProgressEvent::CandidateCompleted { outcomes, .. } = event {
                        for (path, outcome) in &outcomes {
                            let msg = format!("  {}", path.display());
                            match outcome {
                                ImportOutcome::Imported => {
                                    println!("{}", format!("✓ {msg}").green());
                                }
                                ImportOutcome::DuplicateSkipped { .. } => {
                                    println!("{}", format!("= {msg} (duplicate)").yellow());
                                }
                                ImportOutcome::CorruptTransfer => {
                                    println!("{}", format!("✗ {msg} (corrupt transfer)").red());
                                }
                                ImportOutcome::CorruptUnreadable(e) => {
                                    println!("{}", format!("✗ {msg} (unreadable: {e})").red());
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
            .map_err(|e| eyre!("Import execution failed: {e}"))?;

            println!(
                "{}",
                format!(
                    "Done: {} imported, {} duplicates, {} errors",
                    summary.imported_count(),
                    summary.duplicate_count(),
                    summary.error_count()
                )
                .green()
            );

            lib.close()
                .map_err(|e| eyre!("Failed to close library: {e}"))?;
        }

        // ── Sync ──────────────────────────────────────────────────────────
        Commands::Sync { force, dry_run } => {
            println!("{}", "Syncing local and remote data...".green());
            if force {
                println!("{}", "Force sync enabled".yellow());
            }
            if dry_run {
                println!("{}", "Dry run mode enabled".yellow());
            }
            todo!("Implement sync logic");
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
        Commands::List { local, remote } => {
            println!("{}", "Listing files and albums...".green());
            if local {
                println!("{}", "Showing local files only".cyan());
            }
            if remote {
                println!("{}", "Showing remote files only".cyan());
            }
            todo!("Implement list logic");
        }

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
                            Ok(_) => println!(
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
