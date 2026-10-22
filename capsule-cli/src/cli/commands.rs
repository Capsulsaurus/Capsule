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
        /// Move files: delete each source once the server holds it, which needs `--push`.
        /// Without it every source is kept.
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
        /// Print the run as one JSON document on stdout: the totals and each asset's outcome
        /// (verified, failed with its error, or planned on a dry run)
        #[arg(long)]
        json: bool,
    },
    /// Download originals from the server and verify them byte for byte
    Restore {
        /// Path to the Capsule library whose album keys open the originals
        #[arg(long, value_name = "PATH")]
        library: PathBuf,
        /// Directory the originals are written to, one file per asset named by its id; an
        /// interrupted run resumes from what it left there
        #[arg(long, value_name = "DIR")]
        out: PathBuf,
        /// Restore only the assets of this album
        #[arg(long, value_name = "ALBUM_ID", conflicts_with = "asset")]
        album: Option<Uuid>,
        /// Restore only this asset
        #[arg(long, value_name = "ASSET_ID")]
        asset: Option<Uuid>,
        /// Read the library passphrase from stdin instead of prompting, so restores
        /// work in scripts and CI where there is no terminal.
        #[arg(long)]
        passphrase_stdin: bool,
        /// Print the run as one JSON document on stdout: the totals and each asset's outcome
        /// (restored, already present, or failed with its error)
        #[arg(long)]
        json: bool,
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
    /// Show what an imported asset's signed sidecar records: caption, rating, tags, GPS, timestamps
    Show {
        /// The asset to show: its id, or a prefix of at least 8 hex characters of its SHA-256
        /// content hash — the same digest `shasum -a 256` prints for the source file
        #[arg(value_name = "ASSET")]
        asset: String,
        /// Path to the Capsule library
        #[arg(long, value_name = "PATH")]
        library: PathBuf,
        /// Read the library passphrase from stdin instead of prompting, so the command
        /// works in scripts and CI where there is no terminal.
        #[arg(long)]
        passphrase_stdin: bool,
    },
    /// Manage the local library
    Library {
        #[command(subcommand)]
        command: LibraryCommands,
    },
    /// Repair a local library in place, one signed correction per affected asset
    Repair {
        #[command(subcommand)]
        command: RepairCommands,
    },
    /// Issue and revoke view-only share links on the server
    Share {
        #[command(subcommand)]
        command: ShareCommands,
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
    /// Migrate unsigned sidecars from an older library into signed assets
    Migrate {
        /// Path to the library
        path: PathBuf,
        /// Read the library passphrase from stdin instead of prompting, so the command
        /// works in scripts and CI where there is no terminal.
        #[arg(long)]
        passphrase_stdin: bool,
        /// Retention window, in days, for an asset the old library had already deleted:
        /// it is moved to trash with this much time left before it is purged
        #[arg(
            long,
            value_name = "DAYS",
            default_value_t = crate::migrate::DEFAULT_RETAIN_DAYS,
            value_parser = clap::value_parser!(i64).range(0..)
        )]
        retain_days: i64,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum RepairCommands {
    /// Re-read each original's EXIF capture time and report every asset whose signed capture
    /// timestamp disagrees with it; with --apply, correct each one as a signed metadata update
    CaptureTime {
        /// Path to the Capsule library
        #[arg(long, value_name = "PATH")]
        library: PathBuf,
        /// Read the library passphrase from stdin instead of prompting, so the command
        /// works in scripts and CI where there is no terminal.
        #[arg(long)]
        passphrase_stdin: bool,
        /// Write the corrections. Without this flag the pass only reports what it would
        /// change; each correction is an irreversible signed record on the asset's chain.
        #[arg(long)]
        apply: bool,
        /// Correct at most this many affected assets in one --apply run (the report still
        /// covers the whole library); only meaningful with --apply
        #[arg(long, value_name = "COUNT", value_parser = clap::value_parser!(u64).range(1..))]
        limit: Option<u64>,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum ShareCommands {
    /// Issue a view-only share link for an album or one asset, and print the URL to send
    Create {
        /// Path to the Capsule library
        #[arg(long, value_name = "PATH")]
        library: PathBuf,
        /// Share every asset of this album that is not in trash
        #[arg(
            long,
            value_name = "ALBUM_ID",
            required_unless_present = "asset",
            conflicts_with = "asset"
        )]
        album: Option<Uuid>,
        /// Share this one asset
        #[arg(long, value_name = "ASSET_ID")]
        asset: Option<Uuid>,
        /// When the link stops opening, as an RFC 3339 timestamp (e.g. 2026-12-31T23:59:59Z)
        #[arg(long, value_name = "TIMESTAMP")]
        expires_at: Option<String>,
        /// Read the library passphrase from stdin instead of prompting, so the command
        /// works in scripts and CI where there is no terminal.
        #[arg(long)]
        passphrase_stdin: bool,
        /// Protect the link with a passphrase the recipient must enter as well as open the URL
        #[arg(long)]
        protect: bool,
        /// With --protect, read the link passphrase from stdin instead of prompting (the line
        /// after the library passphrase when --passphrase-stdin is also given)
        #[arg(long, requires = "protect")]
        link_passphrase_stdin: bool,
    },
    /// Revoke a share link, so its URL stops opening
    Revoke {
        /// The link's opaque id: the 32 hex characters after /s/ in its URL
        #[arg(value_name = "OPAQUE_ID")]
        opaque_id: String,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum AuthCommands {
    /// Create a Capsule account and sign in
    Register {
        /// Account email (prompted when omitted)
        #[arg(long)]
        email: Option<String>,
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
        /// Sign in through the server's identity provider in a browser instead of with a
        /// password. The server must allow loopback redirects (OIDC_ALLOW_LOOPBACK_REDIRECT=true).
        #[arg(long, conflicts_with_all = ["email", "password_stdin"])]
        oidc: bool,
        /// With --oidc, print the sign-in address without opening a browser
        #[arg(long, requires = "oidc")]
        no_browser: bool,
        /// With --oidc, sign in from another device: print an address and a code instead of
        /// using a browser here
        #[arg(
            long,
            requires = "oidc",
            conflicts_with_all = ["no_browser", "email", "password_stdin"]
        )]
        device: bool,
    },
    /// Set up recovery for a library: bind it to the signed-in account and store its recovery
    /// backup under a new recovery secret
    Escrow {
        /// Path to the Capsule library; it must not hold any assets yet
        #[arg(long, value_name = "PATH")]
        library: PathBuf,
        /// Read the library passphrase from stdin instead of prompting
        #[arg(long)]
        passphrase_stdin: bool,
        /// Replace the account's existing recovery backup; the previous recovery secret stops
        /// working
        #[arg(long)]
        replace: bool,
    },
    /// Recover the signed-in account onto this machine from its recovery backup
    Recover {
        /// Directory for the recovered library; it must not hold a library yet
        #[arg(long, value_name = "PATH")]
        library: PathBuf,
        /// Read the recovery secret from the first line of stdin instead of prompting
        #[arg(long)]
        recovery_secret_stdin: bool,
        /// Read the new library passphrase from the next line of stdin instead of prompting
        #[arg(long)]
        passphrase_stdin: bool,
    },
    /// Logout from Capsule
    Logout,
    /// Show authentication status
    Status,
}
