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
//! [`verify_asset`]: fn@crate::crypto::verify_asset
//! [`ReferenceAuthority`]: crate::crypto::authority::ReferenceAuthority

mod album;
mod backup;
mod drops;
mod groups;
mod import;
mod metadata;
mod open;
mod organize;
mod provenance;
mod sharing;
mod sync_apply;
mod upload;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use thiserror::Error;
use uuid::Uuid;

use self::drops::{InboxEntry, IssuedLink};
pub use self::open::HardwareDekBinding;
pub use self::sync_apply::{QuarantineReason, RemoteAssetFacts, RemoteEntry, SyncApplyOutcome};
pub use self::upload::{DerivativeBlob, UploadBundle};
use crate::backup::BackupError;
use crate::crypto::CryptoError;
use crate::crypto::authority::Authority;
use crate::crypto::hash::Hash32;
use crate::crypto::keys::albumstore::AlbumStoreError;
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
use crate::sidecar::sidecar_v1::{Gps, SidecarV1, StackMembership, StackRole};

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
    /// The durable album-key store could not be read or written (slice `S-A10`). Never
    /// swallowed: losing album keys silently is exactly the failure this store exists to fix.
    #[error(transparent)]
    AlbumStore(#[from] AlbumStoreError),
    /// The album holds AMKs but no write capability — it was recovered from a backup artifact,
    /// which escrows content keys (read access) but never the write-tier / admin signing keys or
    /// the admin-signed epoch ledger. Its assets decrypt and re-export; authoring a *new* write
    /// into it needs its authority re-established first.
    #[error("album {0} is read-only: recovered without write-tier/admin key material")]
    AlbumReadOnly(Uuid),
    /// The ciphertext re-derived from a manifest's recorded `nonce_prefix` does not
    /// content-address to that manifest's `ciphertext_hash` — the library's bytes and its own
    /// signed manifest disagree. Never papered over: it is the one signal that what would go
    /// on the wire is not what the asset's provenance vouches for.
    #[error("asset {0}: re-derived ciphertext does not match the manifest's ciphertext_hash")]
    CiphertextMismatch(Uuid),
    /// [`create_album_with_id`](Workspace::create_album_with_id) was called for an album that
    /// already holds key material. Minting over it would discard the AMKs every existing asset in
    /// that album was encrypted under, so it is refused; use
    /// [`ensure_album`](Workspace::ensure_album) for resolve-or-create.
    #[error("album {0} already exists")]
    AlbumExists(Uuid),
}

type Result<T> = std::result::Result<T, LifecycleError>;

/// One album's key material across one or more epochs.
///
/// Capsule separates **secrecy** from **authorization**: the per-epoch `amks` are the content
/// keys (read access), while `write_tier` / `admin` are signing *capabilities*. A backup artifact
/// escrows only the former, so an album restored into a library that never held it arrives with
/// its content keys and no capabilities — see [`AlbumStore`](crate::crypto::keys::AlbumStore).
/// That state is modelled honestly here rather than papered over by minting fresh signing keys
/// that attest nothing and no peer trusts.
pub struct AlbumKeys {
    /// Album id.
    pub album_id: Uuid,
    /// Display name.
    pub name: String,
    /// AMKs by epoch.
    pub amks: BTreeMap<u32, [u8; 32]>,
    /// Per-album write-tier signing key; `None` for a read-only recovered album.
    pub write_tier: Option<HybridSigningKey>,
    /// Per-album admin signing key (the epoch-ledger root); `None` for a read-only recovered
    /// album.
    pub admin: Option<HybridSigningKey>,
    /// The current (highest) epoch — the one new imports are written under.
    pub current_epoch: u32,
}

impl AlbumKeys {
    /// The write-tier signing key every asset manifest in this album is signed under, or
    /// [`LifecycleError::AlbumReadOnly`] if this album carries no write capability.
    pub fn write_tier_signer(&self) -> Result<&HybridSigningKey> {
        self.write_tier
            .as_ref()
            .ok_or(LifecycleError::AlbumReadOnly(self.album_id))
    }

    /// The admin signing key that roots the epoch ledger, or [`LifecycleError::AlbumReadOnly`].
    pub fn admin_signer(&self) -> Result<&HybridSigningKey> {
        self.admin
            .as_ref()
            .ok_or(LifecycleError::AlbumReadOnly(self.album_id))
    }
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

/// The **index projection** of an asset's stack membership: the two `assets` columns
/// (`stack_id`, `is_stack_hidden`) that keep a non-primary member out of the timeline.
///
/// Since `S-B15` this is a *derived* value, not a source of truth: the durable record is the
/// sidecar's signed `stack_membership` register, which both the importer and
/// [`Workspace::set_stack_membership`] write. It survives here for the one case the register
/// cannot serve — an asset imported **before** `S-B15`, whose placement was written only to the
/// index and therefore exists nowhere else (see [`Workspace::open`] step (6) and
/// [`library::rebuild_index`](crate::library::rebuild_index)).
#[derive(Debug, Clone)]
pub struct StackPlacement {
    /// The shared stack id (the `asset_stacks` row id the members belong to).
    pub stack_id: String,
    /// Whether this member is hidden from the timeline (true for every non-primary member).
    pub hidden: bool,
}

impl StackPlacement {
    /// The index projection of a signed [`StackMembership`] — the one mapping the write path and
    /// [`library::rebuild`](crate::library::rebuild) must agree on: a member is suppressed from
    /// the timeline exactly when it is not the stack's primary.
    pub(crate) fn from_membership(m: &StackMembership) -> Self {
        Self {
            stack_id: m.stack_id.to_string(),
            hidden: m.role != StackRole::Primary,
        }
    }
}

/// Out-of-band metadata a third-party [source adapter](crate::import::SourceAdapter) folded for
/// one media file, in the shape the signed sidecar stores it (slice `S-B10`).
///
/// The [precedence rule] is resolved in two places, and this type is what keeps the two halves
/// apart:
///
/// - [`capture_time`](Self::capture_time) and [`gps`](Self::gps) are **fallbacks**. The file's
///   own embedded EXIF wins wherever it yields a value; these are consulted only where it does
///   not. Because the adapter already folded EXIF-over-exporter at extraction, the value carried
///   here is the *winner* of that fold — so an EXIF capture time the write site cannot resolve
///   to a UTC instant by itself (a floating `DateTimeOriginal` carrying no offset) still beats
///   the exporter's record instead of silently losing to it.
/// - [`caption`](Self::caption), [`rating`](Self::rating) and [`tags`](Self::tags) are
///   **exporter-authoritative** — constructs the file bytes never carried — so they are written
///   unconditionally, stamped `(now, device_id)` like any other register write.
///
/// Every field left empty writes nothing, so an import carrying no exporter record produces a
/// sidecar byte-identical to a plain filesystem import's. The provider-specific mapping that
/// fills this in lives in [`import::sidecar_enrichment`](crate::import::sidecar_enrichment).
///
/// [precedence rule]: https://docs/design/import/pipeline/#third-party-importers
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SidecarEnrichment {
    /// The adapter's folded capture time — used only when the file's own EXIF resolves none.
    pub capture_time: Option<Timestamp>,
    /// The adapter's folded GPS fix — used only when the file's own EXIF carries none.
    pub gps: Option<Gps>,
    /// The exporter's user-typed description, bounded to the schema's caption limit.
    pub caption: Option<String>,
    /// The exporter's favorite/star flag, already mapped onto the sidecar's star scale.
    pub rating: Option<u8>,
    /// Exporter-authoritative user tags (Takeout's album membership); each becomes one
    /// `tags_user` OR-set add with its own `add_id`.
    pub tags: Vec<String>,
}

/// Options for a signed import driven by the import executor. Defaults (`Copy` mode, no stack,
/// no exporter metadata) reproduce the standalone [`Workspace::import_asset`] behaviour.
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
    /// Signed stack membership for a multi-file candidate member (`S-B15`). Written into the
    /// sidecar's `stack_membership` LWW register — the same durable write
    /// [`Workspace::set_stack_membership`] performs — and projected from there onto the index
    /// row. `None` imports a standalone asset and leaves the register wire-absent.
    pub stack: Option<StackMembership>,
    /// Folded third-party exporter metadata for this file (`S-B10`), attached by the
    /// [executor](crate::import::execute_with_source_metadata) when the import came
    /// from a [source adapter](crate::import::SourceAdapter). `None` — a plain filesystem
    /// import — leaves every enriched field exactly as it was before the slice.
    pub enrichment: Option<SidecarEnrichment>,
}

/// Whether an imported asset got thumbnail/preview derivatives — and if not, **why**
/// (slice `S-B13`).
///
/// Capsule is a backup tool, so this never gates admission: the original is imported as a
/// signed, encrypted, verifiable asset in every case below. What varies is only whether a
/// thumbnail/preview and an LQIP could be produced alongside it. The point of the enum is to
/// keep the two "no derivative" reasons apart, because one is expected and one is a bug:
/// a missing codec is a known, deferred gap, while a *supported* format that fails to decode
/// is a real problem someone should look at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DerivativeStatus {
    /// The still decoded: dimensions and LQIP came from real pixels, and signed derivatives
    /// were generated if a still encoder is attached to the workspace.
    Decoded,
    /// **Expected deferral.** This build links no codec for the asset's format — the
    /// supported-image-format table lives in the retired media stack. The original is safely
    /// backed up; dimensions fall back to EXIF and there is no LQIP or preview until the codec
    /// lands, at which point derivatives can be backfilled from the stored original. Counted by
    /// [`ImportExecutionSummary::deferred_derivative_count`](crate::import::ImportExecutionSummary::deferred_derivative_count).
    ///
    /// This build links no codecs at all — the media stack is retired to `legacy-review/`
    /// (`S-B1`) — so every still it imports reports this.
    DeferredNoCodec,
    /// **A real problem.** The format *is* one this build can decode, but these particular
    /// bytes did not decode — truncation, corruption, or a decoder bug. The original is still
    /// imported (the bytes are backed up verbatim, whatever they are), but this is worth
    /// investigating rather than shrugging at.
    DecodeFailed,
    /// Nothing to decode: the extension names no still image this build models — a video, an
    /// XMP sidecar, an unknown suffix, or an exotic RAW flavour the raw-image-format table has
    /// no variant for. Video derivatives are generated on their own path.
    NotAKnownStill,
}

impl DerivativeStatus {
    /// Whether the asset was imported *without* thumbnail/preview derivatives because no codec
    /// exists for it — the deferred-gap count, excluding genuine decode failures and non-stills.
    pub const fn is_deferred_for_missing_codec(self) -> bool {
        matches!(self, Self::DeferredNoCodec)
    }
}

/// What one signed import produced: the asset's id plus whether derivatives could be generated
/// for it.
///
/// Returned by [`Workspace::import_asset_with`] so the executor can report the derivative gap
/// without re-deriving it from the file extension.
/// [`import_asset`](Workspace::import_asset) keeps returning the bare [`Uuid`] for the many
/// callers that only need the id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignedImport {
    /// The imported asset's id.
    pub asset_id: Uuid,
    /// Whether thumbnail/preview derivatives were generated, and if not, why.
    pub derivatives: DerivativeStatus,
}

/// A streamed import: everything the [streaming window](crate::import::execute_streaming) needs
/// about one just-imported asset to drive its upload → verify → release step, without exposing
/// workspace internals. Produced by [`Workspace::import_asset_streaming`], which commits on the
/// signed path with source release **deferred** to the server-side verify-before-destroy gate
/// (`S-D4`), since in streaming mode the local bytes are the only copy until the *server* durably
/// holds them.
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
    /// offline [`ReferenceAuthority`](crate::crypto::authority::ReferenceAuthority) is the shipped
    /// default; the enum lets the live
    /// [`OpenMlsAuthority`](crate::crypto::authority::OpenMlsAuthority) drop in without the
    /// lifecycle naming a concrete backend. **Persisted** alongside the album keys in
    /// [`AlbumStore`](crate::crypto::keys::AlbumStore) and restored on open (`S-A10`) — without
    /// it a reopened library could hold an AMK and still not verify a single manifest.
    authorities: HashMap<Uuid, Authority>,
    assets: HashMap<Uuid, AssetState>,
    /// The reconciled [aggregated-album](crate::federation) group assertion for each album the
    /// workspace can see, keyed by album id (`S-E4`). An own album's entry is self-authored via
    /// [`create_album_group`](Self::create_album_group) / [`join_album_group`](Self::join_album_group);
    /// a peer constituent's entry is folded in from the feed via
    /// [`merge_album_group_assertion`](Self::merge_album_group_assertion). Removing an entry is a
    /// group *leave* — it drops the constituent from every viewer's aggregate on their next sync.
    ///
    /// **Deliberately session-scoped** (`S-A10`): every entry is a self-verifying signed
    /// assertion that the federation feed re-delivers, so it is a cache of reconcilable state,
    /// not key material. Losing it on close costs one reconcile, not access.
    group_assertions: HashMap<Uuid, AlbumGroupAssertion>,
    /// The open, locked library — its `library.sqlite` is the queryable index the crypto
    /// lifecycle writes through to. Held for the workspace's lifetime so the lock is retained.
    library: Library,
    /// Argon2id cost the account was created under; reused for the optional passphrase wrap
    /// on share links so a share's cost matches the device tier.
    argon2_params: Argon2Params,
    /// Issued share links keyed by their revocation handle — the authoritative link records
    /// the serving endpoint (S-C4) consults for scope, expiry, and revocation state.
    ///
    /// **Deliberately session-scoped** (`S-A10`): the durable registry these mirror lives on the
    /// serving side, and a link's *secret* travels in the URL rather than here, so persisting the
    /// local copy is a link-management concern (revoke-after-restart) rather than a data-plane
    /// key-loss one. Tracked with the share-link slices, not with album keys.
    share_links: HashMap<ShareLinkId, ShareLinkRecord>,
    /// Issued upload (guest-drop) links keyed by their revocation handle. Each holds the
    /// escrow-wrapped Drop Key private half so any of this owner's adopting devices can
    /// later decapsulate a drop sealed to it (SSoT: [Web Upload]).
    ///
    /// **Session-scoped, and the one deferral here that is real key material** (`S-A10`): closing
    /// a workspace with an outstanding upload link drops the escrowed Drop Key private half, so a
    /// drop sealed to that link can no longer be adopted. It is deferred rather than folded into
    /// [`AlbumStore`](crate::crypto::keys::AlbumStore) because the link registry is
    /// server-authoritative and its durable shape belongs with the guest-drop slices — an album
    /// keystore is the wrong home for a per-link escrow table. Until then a guest-drop flow must
    /// complete within one workspace session.
    ///
    /// [Web Upload]: https://docs/design/web-upload/
    upload_links: HashMap<UploadLinkId, IssuedLink>,
    /// Pending guest drops in this user's inbox, keyed by drop id. Models the server's
    /// staging store (the server's drop module, `S-C5`) so the offline core can drive the
    /// full seal → stage → adopt path; a real client fills it from server responses.
    ///
    /// **Deliberately session-scoped** (`S-A10`): the server's staging store is the authority and
    /// a client refills this from it, so there is nothing here to lose.
    inbox: HashMap<DropId, InboxEntry>,
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

    /// This device's stable id — the `created_by_device` every manifest this workspace
    /// authors carries, and the [`DeviceEntry`](crate::crypto::keys::DeviceEntry)
    /// key under which its signing key is published in the device directory.
    pub fn device_id(&self) -> Uuid {
        self.account.device.device_id
    }

    /// This workspace's **signed device directory** — the user-IK-signed list of enrolled
    /// device signing keys `verify_asset` resolves a manifest's `created_by_device` against.
    ///
    /// Exposed so a client can publish it (the `S-C9` device-directory surface, driven from
    /// the SDK by `capsule_sdk::directory`). The directory is a signed, self-verifying
    /// document: it carries no private key material, and a reader checks it under
    /// [`user_ik_public`](Self::user_ik_public) before trusting a single entry.
    pub fn device_directory(&self) -> &DeviceDirectory {
        &self.directory
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
    /// `media/{YYYY}/{YYYY-MM}/{uuid}.metadata.bin` — the **exact** sealed metadata-blob wire
    /// bytes the asset's metadata-bearing manifest commits to via `metadata_blob_hash`
    /// ([`AssetState::metadata_blob`]).
    ///
    /// Persisted since `S-A10`. The blob is AMK ciphertext, so it is safe at rest beside the
    /// plaintext original, and it cannot be regenerated: [`seal_metadata_blob`] draws a fresh
    /// nonce per call which is folded into the blob key, so re-sealing the same sidecar produces
    /// different bytes and a different content address. Without these bytes on disk a reopened
    /// library can neither `export_backup` nor upload the asset.
    ///
    /// [`seal_metadata_blob`]: crate::crypto::encryption::seal_metadata_blob
    fn metadata_blob_path(&self, asset: &AssetState) -> PathBuf {
        media_dir(&self.root, asset.capture_utc)
            .join(format!("{}.metadata.bin", asset.asset_id.simple()))
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

    /// This device's published **DEK** public encapsulation key — the bytes a peer (or this
    /// account's own escrow path) wraps a key to so only this device can open it.
    ///
    /// Length-tagged by composition (`S-F8`): 1216 bytes for the software X-Wing DEK, 1249 for
    /// the hardware-bound P-256 hybrid, so a recipient never has to be told which it is holding
    /// and the two can never be confused for one another.
    pub fn device_dek_public(&self) -> Vec<u8> {
        self.account.device.dek.public_bytes()
    }

    /// Recover the 32-byte shared secret from a ciphertext sealed to
    /// [`device_dek_public`](Self::device_dek_public).
    ///
    /// When the DEK is hardware-bound this performs the classical ECDH **inside the secure
    /// element** — the device's P-256 scalar is never in this process's memory — so it can fail on
    /// a cancelled biometric or an evicted key as well as on a malformed ciphertext.
    pub fn device_dek_decapsulate(&self, ciphertext: &[u8]) -> Result<[u8; 32]> {
        Ok(self.account.device.dek.decapsulate(ciphertext)?)
    }

    /// Whether this workspace's DEK has its classical half in a secure element (`S-F8`) rather
    /// than in software — the honest answer to "is this device actually hardware-bound", read
    /// from the account's own recorded binding rather than from whether an element was offered.
    pub fn device_dek_is_hardware_bound(&self) -> bool {
        self.account.device.dek.is_hardware_bound()
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
