//! The offline asset lifecycle — the integration layer that ties the cryptographic data
//! plane to the on-disk client library, and the substrate the CLI showcase drives.
//!
//! A [`Workspace`] holds an unlocked [`Account`], the per-album key material + its
//! [`ReferenceAuthority`], and the signed device directory. Each operation produces the
//! design's real artifacts and self-checks them through [`verify_asset`]:
//!
//! - [`import_asset`](Workspace::import_asset) — derive the file key, STREAM-encrypt to get
//!   the content hash, build + sign the create manifest, append the provenance chain, write
//!   the signed [`SidecarV1`], and gate on `verify_asset == Accept`.
//! - [`tag_add`](Workspace::tag_add) / [`set_caption`](Workspace::set_caption) — CRDT edits
//!   emitting a `metadata-update` provenance record.
//! - [`soft_delete`](Workspace::soft_delete) / [`restore`](Workspace::restore) — `delete`
//!   (with a signed retention window) and `trash-restore` lifecycle records.
//! - [`export_backup`](Workspace::export_backup) / [`import_backup`](Workspace::import_backup)
//!   — the portable artifact round-trip; the client stores plaintext, so ciphertext is
//!   regenerated deterministically from the manifest's recorded nonce prefix.
//!
//! Clients store **plaintext** locally (original + signed sidecar + provenance chain);
//! encryption produces the artifacts that cross a boundary. Offline epoch rotation is supported
//! ([`rotate_epoch`](Workspace::rotate_epoch)); the MLS membership ceremony (`Welcome`,
//! add/remove) remains deferred (see `SLICES.md`).
//!
//! [`verify_asset`]: crate::crypto::verify_asset
//! [`ReferenceAuthority`]: crate::crypto::authority::ReferenceAuthority

mod album;
mod backup;
#[cfg(feature = "media")]
mod derivatives;
mod drops;
mod groups;
mod import;
mod metadata;
mod open;
mod organize;
mod provenance;
mod sharing;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use thiserror::Error;
use uuid::Uuid;

use self::drops::{InboxEntry, IssuedLink};
use crate::backup::BackupError;
use crate::crypto::CryptoError;
use crate::crypto::authority::Authority;
use crate::crypto::hash::Hash32;
use crate::crypto::keys::{Account, DeviceDirectory, HybridSigningKey, HybridVerifyingKey, Signer};
use crate::crypto::primitives::Argon2Params;
use crate::crypto::provenance::ProvenanceChain;
use crate::crypto::provenance::action::Action;
use crate::crypto::verify_asset::{MetadataBinding, VerifyOutcome};
use crate::db::DatabaseDriver;
use crate::drop::{DropId, UploadLinkId};
use crate::federation::AlbumGroupAssertion;
use crate::library::Library;
use crate::metadata::crdt::Counter;
use crate::sharing::{ShareLinkId, ShareLinkRecord};
use crate::sidecar::sidecar_v1::SidecarV1;

/// Errors from lifecycle operations.
#[derive(Debug, Error)]
#[cfg_attr(feature = "ffi", derive(uniffi::Error), uniffi(flat_error))]
pub enum LifecycleError {
    /// Filesystem error.
    #[error("io: {0}")]
    Io(String),
    /// Unknown album / asset id.
    #[error("not found: {0}")]
    NotFound(String),
    /// An asset failed its own `verify_asset` self-check (a bug — should never happen).
    #[error("verify_asset self-check failed: {0:?}")]
    SelfVerify(VerifyOutcome),
    /// A sealed metadata blob failed its own metadata↔manifest binding self-check (a bug —
    /// the sidecar the workspace just wrote does not round-trip to the committed hash).
    #[error("metadata binding self-check failed: {0:?}")]
    MetadataUnbound(MetadataBinding),
    /// Cryptographic error.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// Backup error.
    #[error(transparent)]
    Backup(#[from] BackupError),
    /// CBOR (de)serialization error.
    #[error("cbor: {0}")]
    Cbor(String),
    /// Library index (SQLite) error.
    #[error("db: {0}")]
    Db(String),
}

type Result<T> = std::result::Result<T, LifecycleError>;

/// One album's key material across one or more epochs.
pub struct AlbumKeys {
    /// Album id.
    pub album_id: Uuid,
    /// Display name.
    pub name: String,
    /// AMKs by epoch.
    pub amks: BTreeMap<u32, [u8; 32]>,
    /// Per-album write-tier signing key.
    pub write_tier: HybridSigningKey,
    /// Per-album admin signing key.
    pub admin: HybridSigningKey,
    /// The current (highest) epoch — the one new imports are written under.
    pub current_epoch: u32,
}

/// In-memory state for one managed asset.
pub struct AssetState {
    /// Asset id (== file_id).
    pub asset_id: Uuid,
    /// Owning album.
    pub album_id: Uuid,
    /// Original file extension (lowercase).
    pub ext: String,
    /// UTC seconds used for date bucketing on disk.
    pub capture_utc: i64,
    /// The provenance chain.
    pub chain: ProvenanceChain,
    /// The signed sidecar.
    pub sidecar: SidecarV1,
    /// The **exact** sealed metadata-blob wire bytes the current metadata-bearing manifest
    /// commits to via `metadata_blob_hash`. Because [`seal_metadata_blob`] draws a fresh nonce
    /// per call (folded into the blob key), the blob cannot be regenerated deterministically
    /// from the plaintext sidecar (unlike the asset ciphertext, which is re-derived from the
    /// recorded `nonce_prefix`), so the bytes are retained to keep the content address stable
    /// across export. Re-sealed on every
    /// metadata-bearing write ([`Action::binds_metadata_blob`]); untouched by `delete` /
    /// `trash-restore`, which mint no new blob.
    ///
    /// [`seal_metadata_blob`]: crate::crypto::encryption::seal_metadata_blob
    pub metadata_blob: Vec<u8>,
    /// Placement within a multi-file stack (RAW+JPEG, Live Photo, …) when this asset was
    /// imported as a stack member; `None` for a standalone asset. Drives the queryable index
    /// row's `stack_id` / `is_stack_hidden` so a hidden secondary member stays out of the
    /// timeline exactly as the legacy importer arranged.
    pub stack: Option<StackPlacement>,
}

/// Placement of a signed asset within an import stack. The executor mints one `stack_id` per
/// multi-file [`ImportCandidate`](crate::import::scan::ImportCandidate) and marks every
/// non-primary member `hidden`, so the primary alone surfaces in the timeline.
#[derive(Debug, Clone)]
pub struct StackPlacement {
    /// The shared stack id (the `asset_stacks` row id the members belong to).
    pub stack_id: String,
    /// Whether this member is hidden from the timeline (true for every non-primary member).
    pub hidden: bool,
}

/// Options for a signed import driven by the import executor. Defaults (`Copy` mode, no stack)
/// reproduce the standalone [`Workspace::import_asset`] behaviour.
#[derive(Debug, Clone, Default)]
pub struct SignedImportOptions {
    /// Delete the source file after a durable commit (Move mode).
    pub move_source: bool,
    /// Defer Move-mode source deletion to the caller's verify-before-destroy gate
    /// (`S-D4` [`release_move_source`](crate::library::release_move_source)) instead of
    /// releasing on the local durable commit. Set by online/streaming import (`S-B3`), where
    /// the source is the only copy until the *server* durably holds it; left `false` for a
    /// plain offline import, which releases after the self-verified local commit.
    pub defer_source_release: bool,
    /// Stack placement for a multi-file candidate member.
    pub stack: Option<StackPlacement>,
}

/// A streamed import: everything the [streaming window](crate::import::streaming) needs about one
/// just-imported asset to drive its upload → verify → release step, without exposing workspace
/// internals. Produced by [`Workspace::import_asset_streaming`], which commits on the signed path
/// with source release **deferred** to the server-side verify-before-destroy gate (`S-D4`), since
/// in streaming mode the local bytes are the only copy until the *server* durably holds them.
#[derive(Debug, Clone)]
pub struct StreamedImport {
    /// The imported asset's id.
    pub asset_id: Uuid,
    /// The declared blob content-addresses the release gate re-checks: the original ciphertext
    /// and the sealed metadata blob (the always-present required blobs).
    pub blob_hashes: Vec<Hash32>,
    /// The local library original's path — released (its file deleted and owned-original
    /// representation row dropped) only on a `durable` verdict.
    pub local_original: PathBuf,
    /// The external Move-mode source path, if any — deleted only on a `durable` verdict.
    pub move_source: Option<PathBuf>,
}

/// An offline Capsule workspace over a client library directory.
pub struct Workspace {
    root: PathBuf,
    account: Account,
    /// Signs the device directory and every asset manifest with the device DSK. A software key
    /// by default; a [hardware-backed signer](crate::crypto::keys::HardwareBackedSigner) when
    /// the device key lives in a secure element. The account's own software DSK is retained
    /// (sealed) but unused for signing when a hardware signer is supplied.
    device_signer: Box<dyn Signer>,
    directory: DeviceDirectory,
    /// The `client_version` / `generated_by_client` this workspace stamps on every manifest and
    /// derivative it authors (S-D15). Defaults to the bare-core
    /// [`capsule-core/{semver}+{commit}`](crate::client_build::core_client_version) identity; an
    /// app injects its own product id via [`with_client_id`](Self::with_client_id) (CLI) or the
    /// FFI constructor (native apps) so each client reports itself rather than `capsule-core`.
    client_version: String,
    counter: Counter,
    albums: HashMap<Uuid, AlbumKeys>,
    /// Per-album write authority behind the [`AlbumAuthority`](crate::crypto::authority::AlbumAuthority)
    /// seam (`&Authority` coerces to `&dyn AlbumAuthority` at every `verify_asset` call site). The
    /// offline [`ReferenceAuthority`] is the shipped default; the enum lets the live
    /// [`OpenMlsAuthority`](crate::crypto::authority::OpenMlsAuthority) drop in without the
    /// lifecycle naming a concrete backend. Session-scoped (not persisted), like the album keys.
    authorities: HashMap<Uuid, Authority>,
    assets: HashMap<Uuid, AssetState>,
    /// The reconciled [aggregated-album](crate::federation) group assertion for each album the
    /// workspace can see, keyed by album id (`S-E4`). An own album's entry is self-authored via
    /// [`create_album_group`](Self::create_album_group) / [`join_album_group`](Self::join_album_group);
    /// a peer constituent's entry is folded in from the feed via
    /// [`merge_album_group_assertion`](Self::merge_album_group_assertion). Removing an entry is a
    /// group *leave* — it drops the constituent from every viewer's aggregate on their next sync.
    group_assertions: HashMap<Uuid, AlbumGroupAssertion>,
    /// The open, locked library — its `library.sqlite` is the queryable index the crypto
    /// lifecycle writes through to. Held for the workspace's lifetime so the lock is retained.
    library: Library,
    /// Argon2id cost the account was created under; reused for the optional passphrase wrap
    /// on share links so a share's cost matches the device tier.
    argon2_params: Argon2Params,
    /// Issued share links keyed by their revocation handle — the authoritative link records
    /// the serving endpoint (S-C4) consults for scope, expiry, and revocation state.
    share_links: HashMap<ShareLinkId, ShareLinkRecord>,
    /// Issued upload (guest-drop) links keyed by their revocation handle. Each holds the
    /// escrow-wrapped Drop Key private half so any of this owner's adopting devices can
    /// later decapsulate a drop sealed to it (SSoT: [Web Upload]).
    ///
    /// [Web Upload]: https://docs/design/web-upload/
    upload_links: HashMap<UploadLinkId, IssuedLink>,
    /// Pending guest drops in this user's inbox, keyed by drop id. Models the server's
    /// staging store (`capsule-api-media::drops`, S-C5) so the offline core can drive the
    /// full seal → stage → adopt path; a real client fills it from server responses.
    inbox: HashMap<DropId, InboxEntry>,
    /// The per-platform still-derivative byte encoder (the `capsule-sdk` codec seam). When set
    /// (behind the `media` feature), a signed import additionally decodes the still, computes its
    /// LQIP, and generates + signs thumbnail/preview [`DerivativeManifest`]s per S-B1's pipeline.
    /// `None` leaves imports at signed-original-only, so the default library build stays free of
    /// the media stack.
    #[cfg(feature = "media")]
    still_encoder: Option<Box<dyn crate::media::image::derivative::StillEncoder>>,
}

fn now_rfc3339() -> String {
    Timestamp::now().to_string()
}

fn media_dir(root: &Path, capture_utc: i64) -> PathBuf {
    let date = Timestamp::from_second(capture_utc)
        .unwrap_or(Timestamp::UNIX_EPOCH)
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date();
    root.join("media")
        .join(format!("{:04}", date.year()))
        .join(format!("{:04}-{:02}", date.year(), date.month()))
}

/// Whether an asset is currently in trash — derived by replaying its provenance chain's
/// lifecycle actions (a `delete` moves it to trash; a later `trash-restore` brings it back).
/// The chain is the single source of truth, so a swept (reject-swept) asset is exactly one
/// whose head lifecycle state is deleted.
fn asset_is_deleted(asset: &AssetState) -> bool {
    let mut deleted = false;
    for rec in asset.chain.records() {
        match rec.manifest.core.action {
            Action::Delete => deleted = true,
            Action::TrashRestore => deleted = false,
            _ => {}
        }
    }
    deleted
}

impl Workspace {
    /// The account's user id.
    pub fn user_id(&self) -> Uuid {
        self.account.user_id
    }

    /// This account's user identity **public** key — the key an album-group assertion (and any
    /// user-IK-signed artifact this workspace authors) verifies against.
    pub fn user_ik_public(&self) -> HybridVerifyingKey {
        self.account.user_ik.verifying_key()
    }

    /// The account's default album id (derived from the master key).
    pub fn default_album_id(&self) -> Uuid {
        self.account.master.derive_default_album_id()
    }

    /// The library root directory this workspace writes through. The streaming executor probes
    /// its volume's free space ([`available_bytes`](crate::library::available_bytes)) for the
    /// minimum-headroom check.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn provenance_path(&self, asset: &AssetState) -> PathBuf {
        media_dir(&self.root, asset.capture_utc)
            .join(format!("{}.provenance.cbor", asset.asset_id.simple()))
    }
    fn sidecar_path(&self, asset: &AssetState) -> PathBuf {
        media_dir(&self.root, asset.capture_utc).join(format!("{}.cbor", asset.asset_id.simple()))
    }
    fn media_path(&self, asset: &AssetState) -> PathBuf {
        media_dir(&self.root, asset.capture_utc).join(format!(
            "{}.{}",
            asset.asset_id.simple(),
            asset.ext
        ))
    }

    /// The plaintext bytes of a managed asset (reads from disk).
    pub fn read_plaintext(&self, asset_id: &Uuid) -> Result<Vec<u8>> {
        let asset = self
            .assets
            .get(asset_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("asset {asset_id}")))?;
        fs::read(self.media_path(asset)).map_err(|e| LifecycleError::Io(e.to_string()))
    }

    /// All managed asset ids.
    pub fn asset_ids(&self) -> Vec<Uuid> {
        self.assets.keys().copied().collect()
    }

    /// A managed asset's current state.
    pub fn asset(&self, asset_id: &Uuid) -> Option<&AssetState> {
        self.assets.get(asset_id)
    }

    /// The library's queryable SQLite index — the timeline, user tags, and cached representations
    /// the crypto lifecycle writes through to.
    pub fn db(&self) -> &DatabaseDriver {
        &self.library.db
    }
}

/// A fast-Argon2 workspace over `dir` — the shared fixture every `lifecycle` test module
/// builds on (the production cost would dominate the suite's runtime).
#[cfg(test)]
fn fast_workspace(dir: &Path) -> Workspace {
    Workspace::create_with_params(
        dir,
        b"passphrase",
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        },
    )
    .unwrap()
}
