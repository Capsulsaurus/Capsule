use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub(crate) enum Commands {
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
    /// Run the offline end-to-end data-plane showcase (real cryptography, no network)
    Demo {
        /// Working directory for the demo libraries (a temp dir is used if omitted)
        #[arg(long, value_name = "PATH")]
        workdir: Option<PathBuf>,
        /// A real image/file to import (a small synthetic file is used if omitted)
        #[arg(long, value_name = "PATH")]
        image: Option<PathBuf>,
    },
    /// Sync local and remote data
    Sync {
        /// Discard the saved cursor and re-drain the feed from the start. The per-album
        /// anti-rewind floor still applies, so this cannot resurrect stale entries.
        #[arg(long)]
        force: bool,
        /// Perform a dry run without making changes
        #[arg(long)]
        dry_run: bool,
    },
    /// Show current status
    Status,
    /// List the assets the sync feed has delivered
    List {
        /// Include assets the server has tombstoned (deleted) as well as live ones.
        #[arg(long)]
        include_deleted: bool,
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
pub(crate) enum LibraryCommands {
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
pub(crate) enum AuthCommands {
    /// Create a Capsule account and sign in
    Register {
        /// Account email (prompted when omitted)
        #[arg(long)]
        email: Option<String>,
        /// Login handle (prompted when omitted)
        #[arg(long)]
        username: Option<String>,
        /// Display name (defaults to the username)
        #[arg(long)]
        name: Option<String>,
        /// Read the password from stdin instead of prompting, so the command
        /// works in scripts and CI where there is no terminal.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Login to Capsule
    Login {
        /// Account email (prompted when omitted)
        #[arg(long)]
        email: Option<String>,
        /// Read the password from stdin instead of prompting, so the command
        /// works in scripts and CI where there is no terminal.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Logout from Capsule
    Logout,
    /// Show authentication status
    Status,
}
