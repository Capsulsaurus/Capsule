use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Authentication commands
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },
    /// Import files into a local Capsule library
    Import {
        /// Source file or directory to import
        path: PathBuf,
        /// Path to the Capsule library
        #[arg(long, value_name = "PATH")]
        library: PathBuf,
        /// Move files instead of copying them
        #[arg(long)]
        r#move: bool,
        /// Re-import files even if they already exist (duplicate override)
        #[arg(long)]
        force: bool,
    },
    /// Manage the local library
    Library {
        #[command(subcommand)]
        command: LibraryCommands,
    },
    /// Sync local and remote data
    Sync {
        /// Force sync even if there are conflicts
        #[arg(long)]
        force: bool,
        /// Perform a dry run without making changes
        #[arg(long)]
        dry_run: bool,
    },
    /// Show current status
    Status,
    /// List files and albums
    List {
        /// Show only local files
        #[arg(long)]
        local: bool,
        /// Show only remote files
        #[arg(long)]
        remote: bool,
    },
    /// Match metadata for current file
    Match {
        /// Path to the file to match metadata for
        path: PathBuf,
    },
    /// Reset all local CLI data
    Reset {
        /// Reset configuration
        #[arg(long)]
        config: bool,
        /// Reset data directory
        #[arg(long)]
        data: bool,
        /// Reset cache directory
        #[arg(long)]
        cache: bool,
        /// Reset all data
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum LibraryCommands {
    /// Create a new Capsule library
    Init {
        /// Directory for the new library
        path: PathBuf,
        /// Human-readable library name
        #[arg(long, default_value = "My Library")]
        name: String,
    },
    /// Show library information
    Info {
        /// Path to the library
        path: PathBuf,
    },
    /// Rebuild the SQLite index from sidecar files
    Rebuild {
        /// Path to the library
        path: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
pub enum AuthCommands {
    /// Login to Capsule
    Login,
    /// Logout from Capsule
    Logout,
    /// Show authentication status
    Status,
}
