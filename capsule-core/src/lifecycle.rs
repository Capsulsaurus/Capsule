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

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use thiserror::Error;
use uuid::Uuid;

use crate::backup::{self, BackupArtifact, BackupAsset, BackupInput, RestoreMode};
use crate::cbor;
use crate::crypto::CryptoError;
use crate::crypto::authority::ReferenceAuthority;
use crate::crypto::encryption::keywrap::seal_file_key;
use crate::crypto::encryption::{
    blob_ciphertext_hash, blob_nonce, encrypt_asset_rekey, seal_metadata_blob, stream,
};
use crate::crypto::hash::{self, Hash32};
use crate::crypto::keys::directory::{DeviceEntry, DirectoryCore};
use crate::crypto::keys::{
    Account, AccountFile, Amk, AmkVersion, DekKeypair, DeviceDirectory, HybridSigningKey,
    HybridVerifyingKey, Signer,
};
use crate::crypto::primitives::{Argon2Params, CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use crate::crypto::provenance::action::Action;
use crate::crypto::provenance::manifest::{
    ASSET_MANIFEST_VERSION, KeyMode, ManifestCore, WrappedFileKey,
};
use crate::crypto::provenance::{AssetManifest, ProvenanceChain, ProvenanceRecord};
use crate::crypto::verify_asset::{
    MetadataBinding, VerifyOutcome, verify_asset, verify_metadata_binding,
};
use crate::db::{AssetRow, CachedRepresentationRow, DatabaseDriver};
// The `UploadLinkIssuer` / `DropAdopter` traits are referenced by full path in their impl
// headers below and pulled into scope locally at their (UFCS) call sites — keeping them out
// of module scope avoids a `create_link`/`revoke_link` name clash with `ShareLinkIssuer`.
use crate::drop::{
    DropDescriptor, DropError, DropId, LinkCaps, PassphraseVerifier, PendingDrop, SealedDrop,
    UploadLink, UploadLinkId, generate_opaque_id as generate_drop_id, open_drop_key,
};
use crate::exif::extract::extract_exif;
use crate::exif::timezone::resolve_timezone;
use crate::library::Library;
#[cfg(feature = "media")]
use crate::media::image::derivative::{
    DerivativeContext, DerivativeFormat, DerivativeTier, GeneratedDerivative,
    generate_still_derivatives,
};
use crate::metadata::crdt::{AddId, Counter, Lww};
use crate::sharing::{
    LINK_SECRET_LEN, RevocationRecord, ScopeMaterial, ShareLink, ShareLinkId, ShareLinkIssuer,
    ShareLinkRecord, ShareScope, SharingError, encapsulate_scope, generate_opaque_id,
};
use crate::sidecar::sidecar_v1::{Dimensions, Gps, GpsSource, SIDECAR_SCHEMA_V1, SidecarV1};

/// A device is treated as added far in the past so any import timestamp postdates it.
const DEVICE_ADDED_AT: &str = "2020-01-01T00:00:00Z";

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
    Backup(#[from] backup::BackupError),
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
    counter: Counter,
    albums: HashMap<Uuid, AlbumKeys>,
    authorities: HashMap<Uuid, ReferenceAuthority>,
    assets: HashMap<Uuid, AssetState>,
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

/// The issuer-held state for one live upload link. The Drop Key private half is **escrowed**
/// (sealed under the account master key) so it never sits in the clear at rest, mirroring
/// how the account file seals device keys; the public half is returned to the caller for the
/// URL fragment and never persisted server-side.
struct IssuedLink {
    /// The random 128-bit opaque URL-path id.
    opaque_id: [u8; 16],
    /// The per-link caps the serve path (S-C5) enforces at drop-session creation.
    caps: LinkCaps,
    /// The Drop Key public half (KEM encapsulation key; travels in the URL fragment only).
    drop_pubkey: Vec<u8>,
    /// The Drop Key private half (X-Wing 32-byte seed) sealed under the account master key.
    escrowed_drop_seed: Vec<u8>,
    /// Optional Argon2id write-abuse-gate verifier.
    passphrase: Option<PassphraseVerifier>,
    /// RFC 3339 issuance time.
    created_at: String,
    /// Set once revoked; the serve path then refuses the link.
    revoked_at: Option<String>,
}

/// One pending drop staged in the owner's inbox: the guest's unsigned descriptor, the sealed
/// ciphertext (the server-held blob, referenced in place on adoption), and the link it
/// arrived through (so adoption can unwrap the right Drop Key).
struct InboxEntry {
    descriptor: DropDescriptor,
    ciphertext: Vec<u8>,
    via_link: UploadLinkId,
    received_at: String,
}

fn now_rfc3339() -> String {
    Timestamp::now().to_string()
}

/// Render a Unix-second capture time as the sidecar's RFC 3339 `capture_timestamp`.
fn capture_rfc3339(secs: i64) -> String {
    Timestamp::from_second(secs)
        .unwrap_or(Timestamp::UNIX_EPOCH)
        .to_string()
}

fn content_type_for(ext: &str) -> String {
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "heic" => "image/heic",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
    .to_string()
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

fn asset_type_for(content_type: &str) -> String {
    if content_type.starts_with("video/") {
        "video"
    } else {
        "photo"
    }
    .to_string()
}

fn rfc3339_to_secs(s: &str) -> i64 {
    s.parse::<Timestamp>().map_or(0, Timestamp::as_second)
}

/// Map a managed asset's in-memory state to its queryable `assets` index row. Deletion state is
/// derived from the provenance chain's lifecycle actions; media-derived fields (dimensions,
/// duration, chromahash) stay NULL — they are out of scope in this offline core.
fn asset_row_from_state(asset: &AssetState) -> AssetRow {
    let mut is_deleted = false;
    let mut deleted_at = None;
    for rec in asset.chain.records() {
        match rec.manifest.core.action {
            Action::Delete => {
                is_deleted = true;
                deleted_at = Some(rfc3339_to_secs(&rec.manifest.core.timestamp));
            }
            Action::TrashRestore => {
                is_deleted = false;
                deleted_at = None;
            }
            _ => {}
        }
    }
    AssetRow {
        uuid: asset.asset_id.to_string(),
        asset_type: asset_type_for(&asset.sidecar.content_type),
        capture_timestamp: asset.capture_utc,
        capture_utc: Some(asset.capture_utc),
        capture_tz_source: None,
        import_timestamp: rfc3339_to_secs(&asset.sidecar.import_timestamp),
        hash_sha256: asset.sidecar.hash.to_hex(),
        width: asset.sidecar.dimensions.as_ref().map(|d| d.width as i64),
        height: asset.sidecar.dimensions.as_ref().map(|d| d.height as i64),
        duration_ms: None,
        stack_id: asset.stack.as_ref().map(|s| s.stack_id.clone()),
        is_stack_hidden: asset.stack.as_ref().is_some_and(|s| s.hidden),
        chromahash: None,
        dominant_color: None,
        album_id: Some(asset.album_id.to_string()),
        rating: asset.sidecar.rating.get().copied().unwrap_or(0) as i64,
        is_deleted,
        deleted_at,
    }
}

impl Workspace {
    /// Create a fresh workspace: initialise the library directory and a new account, and
    /// publish a device directory. `passphrase` guards the on-disk account; `tier` sets the
    /// Argon2id cost.
    pub fn create(
        root: &Path,
        passphrase: &[u8],
        tier: crate::crypto::primitives::DeviceTier,
    ) -> Result<Self> {
        Self::create_with_params(root, passphrase, tier.params())
    }

    /// As [`create`](Self::create) but with explicit Argon2id parameters (tests use a fast cost).
    pub fn create_with_params(
        root: &Path,
        passphrase: &[u8],
        params: crate::crypto::primitives::Argon2Params,
    ) -> Result<Self> {
        Self::create_inner(root, passphrase, params, None)
    }

    /// As [`create_with_params`](Self::create_with_params) but signs with a caller-supplied
    /// device signer — e.g. a [hardware-backed key](crate::crypto::keys::HardwareBackedSigner)
    /// (Secure Enclave / StrongBox / TPM). The published device directory and every asset
    /// manifest are then signed by `device_signer`, and its public half is what peers trust.
    pub fn create_with_hardware_signer(
        root: &Path,
        passphrase: &[u8],
        params: crate::crypto::primitives::Argon2Params,
        device_signer: Box<dyn Signer>,
    ) -> Result<Self> {
        Self::create_inner(root, passphrase, params, Some(device_signer))
    }

    fn create_inner(
        root: &Path,
        passphrase: &[u8],
        params: crate::crypto::primitives::Argon2Params,
        device_signer: Option<Box<dyn Signer>>,
    ) -> Result<Self> {
        let library = crate::library::init::init_library(root, "Capsule")
            .map_err(|e| LifecycleError::Io(format!("init library: {e}")))?;
        let account = Account::create();
        let file = account.to_file_with(passphrase, params)?;
        let acct_bytes =
            cbor::to_canonical_vec(&file).map_err(|e| LifecycleError::Cbor(e.to_string()))?;
        fs::write(root.join(".library").join("account.cbor"), &acct_bytes)
            .map_err(|e| LifecycleError::Io(e.to_string()))?;

        // Default to the account's own software DSK; a hardware signer overrides it.
        let device_signer: Box<dyn Signer> =
            device_signer.unwrap_or_else(|| Box::new(account.device.dsk.clone()));
        let directory = Self::build_directory(&account, device_signer.verifying_key());
        let counter = Counter::new(account.device.device_id);
        Ok(Self {
            root: root.to_path_buf(),
            account,
            device_signer,
            directory,
            counter,
            albums: HashMap::new(),
            authorities: HashMap::new(),
            assets: HashMap::new(),
            library,
            argon2_params: params,
            share_links: HashMap::new(),
            upload_links: HashMap::new(),
            inbox: HashMap::new(),
            #[cfg(feature = "media")]
            still_encoder: None,
        })
    }

    /// Attach the per-platform [`StillEncoder`](crate::media::image::derivative::StillEncoder) so
    /// signed imports generate thumbnail/preview derivatives + LQIP (S-B1 → S-B2). Without it,
    /// imports are signed-original-only.
    #[cfg(feature = "media")]
    #[must_use]
    pub fn with_still_encoder(
        mut self,
        encoder: Box<dyn crate::media::image::derivative::StillEncoder>,
    ) -> Self {
        self.still_encoder = Some(encoder);
        self
    }

    /// Open an **existing** library at `root` as a signed workspace, unlocking (or, on first use,
    /// creating + persisting) the account under `passphrase`. `params` sets the Argon2id cost for
    /// a first-time account and the share-link wrap tier. Album key material is session-scoped
    /// (minted per run via [`create_album`](Self::create_album)); durable album-key persistence is
    /// a separate concern tracked in `SLICES.md`.
    pub fn open(
        root: &Path,
        passphrase: &[u8],
        params: crate::crypto::primitives::Argon2Params,
    ) -> Result<Self> {
        let library = crate::library::open_library(root)
            .map_err(|e| LifecycleError::Io(format!("open library: {e}")))?;
        let account_path = root.join(".library").join("account.cbor");
        let account = if account_path.exists() {
            let bytes = fs::read(&account_path).map_err(|e| LifecycleError::Io(e.to_string()))?;
            let file: AccountFile =
                cbor::from_slice(&bytes).map_err(|e| LifecycleError::Cbor(e.to_string()))?;
            file.unlock(passphrase)?
        } else {
            let account = Account::create();
            let file = account.to_file_with(passphrase, params)?;
            let acct_bytes =
                cbor::to_canonical_vec(&file).map_err(|e| LifecycleError::Cbor(e.to_string()))?;
            fs::write(&account_path, &acct_bytes).map_err(|e| LifecycleError::Io(e.to_string()))?;
            account
        };

        let device_signer: Box<dyn Signer> = Box::new(account.device.dsk.clone());
        let directory = Self::build_directory(&account, device_signer.verifying_key());
        let counter = Counter::new(account.device.device_id);
        Ok(Self {
            root: root.to_path_buf(),
            account,
            device_signer,
            directory,
            counter,
            albums: HashMap::new(),
            authorities: HashMap::new(),
            assets: HashMap::new(),
            library,
            argon2_params: params,
            share_links: HashMap::new(),
            upload_links: HashMap::new(),
            inbox: HashMap::new(),
            #[cfg(feature = "media")]
            still_encoder: None,
        })
    }

    fn build_directory(account: &Account, dsk_public: HybridVerifyingKey) -> DeviceDirectory {
        DirectoryCore {
            user_id: account.user_id,
            directory_version: 1,
            updated_at: now_rfc3339(),
            devices: vec![DeviceEntry {
                device_id: account.device.device_id,
                dsk_public,
                added_at: DEVICE_ADDED_AT.into(),
                revoked_at: None,
            }],
        }
        .sign(&account.user_ik)
    }

    /// The account's user id.
    pub fn user_id(&self) -> Uuid {
        self.account.user_id
    }

    /// The account's default album id (derived from the master key).
    pub fn default_album_id(&self) -> Uuid {
        self.account.master.derive_default_album_id()
    }

    /// Create a container album: mint AMK_v1 + write-tier + admin keys and an attested
    /// authority. Returns the new album id.
    pub fn create_album(&mut self, name: &str) -> Uuid {
        self.create_album_with_id(Uuid::now_v7(), name)
    }

    /// Create an album with a specific id (e.g. the derived default-album id).
    pub fn create_album_with_id(&mut self, album_id: Uuid, name: &str) -> Uuid {
        let amk = Amk::generate();
        let write_tier = HybridSigningKey::generate();
        let admin = HybridSigningKey::generate();
        let mut amks = BTreeMap::new();
        amks.insert(1, *amk.as_bytes());

        let authority = ReferenceAuthority::new(album_id, admin.verifying_key()).with_epoch(
            &admin,
            AmkVersion(1),
            &write_tier.verifying_key(),
            true,
        );
        self.authorities.insert(album_id, authority);
        self.albums.insert(
            album_id,
            AlbumKeys {
                album_id,
                name: name.to_string(),
                amks,
                write_tier,
                admin,
                current_epoch: 1,
            },
        );
        album_id
    }

    /// Rotate `album_id` to a fresh epoch: mint AMK_v{n+1} and a new write-tier key, have the
    /// album admin attest the new epoch, and advance the current epoch — the design's
    /// "AMK bump + write-tier rotation are one commit" atomicity. The admin key (the ledger
    /// root) is stable across epochs, and existing assets stay verifiable under their original
    /// epoch. Returns the new epoch. Membership changes / the MLS `Welcome` flow remain deferred
    /// (see `SLICES.md`).
    pub fn rotate_epoch(&mut self, album_id: Uuid) -> Result<u32> {
        let next = {
            let album = self
                .albums
                .get_mut(&album_id)
                .ok_or_else(|| LifecycleError::NotFound(format!("album {album_id}")))?;
            let next = album.current_epoch + 1;
            album.amks.insert(next, *Amk::generate().as_bytes());
            album.write_tier = HybridSigningKey::generate();
            album.current_epoch = next;
            next
        };
        // Disjoint fields: read the album's keys while mutably attesting in its authority.
        let album = self.albums.get(&album_id).expect("album just mutated");
        let authority = self
            .authorities
            .get_mut(&album_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("authority {album_id}")))?;
        authority.attest_epoch(
            &album.admin,
            AmkVersion(next),
            &album.write_tier.verifying_key(),
            true,
        );
        Ok(next)
    }

    fn album(&self, album_id: &Uuid) -> Result<&AlbumKeys> {
        self.albums
            .get(album_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("album {album_id}")))
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

    /// Re-derive the per-file key under a *specific* epoch's AMK for a known `nonce_prefix`
    /// (the value recorded in the manifest). Callers pass the epoch the asset was written
    /// under (`amk_version`), never assuming the album's current epoch — so an asset imported
    /// before a rotation still derives the key it was encrypted with. Because the fresh
    /// `nonce_prefix` is folded into the salt, this is the read/regenerate path; a *fresh*
    /// write goes through [`encrypt_asset_rekey`], which draws the nonce and derives together.
    fn file_key(
        &self,
        album: &AlbumKeys,
        epoch: u32,
        file_id: &Uuid,
        nonce_prefix: &[u8],
    ) -> [u8; 32] {
        let amk = Amk::from_bytes(album.amks[&epoch]);
        amk.derive_file_key(file_id, nonce_prefix)
    }

    /// Build a signed lifecycle manifest for `asset`, sharing the create manifest's content
    /// fields. Used for metadata-update / delete / trash-restore. `metadata_blob_hash` is set
    /// explicitly per the presence-by-action rule (`Some` for a metadata-update that seals a
    /// fresh blob, `None` for delete / trash-restore) rather than inherited from `base`.
    fn sign_lifecycle(
        &self,
        album: &AlbumKeys,
        base: &ManifestCore,
        action: Action,
        prior: Option<Hash32>,
        retention_until: Option<String>,
        metadata_blob_hash: Option<Hash32>,
    ) -> std::result::Result<AssetManifest, CryptoError> {
        let core = ManifestCore {
            action,
            prior_provenance_hash: prior,
            retention_until,
            metadata_blob_hash,
            timestamp: now_rfc3339(),
            ..base.clone()
        };
        core.sign(self.device_signer.as_ref(), &album.write_tier)
    }

    fn write_asset_files(&self, asset: &AssetState, plaintext: &[u8]) -> Result<()> {
        let dir = media_dir(&self.root, asset.capture_utc);
        fs::create_dir_all(&dir).map_err(|e| LifecycleError::Io(e.to_string()))?;
        fs::write(self.media_path(asset), plaintext)
            .map_err(|e| LifecycleError::Io(e.to_string()))?;
        fs::write(self.sidecar_path(asset), asset.sidecar.to_canonical_vec())
            .map_err(|e| LifecycleError::Io(e.to_string()))?;
        let prov = cbor::to_canonical_vec(&asset.chain.records().to_vec())
            .map_err(|e| LifecycleError::Cbor(e.to_string()))?;
        fs::write(self.provenance_path(asset), prov)
            .map_err(|e| LifecycleError::Io(e.to_string()))?;
        Ok(())
    }

    /// Write the queryable index row + user tags for `asset` into `library.sqlite`. Re-syncs on
    /// every change (import, metadata edit, soft-delete/restore), so the index reflects the
    /// asset's current rating, tags, and deletion state. Upsert keeps it conflict-safe even
    /// though the legacy importer shares the same `assets` table.
    fn index_asset_row(&self, asset: &AssetState) -> Result<()> {
        self.library
            .db
            .upsert_asset(&asset_row_from_state(asset))
            .map_err(|e| LifecycleError::Db(e.to_string()))?;
        let tags: Vec<String> = asset.sidecar.tags_user.value().into_iter().collect();
        self.library
            .db
            .replace_asset_tags(&asset.asset_id.to_string(), &tags)
            .map_err(|e| LifecycleError::Db(e.to_string()))
    }

    /// Record the asset's own original as a device-owned cache representation — exempt from the
    /// automatic eviction sweep, and the real lifecycle data that sweep then operates on.
    fn index_original_representation(&self, asset: &AssetState, bytes: usize) -> Result<()> {
        self.library
            .db
            .upsert_representation(&CachedRepresentationRow {
                uuid: asset.asset_id.to_string(),
                tier: "original".to_string(),
                format: Some(asset.ext.clone()),
                bytes: bytes as i64,
                path: self.media_path(asset).to_string_lossy().into_owned(),
                last_accessed_at: Timestamp::now().as_second(),
                pinned: false,
                is_owned_original: true,
            })
            .map_err(|e| LifecycleError::Db(e.to_string()))
    }

    /// Import a file into `album_id`: encrypt, build the signed create manifest + provenance,
    /// write the signed sidecar, and self-verify through `verify_asset` **and** the
    /// metadata↔manifest binding. Follows the [sealing order] so the manifest commits to the
    /// content address of the sidecar it seals, without a cycle. Returns the asset id.
    ///
    /// [sealing order]: https://docs/design/metadata/#provenance-binding-and-sealing-order
    pub fn import_asset(&mut self, album_id: Uuid, src: &Path) -> Result<Uuid> {
        self.import_asset_with(album_id, src, &SignedImportOptions::default())
    }

    /// As [`import_asset`](Self::import_asset) but with executor-supplied [`SignedImportOptions`]
    /// (Move-mode source release + stack placement). This is the single signed write path the
    /// import executor drives (S-B2): every imported member lands as a signed `SidecarV1` +
    /// manifest + append-only provenance, self-verified through [`verify_asset`], and — behind
    /// the `media` feature, when a [`StillEncoder`](crate::media::image::derivative::StillEncoder)
    /// is attached — with signed thumbnail/preview derivatives + an LQIP in the sidecar.
    #[tracing::instrument(skip_all, fields(album_id = %album_id, src = %src.display()))]
    pub fn import_asset_with(
        &mut self,
        album_id: Uuid,
        src: &Path,
        opts: &SignedImportOptions,
    ) -> Result<Uuid> {
        let plaintext = fs::read(src)
            .map_err(|e| LifecycleError::Io(format!("read {}: {e}", src.display())))?;
        let ext = src
            .extension()
            .map_or_else(|| "bin".into(), |e| e.to_string_lossy().to_lowercase());
        let asset_id = Uuid::now_v7();

        // Scan & extract: capture time, dimensions, and GPS from the file's EXIF. Missing values
        // degrade cleanly (capture → now; dimensions/GPS → absent).
        let exif = extract_exif(src).unwrap_or_default();
        let tz = resolve_timezone(&exif);
        let capture_utc = tz
            .capture_utc
            .unwrap_or_else(|| Timestamp::now().as_second());
        // EXIF GPS is the near-universal WGS-84 camera datum (metadata doc, Geolocation);
        // stored verbatim, so the wire-absent default datum applies.
        let gps = exif.gps_lat.zip(exif.gps_lon).map(|(lat, lon)| Gps {
            lat,
            lon,
            source: GpsSource::Exif,
            datum: crate::domain::GpsDatum::Wgs84,
        });

        // Still-derived sidecar metadata (dimensions + LQIP) and the derivatives to persist
        // after the commit. Behind `media` this decodes the still once and generates the signed
        // derivatives; without it, dimensions come from EXIF and no derivatives are generated.
        #[cfg(feature = "media")]
        let (dimensions, lqip, pending_derivatives) =
            self.prepare_still(&plaintext, &ext, &exif, asset_id, album_id)?;
        #[cfg(not(feature = "media"))]
        let (dimensions, lqip) = (
            exif.width
                .zip(exif.height)
                .map(|(width, height)| Dimensions { width, height }),
            None::<crate::sidecar::sidecar_v1::Lqip>,
        );

        let album = self.album(&album_id)?;
        let epoch = album.current_epoch;
        let amk = Amk::from_bytes(album.amks[&epoch]);
        // First write: draw a fresh nonce prefix and derive the folded file key together
        // (nothing to replace on a create).
        let (enc, ciphertext, _file_key) = encrypt_asset_rekey(&amk, &asset_id, &plaintext, None)?;

        // Sealing order (1) the prior head `H` is `None` on a create; (2) author + sign the
        // sidecar with `provenance_chain_hash = H`.
        let mut sidecar = SidecarV1 {
            sidecar_schema: SIDECAR_SCHEMA_V1,
            crypto_suite_id: CRYPTO_SUITE_ID,
            uuid: asset_id,
            hash: hash::hash_bytes(&plaintext),
            capture_timestamp: capture_rfc3339(capture_utc),
            import_timestamp: now_rfc3339(),
            content_type: content_type_for(&ext),
            dimensions,
            lqip,
            tags_user: Default::default(),
            tags_ai: Default::default(),
            caption: Default::default(),
            rating: Default::default(),
            stack_membership: Lww::new(),
            cull: Lww::new(),
            hidden: Lww::new(),
            camera_id: None,
            device_id: self.account.device.device_id,
            session_id: Uuid::now_v7(),
            gps,
            provenance_chain_hash: None,
            unknown: BTreeMap::new(),
            signature: None,
        };
        sidecar.sign(&self.account.user_ik);

        // (3) Seal the sidecar into the metadata blob (fresh nonce folded into the blob key;
        // nothing to replace on a create); compute its content hash.
        let (metadata_blob, blob_key) =
            seal_metadata_blob(&amk, &asset_id, &sidecar.to_canonical_vec(), None)?;
        let metadata_blob_hash = blob_ciphertext_hash(&metadata_blob);

        // (4) Build + sign the manifest with `prior_provenance_hash = H` (None) and the
        // `metadata_blob_hash` from (3); append it as the new chain head.
        let core = ManifestCore {
            version: ASSET_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: PROTOCOL_VERSION.into(),
            file_id: asset_id,
            album_id,
            amk_version: AmkVersion(epoch),
            ciphertext_hash: enc.ciphertext_hash,
            plaintext_size: enc.plaintext_size,
            chunk_size: enc.chunk_size,
            nonce_prefix: enc.nonce_prefix,
            key_mode: KeyMode::Derived,
            wrapped_file_key: None,
            metadata_blob_hash: Some(metadata_blob_hash),
            created_by_user: self.account.user_id,
            created_by_device: self.account.device.device_id,
            client_version: concat!("capsule-core/", env!("CARGO_PKG_VERSION")).into(),
            timestamp: now_rfc3339(),
            action: Action::Create,
            prior_provenance_hash: None,
            retention_until: None,
        };
        let manifest = core.sign(self.device_signer.as_ref(), &album.write_tier)?;

        let mut chain = ProvenanceChain::new();
        chain
            .append(ProvenanceRecord {
                asset_id,
                manifest: manifest.clone(),
                prior_provenance_hash: None,
            })
            .map_err(|e| LifecycleError::Cbor(format!("chain: {e}")))?;

        // Self-check: the asset must verify through the one chokepoint, and the sealed metadata
        // blob must round-trip to the signed sidecar and the committed hash, before we accept it.
        let authority = &self.authorities[&album_id];
        let outcome = verify_asset(&manifest, &ciphertext, &self.directory, authority, None);
        if outcome != VerifyOutcome::Accept {
            return Err(LifecycleError::SelfVerify(outcome));
        }
        let binding = verify_metadata_binding(
            &manifest,
            &metadata_blob,
            &blob_key,
            &sidecar.to_canonical_vec(),
        );
        if binding != MetadataBinding::Bound {
            return Err(LifecycleError::MetadataUnbound(binding));
        }

        let asset = AssetState {
            asset_id,
            album_id,
            ext,
            capture_utc,
            chain,
            sidecar,
            metadata_blob,
            stack: opts.stack.clone(),
        };
        self.write_asset_files(&asset, &plaintext)?;
        self.index_asset_row(&asset)?;
        self.index_original_representation(&asset, plaintext.len())?;

        // Persist the signed still derivatives generated pre-commit (media + encoder attached).
        #[cfg(feature = "media")]
        self.persist_derivatives(&asset, &pending_derivatives)?;

        // Move mode: release the source only after the durable, self-verified commit — unless
        // the caller defers release to its server-side verify-before-destroy gate (S-D4/S-B3),
        // where the source is the only copy until the *server* durably holds it.
        if opts.move_source && !opts.defer_source_release {
            let _ = fs::remove_file(src);
        }

        self.assets.insert(asset_id, asset);
        Ok(asset_id)
    }

    /// Run `verify_asset` for a managed asset (regenerating its ciphertext deterministically).
    pub fn verify(&self, asset_id: &Uuid) -> Result<VerifyOutcome> {
        let asset = self
            .assets
            .get(asset_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("asset {asset_id}")))?;
        let album = self.album(&asset.album_id)?;
        let head = &asset
            .chain
            .records()
            .last()
            .expect("provenance chain is never empty")
            .manifest;
        let plaintext =
            fs::read(self.media_path(asset)).map_err(|e| LifecycleError::Io(e.to_string()))?;
        let file_key = self.file_key(
            album,
            head.core.amk_version.0,
            &head.core.file_id,
            &head.core.nonce_prefix,
        );
        let (_, ciphertext) =
            stream::encrypt_asset_vec_with_prefix(&file_key, head.core.nonce_prefix, &plaintext);

        // Walk the whole chain forward; the head is what enters the trusted set.
        let prior = asset
            .chain
            .records()
            .len()
            .checked_sub(2)
            .map(|i| asset.chain.records()[i].record_hash());
        Ok(verify_asset(
            head,
            &ciphertext,
            &self.directory,
            &self.authorities[&asset.album_id],
            prior,
        ))
    }

    fn append_lifecycle(
        &mut self,
        asset_id: &Uuid,
        action: Action,
        retention_until: Option<String>,
        mutate_sidecar: impl FnOnce(&mut SidecarV1, AddId),
    ) -> Result<()> {
        let album_id = self
            .assets
            .get(asset_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("asset {asset_id}")))?
            .album_id;
        // Sealing order (1): the prior head `H` is this asset's current chain head.
        let prior = self.assets[asset_id].chain.head();
        let base = self.assets[asset_id]
            .chain
            .records()
            .last()
            .expect("provenance chain is never empty")
            .manifest
            .core
            .clone();
        let binds = action.binds_metadata_blob();
        let epoch = base.amk_version.0;
        let album_amk = {
            let album = self.album(&album_id)?;
            Amk::from_bytes(album.amks[&epoch])
        };
        // Set by the metadata-bearing branch to the freshly derived blob key (nonce-folded),
        // for the binding self-check below.
        let mut sealed_blob_key: Option<[u8; 32]> = None;

        // Sealing order (2)+(3) for a metadata-bearing action: mutate + re-sign the sidecar with
        // `provenance_chain_hash = H`, then re-seal it under a fresh nonce folded into the blob
        // key (refusing to reuse the superseded nonce) and compute the fresh blob hash.
        // `delete` / `trash-restore` mint no new blob, so the sidecar and its stored blob are
        // left as the last metadata-bearing write produced them (their manifests commit to no
        // blob).
        let metadata_blob_hash = if binds {
            let add_id = self.counter.issue();
            let asset = self
                .assets
                .get_mut(asset_id)
                .expect("asset_id was validated above");
            // The nonce of the blob this update supersedes — refused for the fresh draw.
            let prior_nonce = blob_nonce(&asset.metadata_blob);
            mutate_sidecar(&mut asset.sidecar, add_id);
            asset.sidecar.provenance_chain_hash = prior;
            asset.sidecar.signature = None;
            asset.sidecar.sign(&self.account.user_ik);
            let (blob, blob_key) = seal_metadata_blob(
                &album_amk,
                asset_id,
                &asset.sidecar.to_canonical_vec(),
                prior_nonce,
            )?;
            let hash = blob_ciphertext_hash(&blob);
            asset.metadata_blob = blob;
            sealed_blob_key = Some(blob_key);
            Some(hash)
        } else {
            None
        };

        // Sealing order (4): build + sign the manifest with `prior_provenance_hash = H` and the
        // `metadata_blob_hash` from (3); append it as the new chain head.
        let album = self.album(&album_id)?;
        let manifest = self.sign_lifecycle(
            album,
            &base,
            action,
            prior,
            retention_until,
            metadata_blob_hash,
        )?;
        {
            let asset = self
                .assets
                .get_mut(asset_id)
                .expect("asset_id was validated above");
            asset
                .chain
                .append(ProvenanceRecord {
                    asset_id: *asset_id,
                    manifest: manifest.clone(),
                    prior_provenance_hash: prior,
                })
                .map_err(|e| LifecycleError::Cbor(format!("chain: {e}")))?;
        }

        // Self-check the metadata↔manifest binding for a metadata-bearing write, enforcement on.
        if binds {
            let asset = &self.assets[asset_id];
            let binding = verify_metadata_binding(
                &manifest,
                &asset.metadata_blob,
                &sealed_blob_key.expect("the metadata-bearing branch set the blob key"),
                &asset.sidecar.to_canonical_vec(),
            );
            if binding != MetadataBinding::Bound {
                return Err(LifecycleError::MetadataUnbound(binding));
            }
        }

        // Re-borrow immutably to write the updated artifacts to disk.
        let asset = self
            .assets
            .get(asset_id)
            .expect("asset_id was validated above");
        let plaintext =
            fs::read(self.media_path(asset)).map_err(|e| LifecycleError::Io(e.to_string()))?;
        self.write_asset_files(asset, &plaintext)?;
        self.index_asset_row(asset)
    }

    /// Add a user tag (OR-set) and emit a `metadata-update` provenance record.
    pub fn tag_add(&mut self, asset_id: &Uuid, tag: &str) -> Result<()> {
        let tag = tag.to_string();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, add_id| {
            s.tags_user.add(tag, add_id);
        })
    }

    /// Set the caption (LWW register) and emit a `metadata-update` provenance record.
    pub fn set_caption(&mut self, asset_id: &Uuid, caption: &str) -> Result<()> {
        let caption = caption.to_string();
        let device = self.account.device.device_id;
        let ts = now_rfc3339();
        self.append_lifecycle(asset_id, Action::MetadataUpdate, None, move |s, _add_id| {
            s.caption.set(caption, ts, device);
        })
    }

    /// Soft-delete: emit a `delete` record carrying a signed retention window.
    pub fn soft_delete(&mut self, asset_id: &Uuid, retain_days: i64) -> Result<()> {
        // Timestamp arithmetic is absolute, so a retention "day" is exactly 24 h — the
        // correct semantic for a UTC retention window.
        let until = (Timestamp::now()
            + jiff::SignedDuration::from_hours(retain_days.saturating_mul(24)))
        .to_string();
        self.append_lifecycle(asset_id, Action::Delete, Some(until), |_, _| {})
    }

    /// Restore a soft-deleted asset: emit a `trash-restore` record.
    pub fn restore(&mut self, asset_id: &Uuid) -> Result<()> {
        self.append_lifecycle(asset_id, Action::TrashRestore, None, |_, _| {})
    }

    /// The current provenance head hash for each managed asset (for backup reconciliation).
    pub fn local_heads(&self) -> BTreeMap<Uuid, Hash32> {
        self.assets
            .iter()
            .filter_map(|(id, a)| a.chain.head().map(|h| (*id, h)))
            .collect()
    }

    /// Export every managed asset to a portable backup artifact.
    #[tracing::instrument(skip_all, fields(out = %out.display()))]
    pub fn export_backup(&self, out: &Path, passphrase: &[u8]) -> Result<()> {
        let mut assets = Vec::new();
        let mut amks: BTreeMap<(Uuid, u32), [u8; 32]> = BTreeMap::new();

        for asset in self.assets.values() {
            let album = self.album(&asset.album_id)?;
            let head = &asset
                .chain
                .records()
                .last()
                .expect("provenance chain is never empty")
                .manifest;
            let plaintext =
                fs::read(self.media_path(asset)).map_err(|e| LifecycleError::Io(e.to_string()))?;
            let epoch = head.core.amk_version.0;
            let file_key = self.file_key(album, epoch, &head.core.file_id, &head.core.nonce_prefix);
            let (_, ciphertext) = stream::encrypt_asset_vec_with_prefix(
                &file_key,
                head.core.nonce_prefix,
                &plaintext,
            );
            amks.insert((asset.album_id, epoch), album.amks[&epoch]);
            // The asset's custody-receipt log, if the client has persisted one beside the chain.
            let receipts = fs::read(crate::library::receipts_path(
                &self.root,
                &asset.asset_id,
                Some(asset.capture_utc),
            ))
            .unwrap_or_default();
            assets.push(BackupAsset {
                album_id: asset.album_id,
                asset_id: asset.asset_id,
                // Export the exact sealed blob the manifest committed to (re-sealing would draw
                // a fresh nonce and break the `metadata_blob_hash` content address).
                metadata_blob: asset.metadata_blob.clone(),
                ciphertext,
                provenance: asset.chain.records().to_vec(),
                receipts,
            });
        }

        let input = BackupInput {
            assets,
            amks,
            exporter_device: self.account.device.device_id,
            source_library_version: "1".into(),
            export_timestamp: now_rfc3339(),
        };
        let bytes = backup::export(&input, passphrase, self.device_signer.as_ref())?;
        fs::write(out, &bytes).map_err(|e| LifecycleError::Io(e.to_string()))?;
        tracing::info!(bytes = bytes.len(), "backup: export complete");
        Ok(())
    }

    /// This device's signing public key (the exporter key a peer verifies a backup against).
    pub fn exporter_verifying_key(&self) -> HybridVerifyingKey {
        self.device_signer.verifying_key()
    }

    /// Open a backup artifact and restore (commit) its assets into this workspace, writing
    /// decrypted plaintext + provenance into the library. `exporter_pub` is the exporting
    /// device's signing key (resolved from the user's device directory). Returns the count
    /// of assets added.
    #[tracing::instrument(skip_all, fields(archive = %archive.display()))]
    pub fn import_backup(
        &mut self,
        archive: &Path,
        passphrase: &[u8],
        exporter_pub: &HybridVerifyingKey,
    ) -> Result<usize> {
        let bytes = fs::read(archive).map_err(|e| LifecycleError::Io(e.to_string()))?;
        let artifact = BackupArtifact::open(&bytes, passphrase, exporter_pub)?;
        let report = artifact.restore(RestoreMode::Commit, &self.local_heads())?;

        let mut added = 0;
        for restored in &report.applied {
            // Rebuild on-disk artifacts for the restored asset.
            let head = &restored
                .provenance
                .last()
                .expect("restored provenance is never empty")
                .manifest;
            let capture_utc = Timestamp::now().as_second();
            let mut chain = ProvenanceChain::new();
            for rec in &restored.provenance {
                chain
                    .append(rec.clone())
                    .map_err(|e| LifecycleError::Cbor(format!("restore chain: {e}")))?;
            }
            // Decode the sidecar from the (decrypted) metadata blob if present.
            let sidecar = self.decode_restored_sidecar(restored, head)?;
            let ext = "bin".to_string();
            let asset = AssetState {
                asset_id: restored.asset_id,
                album_id: restored.album_id,
                ext,
                capture_utc,
                chain,
                sidecar,
                // The artifact preserves the exact sealed blob the manifest committed to.
                metadata_blob: restored.metadata_blob.clone(),
                stack: None,
            };
            self.write_asset_files(&asset, &restored.plaintext)?;
            self.index_asset_row(&asset)?;
            self.index_original_representation(&asset, restored.plaintext.len())?;
            self.assets.insert(restored.asset_id, asset);
            added += 1;
        }
        tracing::info!(added, "backup: import complete");
        Ok(added)
    }

    fn decode_restored_sidecar(
        &self,
        restored: &backup::artifact::RestoredAsset,
        head: &AssetManifest,
    ) -> Result<SidecarV1> {
        // Minimal sidecar reconstructed from the head manifest (the full encrypted metadata
        // blob is preserved verbatim in the artifact; decoding it requires the AMK, which we
        // hold). Here we synthesise a plaintext-equivalent sidecar for the local library.
        let mut sidecar = SidecarV1 {
            sidecar_schema: SIDECAR_SCHEMA_V1,
            crypto_suite_id: CRYPTO_SUITE_ID,
            uuid: restored.asset_id,
            hash: hash::hash_bytes(&restored.plaintext),
            capture_timestamp: head.core.timestamp.clone(),
            import_timestamp: now_rfc3339(),
            content_type: "application/octet-stream".into(),
            dimensions: None,
            lqip: None,
            tags_user: Default::default(),
            tags_ai: Default::default(),
            caption: Default::default(),
            rating: Default::default(),
            stack_membership: Lww::new(),
            cull: Lww::new(),
            hidden: Lww::new(),
            camera_id: None,
            device_id: head.core.created_by_device,
            session_id: Uuid::now_v7(),
            gps: None,
            // Mirror the head manifest's prior (the sealing invariant for its action); the true
            // sidecar plaintext lives in the preserved `metadata_blob`, decodable by an album
            // member that holds the AMK.
            provenance_chain_hash: head.core.prior_provenance_hash,
            unknown: BTreeMap::new(),
            signature: None,
        };
        sidecar.sign(&self.account.user_ik);
        Ok(sidecar)
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

    /// The issuer-held record for a share link, if this workspace issued it. Exposes the
    /// authoritative scope / expiry / revocation state the serving endpoint (S-C4) consults.
    pub fn share_link(&self, link: &ShareLinkId) -> Option<&ShareLinkRecord> {
        self.share_links.get(link)
    }

    /// Resolve the scope's decryption material to encapsulate into a link. Fails with
    /// [`SharingError::ScopeUnavailable`] if the scope is not managed by this workspace.
    fn scope_material(
        &self,
        scope: ShareScope,
    ) -> std::result::Result<ScopeMaterial, SharingError> {
        match scope {
            ShareScope::Asset(asset_id) => {
                let asset = self
                    .assets
                    .get(&asset_id)
                    .ok_or(SharingError::ScopeUnavailable)?;
                let album = self
                    .albums
                    .get(&asset.album_id)
                    .ok_or(SharingError::ScopeUnavailable)?;
                let head = &asset
                    .chain
                    .records()
                    .last()
                    .expect("provenance chain is never empty")
                    .manifest
                    .core;
                let epoch = head.amk_version.0;
                let amk = album
                    .amks
                    .get(&epoch)
                    .ok_or(SharingError::ScopeUnavailable)?;
                // Carry the folded file key: the recipient re-uses the manifest's
                // `nonce_prefix` to decrypt, so the grant must derive under that same prefix.
                let file_key = Amk::from_bytes(*amk).derive_file_key(&asset_id, &head.nonce_prefix);
                Ok(ScopeMaterial::Asset {
                    file_id: asset_id,
                    file_key,
                })
            }
            ShareScope::Album(album_id) => {
                let album = self
                    .albums
                    .get(&album_id)
                    .ok_or(SharingError::ScopeUnavailable)?;
                Ok(ScopeMaterial::Album {
                    album_id,
                    amks: album.amks.clone(),
                })
            }
        }
    }
}

/// Still-derivative generation for the signed import path (S-B1 → S-B2). Compiled only with the
/// `media` feature: decode the still, compute its LQIP, and generate + sign the thumbnail/preview
/// [`DerivativeManifest`](crate::crypto::provenance::DerivativeManifest)s through the injected
/// [`StillEncoder`](crate::media::image::derivative::StillEncoder).
#[cfg(feature = "media")]
impl Workspace {
    /// Decode a still into an in-memory pixel buffer, dispatching on extension. Unsupported or
    /// undecodable bytes yield `None` (the import proceeds signed-original-only).
    fn decode_still(
        &self,
        bytes: &[u8],
        ext: &str,
    ) -> Option<crate::media::image::buffer::ImageBuffer> {
        use crate::media::image::{Image, ImageDecode};
        match ext {
            "jpg" | "jpeg" => {
                crate::media::image::formats::jpeg::JpegImage::decode_from_bytes(bytes)
                    .ok()
                    .map(|img| img.get_buffer())
            }
            "png" => crate::media::image::formats::png::PngImage::decode_from_bytes(bytes)
                .ok()
                .map(|img| img.get_buffer()),
            _ => None,
        }
    }

    /// Compute the sidecar LQIP (chromahash + versioned fallback color) from a decoded buffer.
    fn lqip_from_buffer(
        buffer: &crate::media::image::buffer::ImageBuffer,
    ) -> Option<crate::sidecar::sidecar_v1::Lqip> {
        let rgba = buffer.to_rgba8().ok()?;
        let lqip = crate::media::image::lqip::LQIP::from_rgba_buffer(&rgba).ok()?;
        lqip.to_sidecar().ok()
    }

    /// Decode the still once and derive: pixel `dimensions`, the sidecar `lqip`, and the signed
    /// thumbnail/preview derivatives (empty when no encoder is attached). All are attached before
    /// the sidecar is sealed / after the manifest is signed, per the pipeline's Execute step.
    fn prepare_still(
        &self,
        plaintext: &[u8],
        ext: &str,
        exif: &crate::exif::extract::ExifExtract,
        asset_id: Uuid,
        album_id: Uuid,
    ) -> Result<(
        Option<Dimensions>,
        Option<crate::sidecar::sidecar_v1::Lqip>,
        Vec<GeneratedDerivative>,
    )> {
        let exif_dimensions = exif
            .width
            .zip(exif.height)
            .map(|(width, height)| Dimensions { width, height });

        let Some(buffer) = self.decode_still(plaintext, ext) else {
            // Undecodable / unsupported still: EXIF dimensions only, no LQIP or derivatives.
            return Ok((exif_dimensions, None, Vec::new()));
        };

        let dimensions = Some(Dimensions {
            width: buffer.width as u32,
            height: buffer.height as u32,
        });
        let lqip = Self::lqip_from_buffer(&buffer);

        let derivatives = match self.still_encoder.as_ref() {
            Some(encoder) => {
                let album = self.album(&album_id)?;
                let ctx = DerivativeContext {
                    source_asset_id: asset_id,
                    crypto_suite_id: CRYPTO_SUITE_ID,
                    protocol_version: PROTOCOL_VERSION.into(),
                    amk_version: AmkVersion(album.current_epoch),
                    generated_by_device: self.account.device.device_id,
                    generated_by_client: concat!("capsule-core/", env!("CARGO_PKG_VERSION")).into(),
                    generated_at: now_rfc3339(),
                    device_signer: self.device_signer.as_ref(),
                    write_tier_signer: &album.write_tier,
                };
                generate_still_derivatives(
                    &buffer,
                    plaintext,
                    &[DerivativeTier::Thumbnail, DerivativeTier::Preview],
                    encoder.as_ref(),
                    &ctx,
                )
                .map_err(|e| LifecycleError::Io(format!("derivative generation: {e}")))?
            }
            None => Vec::new(),
        };
        Ok((dimensions, lqip, derivatives))
    }

    /// Write the generated derivative bytes + their signed manifest bundle under the asset's
    /// media directory (`derivatives/{uuid}.{role}.{ext}` and `{uuid}.derivatives.cbor`).
    fn persist_derivatives(
        &self,
        asset: &AssetState,
        derivatives: &[GeneratedDerivative],
    ) -> Result<()> {
        if derivatives.is_empty() {
            return Ok(());
        }
        let dir = media_dir(&self.root, asset.capture_utc).join("derivatives");
        fs::create_dir_all(&dir).map_err(|e| LifecycleError::Io(e.to_string()))?;
        let stem = asset.asset_id.simple();

        let mut manifests = Vec::with_capacity(derivatives.len());
        for d in derivatives {
            let format_ext = match d.format {
                DerivativeFormat::Jxl => "jxl",
                DerivativeFormat::Avif => "avif",
                DerivativeFormat::WebP => "webp",
                DerivativeFormat::Original => asset.ext.as_str(),
            };
            let role = match d.tier {
                DerivativeTier::Thumbnail => "thumbnail",
                DerivativeTier::Preview => "preview",
            };
            fs::write(dir.join(format!("{stem}.{role}.{format_ext}")), &d.bytes)
                .map_err(|e| LifecycleError::Io(e.to_string()))?;
            manifests.push(d.manifest.clone());
        }
        let bundle =
            cbor::to_canonical_vec(&manifests).map_err(|e| LifecycleError::Cbor(e.to_string()))?;
        fs::write(dir.join(format!("{stem}.derivatives.cbor")), bundle)
            .map_err(|e| LifecycleError::Io(e.to_string()))?;
        Ok(())
    }
}

impl ShareLinkIssuer for Workspace {
    /// Issue a view-only share link: resolve the scope's decryption material, encapsulate
    /// it around a fresh CSPRNG link secret (and, if a passphrase is given, an Argon2id
    /// layer on top), draw a 128-bit opaque id, and record the link for revocation. The
    /// fragment secret is returned to the caller and never persisted server-side.
    fn create_link(
        &mut self,
        scope: ShareScope,
        expires_at: Option<String>,
        passphrase: Option<&str>,
    ) -> std::result::Result<ShareLink, SharingError> {
        let material = self.scope_material(scope)?;
        let fragment_secret = crate::crypto::rng::random_array::<LINK_SECRET_LEN>();
        let opaque_id = generate_opaque_id();
        let wrapped_scope = encapsulate_scope(
            &material,
            &fragment_secret,
            &opaque_id,
            passphrase,
            self.argon2_params,
        )?;
        let link_id = ShareLinkId(Uuid::now_v7());
        let created_at = now_rfc3339();
        self.share_links.insert(
            link_id,
            ShareLinkRecord {
                link_id,
                opaque_id,
                scope,
                wrapped_scope: wrapped_scope.clone(),
                expires_at: expires_at.clone(),
                created_at,
                revocation: None,
            },
        );
        Ok(ShareLink {
            link_id,
            opaque_id,
            scope,
            fragment_secret,
            wrapped_scope,
            expires_at,
        })
    }

    /// Publish a revocation record for `link`; the serve path then refuses it. Revoking an
    /// unknown link is [`SharingError::NotFound`]; revoking an already-revoked link is
    /// idempotent (the original revocation timestamp is kept).
    fn revoke_link(&mut self, link: ShareLinkId) -> std::result::Result<(), SharingError> {
        let record = self
            .share_links
            .get_mut(&link)
            .ok_or(SharingError::NotFound)?;
        if record.revocation.is_none() {
            record.revocation = Some(RevocationRecord {
                link_id: link,
                revoked_at: now_rfc3339(),
            });
        }
        Ok(())
    }
}

impl Workspace {
    /// The public (fragment) half of an issued upload link, if this workspace issued it.
    /// Exposed for tests and clients rebuilding the `.../u/{opaque-id}#{drop_pubkey}` URL.
    pub fn upload_link_pubkey(&self, link: &UploadLinkId) -> Option<&[u8]> {
        self.upload_links
            .get(link)
            .map(|l| l.drop_pubkey.as_slice())
    }

    /// The opaque URL-path id of an issued upload link (the `.../u/{opaque-id}` component).
    pub fn upload_link_opaque_id(&self, link: &UploadLinkId) -> Option<[u8; 16]> {
        self.upload_links.get(link).map(|l| l.opaque_id)
    }

    /// The optional Argon2id write-abuse verifier of an issued link — the record the serve
    /// path (S-C5) consults to gate a drop-session on passphrase possession.
    pub fn upload_link_passphrase(&self, link: &UploadLinkId) -> Option<&PassphraseVerifier> {
        self.upload_links
            .get(link)
            .and_then(|l| l.passphrase.as_ref())
    }

    /// RFC 3339 issuance time of an issued upload link.
    pub fn upload_link_created_at(&self, link: &UploadLinkId) -> Option<&str> {
        self.upload_links.get(link).map(|l| l.created_at.as_str())
    }

    /// Whether an issued upload link is still live at `now` (not revoked, not past expiry).
    pub fn upload_link_is_live(&self, link: &UploadLinkId, now: jiff::Timestamp) -> bool {
        self.upload_links.get(link).is_some_and(|l| {
            l.revoked_at.is_none()
                && l.caps
                    .expires_at
                    .as_ref()
                    .is_none_or(|ts| ts.parse::<jiff::Timestamp>().is_ok_and(|exp| now < exp))
        })
    }

    /// Stage a guest's [`SealedDrop`] into this owner's inbox, modeling the server's stage
    /// step (S-C5) so the offline core can drive the full seal → stage → adopt path. Refuses
    /// a drop through an unknown or revoked link (a fail-closed stand-in for the serve
    /// path's cap/quota checks). Returns the new inbox drop id.
    pub fn receive_drop(
        &mut self,
        via_link: UploadLinkId,
        sealed: SealedDrop,
    ) -> std::result::Result<DropId, DropError> {
        let link = self
            .upload_links
            .get(&via_link)
            .ok_or(DropError::NotFound)?;
        if link.revoked_at.is_some() {
            return Err(DropError::LinkRefused("link revoked"));
        }
        let drop_id = DropId(Uuid::now_v7());
        self.inbox.insert(
            drop_id,
            InboxEntry {
                descriptor: sealed.descriptor,
                ciphertext: sealed.ciphertext,
                via_link,
                received_at: now_rfc3339(),
            },
        );
        Ok(drop_id)
    }

    /// Recover the Drop Key keypair for an issued link by unsealing its escrowed seed.
    fn drop_keypair(&self, link: &UploadLinkId) -> std::result::Result<DekKeypair, DropError> {
        let issued = self.upload_links.get(link).ok_or(DropError::NotFound)?;
        let seed = self
            .account
            .master
            .open(&issued.escrowed_drop_seed)
            .map_err(|_| DropError::Crypto("drop key escrow unseal failed"))?;
        let seed: [u8; 32] = seed
            .as_slice()
            .try_into()
            .map_err(|_| DropError::Crypto("drop key seed wrong length"))?;
        Ok(DekKeypair::from_seed(&seed))
    }
}

impl crate::drop::UploadLinkIssuer for Workspace {
    /// Provision an upload link: mint a fresh [Drop Key](crate::crypto::keys::DekKeypair)
    /// (X-Wing KEM), escrow its private half under the account master key (the OGK re-wrap
    /// that lets *any* enrolled device decapsulate rides the multi-device escrow seam),
    /// draw a 128-bit opaque id, derive the optional Argon2id write-abuse verifier, and
    /// record the link for revocation. The public half is returned for the URL fragment and
    /// never persisted server-side. SSoT: [Web Upload].
    ///
    /// [Web Upload]: https://docs/design/web-upload/
    fn create_link(
        &mut self,
        caps: LinkCaps,
        passphrase: Option<&str>,
    ) -> std::result::Result<UploadLink, DropError> {
        let drop_key = DekKeypair::generate();
        let drop_pubkey = drop_key.public_bytes();
        let escrowed_drop_seed = self.account.master.seal(&drop_key.to_seed_bytes());
        let opaque_id = generate_drop_id();
        let passphrase = passphrase
            .map(|pw| PassphraseVerifier::derive(pw, self.argon2_params))
            .transpose()?;
        let link_id = UploadLinkId(Uuid::now_v7());
        self.upload_links.insert(
            link_id,
            IssuedLink {
                opaque_id,
                caps: caps.clone(),
                drop_pubkey: drop_pubkey.clone(),
                escrowed_drop_seed,
                passphrase,
                created_at: now_rfc3339(),
                revoked_at: None,
            },
        );
        Ok(UploadLink {
            link_id,
            opaque_id,
            drop_pubkey,
            caps,
        })
    }

    /// Revoke a link; the serve path refuses it within its fail-closed cache window.
    /// Revoking an unknown link is [`DropError::NotFound`]; re-revocation keeps the original
    /// timestamp (idempotent).
    fn revoke_link(&mut self, link: UploadLinkId) -> std::result::Result<(), DropError> {
        let issued = self
            .upload_links
            .get_mut(&link)
            .ok_or(DropError::NotFound)?;
        if issued.revoked_at.is_none() {
            issued.revoked_at = Some(now_rfc3339());
        }
        Ok(())
    }
}

impl crate::drop::DropAdopter for Workspace {
    /// This owner's pending drops, newest-arrival order unspecified.
    fn list_inbox(&self) -> std::result::Result<Vec<PendingDrop>, DropError> {
        Ok(self
            .inbox
            .iter()
            .map(|(drop_id, e)| PendingDrop {
                drop_id: *drop_id,
                descriptor: e.descriptor.clone(),
                via_link: e.via_link,
                received_at: e.received_at.clone(),
            })
            .collect())
    }

    /// Adopt a pending drop into `album_id` **in place** (no byte re-upload): decapsulate the
    /// guest-chosen `K`, rewrap it under the album AMK (`asset-keywrap/v1`), author + sign a
    /// `create` manifest with `key_mode = wrapped` whose `ciphertext_hash` references the
    /// staged blob, self-verify through [`verify_asset`], and drop the inbox row. Returns the
    /// signed adopting manifest. SSoT: [Web Upload] — Review and adopt-in-place.
    ///
    /// [Web Upload]: https://docs/design/web-upload/
    fn adopt(
        &mut self,
        drop: DropId,
        album_id: Uuid,
    ) -> std::result::Result<AssetManifest, DropError> {
        let entry = self.inbox.get(&drop).ok_or(DropError::NotFound)?;
        let descriptor = entry.descriptor.clone();
        let ciphertext = entry.ciphertext.clone();
        let via_link = entry.via_link;

        // Decapsulate the guest-chosen K with the link's escrowed Drop Key private half.
        let drop_key = self.drop_keypair(&via_link)?;
        let file_key = open_drop_key(&drop_key, &descriptor.kem_ct)?;

        let album = self
            .album(&album_id)
            .map_err(|_| DropError::LinkRefused("destination album not found"))?;
        let epoch = album.current_epoch;
        let amk = Amk::from_bytes(album.amks[&epoch]);
        let file_id = Uuid::now_v7();

        // The adopter can decrypt the staged bytes (it now holds K) to author the sidecar's
        // plaintext hash and preview; the bulk ciphertext itself never moves.
        let plaintext = stream::decrypt_asset_vec(&file_key, &descriptor.nonce_prefix, &ciphertext)
            .map_err(|_| DropError::Crypto("staged drop ciphertext failed to decrypt"))?;

        // Author + sign the sidecar. `created_by`/author is the adopter — the guest is never
        // a signer; its origin is an unverified, self-asserted descriptive note.
        let mut sidecar = SidecarV1 {
            sidecar_schema: SIDECAR_SCHEMA_V1,
            crypto_suite_id: CRYPTO_SUITE_ID,
            uuid: file_id,
            hash: hash::hash_bytes(&plaintext),
            capture_timestamp: now_rfc3339(),
            import_timestamp: now_rfc3339(),
            content_type: descriptor.content_type.clone(),
            dimensions: None,
            lqip: None,
            tags_user: Default::default(),
            tags_ai: Default::default(),
            caption: Default::default(),
            rating: Default::default(),
            stack_membership: Lww::new(),
            cull: Lww::new(),
            hidden: Lww::new(),
            camera_id: None,
            device_id: self.account.device.device_id,
            session_id: Uuid::now_v7(),
            gps: None,
            provenance_chain_hash: None,
            unknown: BTreeMap::new(),
            signature: None,
        };
        sidecar.sign(&self.account.user_ik);

        // Seal the sidecar into the metadata blob (fresh nonce folded into the blob key; the
        // adoption is a create, so there is no prior nonce to refuse).
        let (metadata_blob, blob_key) =
            seal_metadata_blob(&amk, &file_id, &sidecar.to_canonical_vec(), None)
                .map_err(|_| DropError::Crypto("adopted metadata blob seal failed"))?;
        let metadata_blob_hash = blob_ciphertext_hash(&metadata_blob);

        // Rewrap K under the album AMK (`asset-keywrap/v1`) — K is *carried* wrapped, not
        // derived, because an external party chose it.
        let wrapped_file_key = seal_file_key(&amk, &file_id, &file_key);

        let core = ManifestCore {
            version: ASSET_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: PROTOCOL_VERSION.into(),
            file_id,
            album_id,
            amk_version: AmkVersion(epoch),
            ciphertext_hash: descriptor.ciphertext_hash,
            plaintext_size: descriptor.plaintext_size,
            chunk_size: descriptor.chunk_size,
            nonce_prefix: descriptor.nonce_prefix,
            key_mode: KeyMode::Wrapped,
            wrapped_file_key: Some(WrappedFileKey(wrapped_file_key)),
            metadata_blob_hash: Some(metadata_blob_hash),
            created_by_user: self.account.user_id,
            created_by_device: self.account.device.device_id,
            client_version: concat!("capsule-core/", env!("CARGO_PKG_VERSION")).into(),
            timestamp: now_rfc3339(),
            action: Action::Create,
            prior_provenance_hash: None,
            retention_until: None,
        };
        let manifest = core
            .sign(self.device_signer.as_ref(), &album.write_tier)
            .map_err(|_| DropError::Crypto("adopting manifest signing failed"))?;

        // Self-verify through the one chokepoint against the unchanged staged ciphertext, and
        // confirm the sealed metadata blob binds to the signed sidecar, before committing.
        let authority = &self.authorities[&album_id];
        if verify_asset(&manifest, &ciphertext, &self.directory, authority, None)
            != VerifyOutcome::Accept
        {
            return Err(DropError::Crypto(
                "adopting manifest failed self-verification",
            ));
        }
        if verify_metadata_binding(
            &manifest,
            &metadata_blob,
            &blob_key,
            &sidecar.to_canonical_vec(),
        ) != MetadataBinding::Bound
        {
            return Err(DropError::Crypto(
                "adopted metadata blob failed its binding",
            ));
        }

        // The drop is now a library asset — remove the inbox row (atomic on the server side).
        self.inbox.remove(&drop);
        Ok(manifest)
    }

    /// Discard a pending drop the owner rejects; its bytes are GC'd and the quota freed.
    fn discard(&mut self, drop: DropId) -> std::result::Result<(), DropError> {
        self.inbox.remove(&drop).ok_or(DropError::NotFound)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::crypto::primitives::Argon2Params;

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

    /// S-A6: the full guest-drop path through the `Workspace` — issue an upload link, seal a
    /// drop to its Drop Key (the WASM/browser half), stage it, and adopt it into an album.
    /// The adopting `create` manifest is `key_mode = wrapped`, self-verifies, and a second
    /// member (holding the same AMK) unwraps `wrapped_file_key` and decrypts the unchanged
    /// ciphertext. Also covers revocation, the passphrase verifier, and discard.
    #[test]
    fn upload_link_seal_stage_and_adopt() {
        use crate::crypto::encryption::keywrap::unseal_file_key;
        use crate::drop::{DropAdopter, UploadLinkIssuer, seal_drop};

        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Guest contributions");

        // Provision an upload link with a passphrase abuse gate.
        let caps = LinkCaps {
            max_file_count: Some(10),
            single_use: false,
            ..Default::default()
        };
        let link = UploadLinkIssuer::create_link(&mut ws, caps, Some("open sesame")).unwrap();
        assert_eq!(link.drop_pubkey.len(), crate::crypto::keys::DEK_PUBLIC_LEN);
        assert_eq!(
            ws.upload_link_opaque_id(&link.link_id).unwrap(),
            link.opaque_id
        );
        assert!(ws.upload_link_created_at(&link.link_id).is_some());
        assert!(
            ws.upload_link_passphrase(&link.link_id)
                .unwrap()
                .verify("open sesame")
        );
        assert!(ws.upload_link_is_live(&link.link_id, Timestamp::now()));

        // A guest seals an asset to the link's Drop Key public half (no account, no album key).
        let plaintext = b"\xFF\xD8\xFF a guest-uploaded photo, sealed in the browser";
        let sealed = seal_drop(plaintext, &link.drop_pubkey, "image/jpeg").unwrap();
        let ciphertext = sealed.ciphertext.clone();

        // The server stages it into the owner's inbox; the owner sees it awaiting review.
        let drop_id = ws.receive_drop(link.link_id, sealed).unwrap();
        assert_eq!(DropAdopter::list_inbox(&ws).unwrap().len(), 1);

        // The owner adopts it into the album on their trusted device.
        let manifest = DropAdopter::adopt(&mut ws, drop_id, album).unwrap();
        assert_eq!(manifest.core.key_mode, KeyMode::Wrapped);
        assert!(manifest.core.wrapped_file_key.is_some());
        assert_eq!(manifest.core.album_id, album);
        // The bytes stayed put: the manifest references the drop's ciphertext hash unchanged.
        assert_eq!(manifest.core.ciphertext_hash, hash::hash_bytes(&ciphertext));
        // Adoption emptied the inbox.
        assert!(DropAdopter::list_inbox(&ws).unwrap().is_empty());

        // A second album member unwraps K under the AMK and decrypts the unchanged ciphertext.
        let epoch = manifest.core.amk_version.0;
        let amk = Amk::from_bytes(ws.album(&album).unwrap().amks[&epoch]);
        let k = unseal_file_key(
            &amk,
            &manifest.core.file_id,
            &manifest.core.wrapped_file_key.as_ref().unwrap().0,
        )
        .unwrap();
        assert_eq!(
            stream::decrypt_asset_vec(&k, &manifest.core.nonce_prefix, &ciphertext).unwrap(),
            plaintext,
        );

        // Revocation makes the link non-live and refuses further drops.
        UploadLinkIssuer::revoke_link(&mut ws, link.link_id).unwrap();
        assert!(!ws.upload_link_is_live(&link.link_id, Timestamp::now()));
        let sealed2 = seal_drop(b"late drop", &link.drop_pubkey, "image/jpeg").unwrap();
        assert!(matches!(
            ws.receive_drop(link.link_id, sealed2),
            Err(DropError::LinkRefused(_))
        ));
        // Revoking an unknown link is NotFound; re-revocation is idempotent.
        assert!(matches!(
            UploadLinkIssuer::revoke_link(&mut ws, UploadLinkId(Uuid::now_v7())),
            Err(DropError::NotFound)
        ));
        UploadLinkIssuer::revoke_link(&mut ws, link.link_id).unwrap();
    }

    /// S-A6: a rejected drop is discarded — removed from the inbox with no library trace.
    #[test]
    fn discarded_drop_leaves_no_trace() {
        use crate::drop::{DropAdopter, UploadLinkIssuer, seal_drop};

        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let link = UploadLinkIssuer::create_link(&mut ws, LinkCaps::default(), None).unwrap();
        let sealed = seal_drop(b"unwanted", &link.drop_pubkey, "image/jpeg").unwrap();
        let drop_id = ws.receive_drop(link.link_id, sealed).unwrap();

        assert_eq!(DropAdopter::list_inbox(&ws).unwrap().len(), 1);
        DropAdopter::discard(&mut ws, drop_id).unwrap();
        assert!(DropAdopter::list_inbox(&ws).unwrap().is_empty());
        // Discarding again (or an unknown drop) is NotFound.
        assert!(matches!(
            DropAdopter::discard(&mut ws, drop_id),
            Err(DropError::NotFound)
        ));
    }

    /// S-A3: the `Workspace` populates `metadata_blob_hash` per the sealing order, the sidecar
    /// binds to the manifest through the prior head, and a one-byte sidecar mutation quarantines.
    #[test]
    fn metadata_binding_populated_and_enforced() {
        use crate::crypto::verify_asset::{
            BindingReject, MetadataBinding, verify_metadata_binding,
        };

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(&img, b"\xFF\xD8\xFF metadata-binding bytes").unwrap();

        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip");
        let asset = ws.import_asset(album, &img).unwrap();

        // The create manifest commits to a metadata blob; its sidecar references no prior head,
        // and that absence equals the manifest's `prior_provenance_hash` (both `None` on create).
        let st = ws.asset(&asset).unwrap();
        let head = &st.chain.records().last().unwrap().manifest;
        let epoch = head.core.amk_version.0;
        assert!(
            head.core.metadata_blob_hash.is_some(),
            "create must bind a metadata blob"
        );
        assert_eq!(st.sidecar.provenance_chain_hash, None);
        assert_eq!(
            head.core.prior_provenance_hash,
            st.sidecar.provenance_chain_hash
        );

        // The stored blob round-trips to the signed sidecar under the asset's blob key,
        // re-derived from the blob's own (folded) nonce.
        let blob_key = Amk::from_bytes(ws.album(&album).unwrap().amks[&epoch])
            .derive_blob_key(&asset, &blob_nonce(&st.metadata_blob).unwrap());
        assert_eq!(
            verify_metadata_binding(
                head,
                &st.metadata_blob,
                &blob_key,
                &st.sidecar.to_canonical_vec()
            ),
            MetadataBinding::Bound
        );
        // A one-byte mutation of the local sidecar quarantines (surfaced, never persisted).
        let mut tampered = st.sidecar.to_canonical_vec();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        assert_eq!(
            verify_metadata_binding(head, &st.metadata_blob, &blob_key, &tampered),
            MetadataBinding::Quarantine(BindingReject::SidecarMismatch)
        );

        // A metadata-update re-binds: the sidecar references the PRIOR head (the create record),
        // equal to the update manifest's `prior_provenance_hash`.
        let create_head = ws.asset(&asset).unwrap().chain.records()[0].record_hash();
        ws.tag_add(&asset, "vacation").unwrap();
        let st = ws.asset(&asset).unwrap();
        let update = &st.chain.records().last().unwrap().manifest;
        assert!(update.core.metadata_blob_hash.is_some());
        assert_eq!(st.sidecar.provenance_chain_hash, Some(create_head));
        assert_eq!(
            update.core.prior_provenance_hash,
            st.sidecar.provenance_chain_hash
        );
        // The re-seal drew a fresh nonce folded into a new blob key — re-derive from the
        // updated blob's own nonce (the create-era `blob_key` no longer opens it).
        let update_blob_key = Amk::from_bytes(ws.album(&album).unwrap().amks[&epoch])
            .derive_blob_key(&asset, &blob_nonce(&st.metadata_blob).unwrap());
        assert_ne!(
            update_blob_key, blob_key,
            "the metadata-update re-rolled the blob key"
        );
        assert_eq!(
            verify_metadata_binding(
                update,
                &st.metadata_blob,
                &update_blob_key,
                &st.sidecar.to_canonical_vec()
            ),
            MetadataBinding::Bound
        );

        // A delete mints no metadata blob: the head manifest commits to none, and that is
        // structurally valid under the presence-by-action rule.
        ws.soft_delete(&asset, 30).unwrap();
        let st = ws.asset(&asset).unwrap();
        let del = &st.chain.records().last().unwrap().manifest;
        assert!(
            del.core.metadata_blob_hash.is_none(),
            "delete binds no metadata blob"
        );
        assert!(del.structural_ok());
    }

    #[test]
    fn end_to_end_data_plane() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(
            &img,
            b"\xFF\xD8\xFF\xE0 fake jpeg bytes for the e2e test \x00\x01\x02",
        )
        .unwrap();

        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip");

        // Import → encrypt → manifest+provenance+signed sidecar → verify_asset(Accept).
        let asset = ws.import_asset(album, &img).unwrap();
        assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);

        // The signed sidecar + provenance + plaintext exist on disk.
        let st = ws.asset(&asset).unwrap();
        assert!(ws.media_path(st).exists());
        assert!(ws.sidecar_path(st).exists());
        assert!(ws.provenance_path(st).exists());
        assert!(st.sidecar.verify(&ws.account.user_ik.verifying_key()));

        // CRDT metadata edits advance the chain and re-sign the sidecar.
        ws.tag_add(&asset, "vacation").unwrap();
        ws.set_caption(&asset, "sunset over the bay").unwrap();
        let st = ws.asset(&asset).unwrap();
        assert!(st.sidecar.tags_user.value().contains("vacation"));
        assert_eq!(st.sidecar.caption.get().unwrap(), "sunset over the bay");
        assert_eq!(st.chain.records().len(), 3); // create + 2 metadata-update
        ProvenanceChain::verify_walk(st.chain.records()).unwrap();

        // Soft delete + restore append lifecycle records.
        ws.soft_delete(&asset, 30).unwrap();
        ws.restore(&asset).unwrap();
        let st = ws.asset(&asset).unwrap();
        assert_eq!(st.chain.records().len(), 5);
        // The delete record carries a retention window; it remains in the chain after restore.
        let actions: Vec<_> = st
            .chain
            .records()
            .iter()
            .map(|r| r.manifest.core.action)
            .collect();
        assert_eq!(
            actions,
            vec![
                Action::Create,
                Action::MetadataUpdate,
                Action::MetadataUpdate,
                Action::Delete,
                Action::TrashRestore
            ]
        );

        // Backup → restore into a FRESH library (new device, verifying against the
        // exporter's published key) → byte-equal plaintext.
        let backup_path = src.path().join("backup.tar");
        ws.export_backup(&backup_path, b"recovery-pass").unwrap();
        let exporter_pub = ws.exporter_verifying_key();

        let fresh = TempDir::new().unwrap();
        let mut ws2 = fast_workspace(fresh.path());
        let added = ws2
            .import_backup(&backup_path, b"recovery-pass", &exporter_pub)
            .unwrap();
        assert_eq!(added, 1);
        assert_eq!(
            ws2.read_plaintext(&asset).unwrap(),
            ws.read_plaintext(&asset).unwrap(),
            "restored library must be byte-equal to the source"
        );

        // A wrong exporter key (untrusted device) is refused.
        let imposter = HybridSigningKey::generate().verifying_key();
        let mut ws3 = fast_workspace(TempDir::new().unwrap().path());
        assert!(
            ws3.import_backup(&backup_path, b"recovery-pass", &imposter)
                .is_err()
        );
    }

    #[test]
    fn epoch_rotation_keeps_old_assets_verifiable_and_backs_up() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let a = src.path().join("a.jpg");
        let b = src.path().join("b.jpg");
        fs::write(&a, b"\xFF\xD8\xFF first photo, written at epoch 1").unwrap();
        fs::write(&b, b"\xFF\xD8\xFF second photo, written at epoch 2").unwrap();

        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip");

        // Import at epoch 1, rotate the album, import at epoch 2.
        let id_a = ws.import_asset(album, &a).unwrap();
        assert_eq!(ws.rotate_epoch(album).unwrap(), 2);
        let id_b = ws.import_asset(album, &b).unwrap();

        // Each asset recorded the epoch it was written under...
        let epoch_of = |ws: &Workspace, id| {
            ws.asset(id).unwrap().chain.records()[0]
                .manifest
                .core
                .amk_version
        };
        assert_eq!(epoch_of(&ws, &id_a), AmkVersion(1));
        assert_eq!(epoch_of(&ws, &id_b), AmkVersion(2));
        // ...and BOTH still verify — the pre-rotation asset under its original epoch key (the
        // regression guard for the `current_epoch` file-key bug).
        assert_eq!(ws.verify(&id_a).unwrap(), VerifyOutcome::Accept);
        assert_eq!(ws.verify(&id_b).unwrap(), VerifyOutcome::Accept);

        // A cross-epoch backup escrows each asset's own-epoch AMK; restore into a fresh library
        // is byte-equal for both (guards the export file-key / blob-key / escrow-value epochs).
        let backup_path = src.path().join("backup.tar");
        ws.export_backup(&backup_path, b"recovery-pass").unwrap();
        let exporter_pub = ws.exporter_verifying_key();

        let fresh = TempDir::new().unwrap();
        let mut ws2 = fast_workspace(fresh.path());
        let added = ws2
            .import_backup(&backup_path, b"recovery-pass", &exporter_pub)
            .unwrap();
        assert_eq!(added, 2);
        assert_eq!(
            ws2.read_plaintext(&id_a).unwrap(),
            ws.read_plaintext(&id_a).unwrap()
        );
        assert_eq!(
            ws2.read_plaintext(&id_b).unwrap(),
            ws.read_plaintext(&id_b).unwrap()
        );
    }

    #[test]
    fn crypto_lifecycle_writes_through_to_the_index() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(&img, b"\xFF\xD8\xFF indexed photo").unwrap();

        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip");
        let id = ws.import_asset(album, &img).unwrap();
        let uuid = id.to_string();

        // The import is queryable in the timeline, tagged to its album.
        let timeline = ws.db().query_timeline(0, 100).unwrap();
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].uuid, uuid);
        assert_eq!(
            timeline[0].album_id.as_deref(),
            Some(album.to_string().as_str())
        );

        // It recorded a device-owned `original` representation, exempt from eviction.
        let reps = ws.db().representations_for(&uuid).unwrap();
        assert_eq!(reps.len(), 1);
        assert_eq!(reps[0].tier, "original");
        assert!(reps[0].is_owned_original);
        assert!(
            ws.db().eviction_candidates(0).unwrap().is_empty(),
            "an owned original is never an eviction candidate"
        );

        // A tag edit re-syncs into the index.
        ws.tag_add(&id, "vacation").unwrap();
        assert_eq!(
            ws.db().tags_for(&uuid).unwrap(),
            vec!["vacation".to_string()]
        );

        // Soft-delete hides it from the timeline; restore brings it back (deletion state is
        // derived from the provenance chain).
        ws.soft_delete(&id, 30).unwrap();
        assert!(ws.db().query_timeline(0, 100).unwrap().is_empty());
        ws.restore(&id).unwrap();
        assert_eq!(ws.db().query_timeline(0, 100).unwrap().len(), 1);
    }

    fn imported_asset(ws: &mut Workspace, bytes: &[u8]) -> (Uuid, Uuid) {
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(&img, bytes).unwrap();
        let album = ws.create_album("Shared");
        let asset = ws.import_asset(album, &img).unwrap();
        // Keep the tempdir alive only until import copies the bytes in; return ids.
        (album, asset)
    }

    /// The encapsulated scope key genuinely grants the recipient decryption: a client with
    /// only the fragment secret + opaque id opens the scope and recovers the exact file key
    /// the asset was encrypted under, and decrypts the ciphertext back to the plaintext.
    #[test]
    fn share_link_asset_scope_grants_decryption() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let plaintext = b"\xFF\xD8\xFF shared-asset plaintext bytes";
        let (album_id, asset_id) = imported_asset(&mut ws, plaintext);

        let link = ws
            .create_link(ShareScope::Asset(asset_id), None, None)
            .unwrap();
        // Opaque id is a full 128 bits; the fragment secret is ≥128 bits; neither is empty.
        assert_eq!(link.opaque_id.len(), 16);
        assert_eq!(link.fragment_secret.len(), 32);
        assert!(!link.is_passphrase_protected());

        // Recipient side: open with only the URL material.
        let material = crate::sharing::open_scope(
            &link.wrapped_scope,
            &link.opaque_id,
            &link.fragment_secret,
            None,
        )
        .unwrap();

        let head = ws
            .asset(&asset_id)
            .unwrap()
            .chain
            .records()
            .last()
            .unwrap()
            .manifest
            .core
            .clone();
        let epoch = head.amk_version.0;
        let recovered = material
            .file_key_for(&asset_id, epoch, &head.nonce_prefix)
            .unwrap();

        // The recovered key equals the real per-file key ...
        let expected = ws.file_key(
            ws.album(&album_id).unwrap(),
            epoch,
            &asset_id,
            &head.nonce_prefix,
        );
        assert_eq!(recovered, expected, "recipient recovers the true file key");
        // ... and it actually decrypts the asset ciphertext back to plaintext.
        let (_, ciphertext) =
            stream::encrypt_asset_vec_with_prefix(&expected, head.nonce_prefix, plaintext);
        let back = stream::decrypt_asset_vec(&recovered, &head.nonce_prefix, &ciphertext).unwrap();
        assert_eq!(back, plaintext);
    }

    /// An album-scoped link with a passphrase: the material is passphrase-wrapped, unwraps
    /// client-side, and covers the album's epoch AMKs; the wrong passphrase is rejected.
    #[test]
    fn share_link_album_scope_with_passphrase() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let (album_id, asset_id) = imported_asset(&mut ws, b"\xFF\xD8\xFF album share");
        // Rotate so the album spans two epochs; the album grant must cover both.
        ws.rotate_epoch(album_id).unwrap();

        let link = ws
            .create_link(ShareScope::Album(album_id), None, Some("hunter2"))
            .unwrap();
        assert!(link.is_passphrase_protected());

        // Wrong passphrase → refused; no passphrase → refused; correct → material.
        assert_eq!(
            crate::sharing::open_scope(
                &link.wrapped_scope,
                &link.opaque_id,
                &link.fragment_secret,
                Some("nope")
            ),
            Err(SharingError::WrongPassphrase),
        );
        assert_eq!(
            crate::sharing::open_scope(
                &link.wrapped_scope,
                &link.opaque_id,
                &link.fragment_secret,
                None
            ),
            Err(SharingError::PassphraseRequired),
        );
        let material = crate::sharing::open_scope(
            &link.wrapped_scope,
            &link.opaque_id,
            &link.fragment_secret,
            Some("hunter2"),
        )
        .unwrap();

        // The album grant derives the asset's file key under its written epoch.
        let core = ws.asset(&asset_id).unwrap().chain.records()[0]
            .manifest
            .core
            .clone();
        let epoch = core.amk_version.0;
        assert_eq!(
            material
                .file_key_for(&asset_id, epoch, &core.nonce_prefix)
                .unwrap(),
            ws.file_key(
                ws.album(&album_id).unwrap(),
                epoch,
                &asset_id,
                &core.nonce_prefix
            ),
        );
    }

    /// Revocation & expiry: the issuer's record drives the serve-path liveness predicate.
    #[test]
    fn share_link_revocation_and_expiry() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let (album_id, _) = imported_asset(&mut ws, b"\xFF\xD8\xFF revoke me");
        let now = Timestamp::now();

        // A future-expiry link is live; a past-expiry link is not (fail-closed on expiry).
        let future = (now + jiff::SignedDuration::from_hours(24)).to_string();
        let live = ws
            .create_link(ShareScope::Album(album_id), Some(future), None)
            .unwrap();
        assert!(ws.share_link(&live.link_id).unwrap().is_live_at(now));

        let past = (now - jiff::SignedDuration::from_hours(24)).to_string();
        let expired = ws
            .create_link(ShareScope::Album(album_id), Some(past), None)
            .unwrap();
        assert!(!ws.share_link(&expired.link_id).unwrap().is_live_at(now));

        // Revoke the live link → its record is no longer live and carries a revocation record.
        ws.revoke_link(live.link_id).unwrap();
        let rec = ws.share_link(&live.link_id).unwrap();
        assert!(!rec.is_live_at(now));
        let revoked_at = rec.revocation.as_ref().unwrap().revoked_at.clone();

        // Re-revocation is idempotent; the original timestamp is kept.
        ws.revoke_link(live.link_id).unwrap();
        assert_eq!(
            ws.share_link(&live.link_id)
                .unwrap()
                .revocation
                .as_ref()
                .unwrap()
                .revoked_at,
            revoked_at,
        );

        // Revoking an unknown link is NotFound.
        assert_eq!(
            ws.revoke_link(ShareLinkId(Uuid::now_v7())),
            Err(SharingError::NotFound),
        );
    }

    /// A scope the workspace does not manage cannot be shared.
    #[test]
    fn share_link_unknown_scope_is_unavailable() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        assert_eq!(
            ws.create_link(ShareScope::Album(Uuid::now_v7()), None, None)
                .unwrap_err(),
            SharingError::ScopeUnavailable,
        );
        assert_eq!(
            ws.create_link(ShareScope::Asset(Uuid::now_v7()), None, None)
                .unwrap_err(),
            SharingError::ScopeUnavailable,
        );
    }

    #[test]
    fn hardware_backed_device_imports_and_verifies() {
        use std::sync::Arc;

        use crate::crypto::keys::HardwareBackedSigner;
        use crate::crypto::keys::hardware::MockHardwareSigner;

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(&img, b"\xFF\xD8\xFF hardware-signed asset").unwrap();

        // The DSK's classical half lives in the (mock) secure element; the PQ half is the
        // software ξ seed. Create the workspace with the hardware-backed signer.
        let hw = Arc::new(MockHardwareSigner::new([5; 32], false));
        let signer = HardwareBackedSigner::enroll(hw, "device-dsk".into(), &[6; 32]).unwrap();
        let mut ws = Workspace::create_with_hardware_signer(
            lib.path(),
            b"passphrase",
            Argon2Params {
                mem_kib: 64,
                t_cost: 1,
                p_cost: 1,
            },
            Box::new(signer),
        )
        .unwrap();

        // The full offline lifecycle runs on hardware-composed signatures: the manifest's
        // device_sig (hardware Ed25519 ‖ software ML-DSA) verifies through `verify_asset`
        // against the directory key the workspace published from the same signer.
        let album = ws.create_album("Trip");
        let asset = ws.import_asset(album, &img).unwrap();
        assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
        // A metadata edit re-signs with the hardware signer and still verifies.
        ws.tag_add(&asset, "vacation").unwrap();
        assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
        // The exporter key is the hardware-backed device key (not the account's software DSK).
        assert_eq!(
            ws.exporter_verifying_key(),
            ws.directory
                .device(&ws.account.device.device_id)
                .unwrap()
                .dsk_public
        );
    }
}
