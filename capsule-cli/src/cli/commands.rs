use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};
use uuid::Uuid;

/// The trinary culling flag as a command-line value (`--filter pick`). Maps onto
/// `capsule_core`'s `CullFlag`; kept separate so the argument surface owns its own spelling.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CullFlagArg {
    /// A keeper.
    Pick,
    /// Not yet culled either way — the never-flagged default.
    Neutral,
    /// Marked for rejection; the set `--sweep` moves to trash.
    Reject,
}

/// The exporting service an import reads (`--provider takeout`). Only the providers with a
/// committed source adapter are spellable: `capsule_core::import::ImportProvider` also names
/// iCloud, Immich, and tethered-camera imports, but their adapters are post-v1 and offering the
/// flag before the adapter exists would be a promise the CLI cannot keep.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportProviderArg {
    /// A Google Photos export produced by Google Takeout, extracted to disk.
    Takeout,
}

impl ImportProviderArg {
    /// The provider's name as shown to the user. A brand name is displayed verbatim in
    /// every locale, so it is a *value* substituted into the `cli.import.provider_notice`
    /// message rather than translatable prose of its own.
    #[must_use]
    pub(crate) const fn display_name(self) -> &'static str {
        match self {
            Self::Takeout => "Google Takeout",
        }
    }
}

#[derive(Subcommand, Debug)]
pub(crate) enum Commands {
    /// Authentication commands
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },
    /// Import files into a local Capsule library
    Import {
        /// Source file or directory to import. Repeatable: a split Takeout export extracted
        /// into several folders is imported by naming every part in one run, so a media file
        /// and a sidecar that landed in different parts are still paired.
        #[arg(required = true, value_name = "PATH", num_args = 1..)]
        paths: Vec<PathBuf>,
        /// Read the source as an export from this service instead of as a plain directory
        /// tree, so its out-of-band metadata (capture time, GPS, captions, favorites, album
        /// membership) is folded into the imported assets.
        #[arg(long, value_name = "PROVIDER")]
        provider: Option<ImportProviderArg>,
        /// Path to the Capsule library
        #[arg(long, value_name = "PATH")]
        library: PathBuf,
        /// Move files instead of copying them
        #[arg(long)]
        r#move: bool,
        /// Re-import files even if they already exist (duplicate override)
        #[arg(long)]
        force: bool,
        /// Read the library passphrase from stdin instead of prompting, so imports
        /// work in scripts and CI where there is no terminal.
        #[arg(long)]
        passphrase_stdin: bool,
        /// Push the library to the server after importing — sugar for a `capsule push`
        /// run over the same library. The import itself stays offline.
        #[arg(long)]
        push: bool,
        /// Stage the follow-on push (`--push`) in tier order, gating the preview and
        /// original tiers on the connection class.
        #[arg(long)]
        staged: bool,
    },
    /// Upload a local Capsule library to the server
    Push {
        /// Path to the Capsule library to push
        #[arg(long, value_name = "PATH")]
        library: PathBuf,
        /// Read the library passphrase from stdin instead of prompting, so pushes
        /// work in scripts and CI where there is no terminal.
        #[arg(long)]
        passphrase_stdin: bool,
        /// Report what would be uploaded without opening a single upload session
        #[arg(long)]
        dry_run: bool,
        /// Re-drive every blob regardless of what the server already holds
        #[arg(long)]
        force: bool,
        /// Open the tier sessions in ladder order (index → preview → original), gating
        /// the above-index tiers on the connection class, instead of opening all eagerly
        #[arg(long)]
        staged: bool,
    },
    /// Review a local library: flag assets, filter by flag, sweep rejects to trash
    Cull {
        /// Path to the Capsule library
        #[arg(long, value_name = "PATH")]
        library: PathBuf,
        /// Read the library passphrase from stdin instead of prompting, so culling
        /// works in scripts and CI where there is no terminal.
        #[arg(long)]
        passphrase_stdin: bool,
        /// Flag an asset as a keeper (repeatable)
        #[arg(long, value_name = "ASSET_ID")]
        pick: Vec<Uuid>,
        /// Clear an asset's flag back to the never-flagged default (repeatable)
        #[arg(long, value_name = "ASSET_ID")]
        neutral: Vec<Uuid>,
        /// Flag an asset for rejection (repeatable)
        #[arg(long, value_name = "ASSET_ID")]
        reject: Vec<Uuid>,
        /// List the assets carrying one flag instead of only counting them
        #[arg(long, value_name = "FLAG")]
        filter: Option<CullFlagArg>,
        /// Move every rejected asset to trash. The only destructive step, and soft per
        /// retention — swept assets stay restorable until the window elapses.
        #[arg(long)]
        sweep: bool,
        /// Retention window, in days, the sweep's soft delete stamps
        #[arg(long, value_name = "DAYS", default_value_t = crate::cull::DEFAULT_RETAIN_DAYS)]
        retain_days: i64,
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
