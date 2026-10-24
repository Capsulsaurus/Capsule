use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
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
/// committed source adapter are spellable: `capsule_library::import::ImportProvider` also names
/// iCloud, Immich, and tethered-camera imports, but their adapters are post-v1 and offering the
/// flag before the adapter exists would be a promise the CLI cannot keep.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportProviderArg {
    /// A Google Photos export produced by Google Takeout, extracted to disk.
    Takeout,
}

impl ImportProviderArg {
    /// The provider's name as shown to the user, substituted into the line `capsule import`
    /// prints when it reads a source as that provider's export.
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
        /// Print the run as one JSON document on stdout: the totals, including how many
        /// originals arrived without derivatives and why, and each file's outcome. Not combined
        /// with `--push`; run `capsule push --json` for the push report
        #[arg(long, conflicts_with = "push")]
        json: bool,
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
        /// Push only this asset (repeatable); every asset in the library when omitted. Retries
        /// the assets a run reported failed without re-importing them or re-checking the ones
        /// that reached custody
        #[arg(long, value_name = "ASSET_ID")]
        asset: Vec<Uuid>,
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
    /// Ask the server whether it durably holds this library's originals, and keep its signed
    /// answer as evidence
    Verify {
        /// Path to the Capsule library
        #[arg(long, value_name = "PATH")]
        library: PathBuf,
        /// Verify only this asset (repeatable); every asset in the library when omitted
        #[arg(long, value_name = "ASSET_ID")]
        asset: Vec<Uuid>,
        /// Ask for the server's signed verdict, check it under the server's attestation key,
        /// and keep it in the library beside each asset's custody receipts
        #[arg(long)]
        signed: bool,
        /// Have the server re-hash the stored bytes rather than only check they are present;
        /// spends the account's deep-verify budget
        #[arg(long)]
        deep: bool,
        /// Read the library passphrase from stdin instead of prompting, so the command
        /// works in scripts and CI where there is no terminal.
        #[arg(long)]
        passphrase_stdin: bool,
        /// Print the run as one JSON document on stdout: the totals and each asset's verdict
        /// (durable, not durable, or failed with its error)
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
    /// Keep a library's cache within the budget the current connection class allows
    Cache {
        #[command(subcommand)]
        command: CacheCommands,
    },
    /// Repair a local library in place, one signed correction per affected asset
    Repair {
        #[command(subcommand)]
        command: RepairCommands,
    },
    /// Add or remove a label on an asset, a region of one, a stack or an album
    Label {
        #[command(subcommand)]
        command: LabelCommands,
    },
    /// Mark or delete a circle or rectangle region on an asset
    Region {
        #[command(subcommand)]
        command: RegionCommands,
    },
    /// Correct an asset's capture time or location; the original's value is always kept
    Edit {
        #[command(subcommand)]
        command: EditCommands,
    },
    /// Issue and revoke view-only share links on the server
    Share {
        #[command(subcommand)]
        command: ShareCommands,
    },
    /// Hand uploads to another device of this account, or upload ones handed to this device
    Peer {
        #[command(subcommand)]
        command: PeerCommands,
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
    #[command(args_conflicts_with_subcommands = true)]
    Sync {
        #[command(subcommand)]
        command: Option<SyncCommands>,
        /// Discard the saved cursor and re-drain the feed from the start. The per-album
        /// anti-rewind floor still applies, so this cannot resurrect stale entries.
        #[arg(long)]
        force: bool,
        /// Perform a dry run without making changes
        #[arg(long)]
        dry_run: bool,
        /// The library to re-announce from when the server has rebuilt this library's index:
        /// with it, a sync that meets a stale cursor runs `capsule sync reannounce` itself
        #[arg(long, value_name = "PATH")]
        library: Option<PathBuf>,
        /// Read the library passphrase from stdin instead of prompting (with `--library`)
        #[arg(long, requires = "library")]
        passphrase_stdin: bool,
        /// Print the run as one JSON document on stdout: how many changes were applied across
        /// how many albums and pages, whether the server had rebuilt the index, and the
        /// re-announce report when `--library` ran one
        #[arg(long)]
        json: bool,
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
pub(crate) enum SyncCommands {
    /// Give the server back the signed manifests this library holds and its feed no longer
    /// lists, after it rebuilt this library's index. Manifests only: no bytes are uploaded
    Reannounce {
        /// Path to the Capsule library whose manifests are re-announced
        #[arg(long, value_name = "PATH")]
        library: PathBuf,
        /// Read the library passphrase from stdin instead of prompting, so the command
        /// works in scripts and CI where there is no terminal.
        #[arg(long)]
        passphrase_stdin: bool,
        /// Report what would be re-announced without sending anything
        #[arg(long)]
        dry_run: bool,
        /// Print the run as one JSON document on stdout: the totals and each asset's answer
        /// (restored, already indexed, missing blobs, refused, or planned on a dry run)
        #[arg(long)]
        json: bool,
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
pub(crate) enum CacheCommands {
    /// Evict cached thumbnails, previews and fetched originals, least recently viewed first,
    /// until the cache fits the budget the current connection class allows. Pinned
    /// representations, device-owned originals and the library's own media are never evicted.
    Sweep {
        /// Path to the Capsule library
        #[arg(long, value_name = "PATH")]
        library: PathBuf,
        /// Print the run as one JSON document on stdout: the plan it ran under and each
        /// evicted representation
        #[arg(long)]
        json: bool,
    },
    /// Show the cache budget for the current connection class, or change the base budget
    /// every class scales from (2 GiB unless set)
    Budget {
        /// Store this many bytes as the base budget
        #[arg(long, value_name = "BYTES", conflicts_with = "reset")]
        set: Option<u64>,
        /// Clear the stored base budget, returning to the 2 GiB default
        #[arg(long)]
        reset: bool,
        /// Print the plan as one JSON document on stdout
        #[arg(long)]
        json: bool,
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

/// The library an edit opens, and how its outcome is printed: shared by every `label`,
/// `region` and `edit` subcommand.
#[derive(Args, Debug)]
pub(crate) struct EditLibraryArgs {
    /// Path to the Capsule library
    #[arg(long, value_name = "PATH")]
    pub(crate) library: PathBuf,
    /// Read the library passphrase from stdin instead of prompting, so the command
    /// works in scripts and CI where there is no terminal.
    #[arg(long)]
    pub(crate) passphrase_stdin: bool,
    /// Print what the edit did as one JSON document on stdout: the target and its labels,
    /// capture time or location afterwards
    #[arg(long)]
    pub(crate) json: bool,
}

/// What a label edit applies to: exactly one of an asset (or one of its regions), a stack or
/// an album.
#[derive(Args, Debug)]
#[group(required = true, multiple = false, id = "label_target")]
pub(crate) struct LabelTargetArgs {
    /// Label this asset: its id, or a prefix of at least 8 hex characters of its SHA-256
    /// content hash
    #[arg(long, value_name = "ASSET")]
    pub(crate) asset: Option<String>,
    /// Label this stack
    #[arg(long, value_name = "STACK_ID")]
    pub(crate) stack: Option<Uuid>,
    /// Label this album
    #[arg(long, value_name = "ALBUM_ID")]
    pub(crate) album: Option<Uuid>,
}

/// One label edit's arguments.
#[derive(Args, Debug)]
pub(crate) struct LabelArgs {
    /// The label: 1 to 256 bytes once trimmed
    #[arg(value_name = "LABEL")]
    pub(crate) label: String,
    #[command(flatten)]
    pub(crate) target: LabelTargetArgs,
    /// Label this region of the asset instead of the asset itself
    #[arg(long, value_name = "REGION_ID", requires = "asset")]
    pub(crate) region: Option<Uuid>,
    #[command(flatten)]
    pub(crate) library: EditLibraryArgs,
}

#[derive(Subcommand, Debug)]
pub(crate) enum LabelCommands {
    /// Add a label, as a signed edit
    Add(LabelArgs),
    /// Remove a label: every add of it this device has seen
    Remove(LabelArgs),
}

#[derive(Subcommand, Debug)]
pub(crate) enum RegionCommands {
    /// Mark a circle or rectangle region on an asset, in fractions of the image (display
    /// orientation), and print its id
    Add {
        /// The asset: its id, or a prefix of at least 8 hex characters of its content hash
        #[arg(long, value_name = "ASSET")]
        asset: String,
        /// A circle: centre, a fraction of width and height, and radius, a fraction of the
        /// shorter side (min(width, height))
        #[arg(
            long,
            value_name = "CX,CY,R",
            value_parser = crate::edit::parse_circle,
            conflicts_with = "rect",
            required_unless_present = "rect"
        )]
        circle: Option<capsule_core::sidecar::RegionShape>,
        /// A rectangle: top-left corner, width and height, each a fraction of the image
        #[arg(long, value_name = "X,Y,W,H", value_parser = crate::edit::parse_rect)]
        rect: Option<capsule_core::sidecar::RegionShape>,
        /// Give the region this label (repeatable)
        #[arg(long, value_name = "LABEL")]
        label: Vec<String>,
        #[command(flatten)]
        library: EditLibraryArgs,
    },
    /// Delete a region of an asset. Final: another device's edit of it does not bring it back
    Delete {
        /// The region
        #[arg(value_name = "REGION_ID")]
        region: Uuid,
        /// The asset: its id, or a prefix of at least 8 hex characters of its content hash
        #[arg(long, value_name = "ASSET")]
        asset: String,
        #[command(flatten)]
        library: EditLibraryArgs,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum EditCommands {
    /// Correct an asset's capture time; the original's capture time is kept beside it
    CaptureTime {
        /// The corrected instant, RFC 3339 with an offset (2021-07-04T18:30:00Z)
        #[arg(value_name = "TIME")]
        time: jiff::Timestamp,
        /// The asset: its id, or a prefix of at least 8 hex characters of its content hash
        #[arg(long, value_name = "ASSET")]
        asset: String,
        #[command(flatten)]
        library: EditLibraryArgs,
    },
    /// Correct an asset's location, or record that it has none; the original's location is
    /// kept beside it
    Location {
        /// The asset: its id, or a prefix of at least 8 hex characters of its content hash
        #[arg(long, value_name = "ASSET")]
        asset: String,
        /// Latitude, in degrees
        #[arg(
            long,
            value_name = "DEGREES",
            value_parser = crate::edit::parse_coordinate,
            allow_negative_numbers = true,
            requires = "lon",
            required_unless_present = "clear"
        )]
        lat: Option<f64>,
        /// Longitude, in degrees
        #[arg(
            long,
            value_name = "DEGREES",
            value_parser = crate::edit::parse_coordinate,
            allow_negative_numbers = true,
            requires = "lat"
        )]
        lon: Option<f64>,
        /// The datum the coordinates are in (stored verbatim, never converted): WGS-84 or
        /// GCJ-02
        #[arg(
            long,
            value_name = "DATUM",
            value_parser = crate::edit::parse_datum,
            default_value = "WGS-84"
        )]
        datum: capsule_core::domain::GpsDatum,
        /// Record that the asset has no location
        #[arg(long, conflicts_with_all = ["lat", "lon"])]
        clear: bool,
        #[command(flatten)]
        library: EditLibraryArgs,
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
        /// Print the issued link as one JSON document on stdout: its opaque id, URL, how many
        /// assets it covers, whether it is passphrase-protected, and its expiry
        #[arg(long)]
        json: bool,
    },
    /// Revoke a share link, so its URL stops opening
    Revoke {
        /// The link's opaque id: the 32 hex characters after /s/ in its URL
        #[arg(value_name = "OPAQUE_ID")]
        opaque_id: String,
        /// Print the outcome as one JSON document on stdout: the revoked link's opaque id
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum PeerCommands {
    /// Seal assets' uploads for another device of this account to upload later. Each asset is
    /// written to a hand-off file the other device takes with `capsule peer accept`, or with
    /// `--peer` handed over the LAN peering channel to `capsule peer accept --listen`; this
    /// device keeps every original until its own check finds the server holds it
    Delegate {
        /// Path to the Capsule library holding the assets
        #[arg(long, value_name = "PATH")]
        library: PathBuf,
        /// The device to hand the uploads to: its id in this account's device directory.
        /// Required with `--out`; with `--peer`, the device the channel verifies must be this one
        #[arg(long, value_name = "DEVICE_ID", required_unless_present = "peer")]
        to: Option<Uuid>,
        /// An asset to hand off (repeatable)
        #[arg(long, value_name = "ASSET_ID", required = true)]
        asset: Vec<Uuid>,
        /// Directory the hand-off files are written to
        #[arg(
            long,
            value_name = "DIR",
            required_unless_present = "peer",
            conflicts_with = "peer"
        )]
        out: Option<PathBuf>,
        /// Hand the uploads over the LAN peering channel instead of files: dial the other
        /// device's `capsule peer accept --listen` at HOST:PORT, verify it over mutual TLS as a
        /// device of this account, and serve it each hand-off to pull and acknowledge over the
        /// same channel. Only a local-network address is dialed
        #[arg(long, value_name = "HOST:PORT")]
        peer: Option<String>,
        /// Hours to wait for the server to hold the assets before taking the uploads back
        #[arg(
            long,
            value_name = "HOURS",
            default_value_t = 24,
            value_parser = clap::value_parser!(u64).range(1..)
        )]
        deadline_hours: u64,
        /// Read the library passphrase from stdin instead of prompting, so the command
        /// works in scripts and CI where there is no terminal.
        #[arg(long)]
        passphrase_stdin: bool,
        /// Print the run as one JSON document on stdout: the delegate, each asset handed off,
        /// with its file or whether the delegate acknowledged it over the channel, and its
        /// content address, and each that was not, with a stable `kind`, the server's `code`
        /// when it gave one, and the detail
        #[arg(long)]
        json: bool,
    },
    /// Take hand-off files another device of this account sealed for this one, then upload
    /// every hand-off this device holds under its own session, deleting each copy once the
    /// server holds it. A hand-off that can never succeed is quarantined: kept, not retried,
    /// listed by `capsule peer status` and deleted only by `capsule peer discard`
    Accept {
        /// Path to this device's Capsule library
        #[arg(long, value_name = "PATH")]
        library: PathBuf,
        /// A hand-off file to take (repeatable); without one, only the held hand-offs upload
        #[arg(long, value_name = "FILE")]
        from: Vec<PathBuf>,
        /// Also take hand-offs over the LAN peering channel: listen on ADDR (e.g.
        /// `0.0.0.0:7420`) for the other device's `capsule peer delegate --peer`, verify it over
        /// mutual TLS as a device of this account, pull each hand-off it sealed for this device,
        /// and acknowledge each over the same channel once it is persisted. Only local-network
        /// peers are admitted. The address bound is printed on stderr
        #[arg(long, value_name = "ADDR")]
        listen: Option<SocketAddr>,
        /// Seconds `--listen` waits for a peer to connect and verify; a verified session is not
        /// cut by it, but runs to its end at a minimum transfer rate
        #[arg(
            long,
            value_name = "SECONDS",
            default_value_t = 300,
            value_parser = clap::value_parser!(u64).range(1..),
            requires = "listen"
        )]
        listen_timeout: u64,
        /// Open the tier sessions in ladder order, gating the above-index tiers on the
        /// connection class, as `capsule push --staged` does
        #[arg(long)]
        staged: bool,
        /// Read the library passphrase from stdin instead of prompting, so the command
        /// works in scripts and CI where there is no terminal.
        #[arg(long)]
        passphrase_stdin: bool,
        /// Print the run as one JSON document on stdout: each hand-off taken with its
        /// acknowledgement (and, over the channel, the peer it came from and whether the peer
        /// took the acknowledgement), each refused, and what became of each held hand-off
        /// (removed, kept, failed and kept, or permanently refused and quarantined), each
        /// failure with a stable `kind`, the server's `code` when it gave one, and the detail
        #[arg(long)]
        json: bool,
    },
    /// Show this device's handed-off assets: take the other device's acknowledgements, take back
    /// uploads that are overdue or whose device was revoked, and release what the server holds.
    /// Also lists every hand-off this device quarantined
    Status {
        /// Path to the Capsule library holding the assets
        #[arg(long, value_name = "PATH")]
        library: PathBuf,
        /// An acknowledgement file `capsule peer accept` wrote (repeatable)
        #[arg(long, value_name = "FILE")]
        ack: Vec<PathBuf>,
        /// Read the library passphrase from stdin instead of prompting, so the command
        /// works in scripts and CI where there is no terminal.
        #[arg(long)]
        passphrase_stdin: bool,
        /// Print the run as one JSON document on stdout: each delegation with its state, each
        /// refused acknowledgement with a stable `kind` and the detail, and each quarantined
        /// hand-off with its file, the stable `kind` of its refusal, the server's `code` when
        /// it gave one, and the detail
        #[arg(long)]
        json: bool,
    },
    /// Delete a quarantined hand-off this device holds for another device. Only a quarantined
    /// copy can be discarded; it may be the account's only copy of the asset, so recover it
    /// first if its origin device is lost
    Discard {
        /// Path to this device's Capsule library
        #[arg(long, value_name = "PATH")]
        library: PathBuf,
        /// The quarantined hand-off's content address, as `capsule peer status` lists it
        #[arg(value_name = "HAND_OFF")]
        hand_off: String,
        /// Read the library passphrase from stdin instead of prompting, so the command
        /// works in scripts and CI where there is no terminal.
        #[arg(long)]
        passphrase_stdin: bool,
        /// Print the discarded hand-off as one JSON document on stdout: its content address,
        /// its asset and the `kind` it had been quarantined for
        #[arg(long)]
        json: bool,
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
        /// Print the outcome of a password sign-in as one JSON document on stdout; prompts and
        /// progress go to stderr. Not combined with --oidc, whose address is printed for a person
        #[arg(long, conflicts_with = "oidc")]
        json: bool,
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
    Logout {
        /// Print the outcome as one JSON document on stdout: whether a session was signed out
        #[arg(long)]
        json: bool,
    },
    /// Show authentication status
    Status {
        /// Print the status as one JSON document on stdout: whether a session is stored, whether
        /// its access token is still fresh, and when that token expires
        #[arg(long)]
        json: bool,
    },
}
