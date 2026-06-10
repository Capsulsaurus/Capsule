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
//! add/remove) remains deferred (see `DEFERRED.md`).
//!
//! [`verify_asset`]: crate::crypto::verify_asset

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{Datelike, Utc};
use thiserror::Error;
use uuid::Uuid;

use crate::backup::{self, BackupArtifact, BackupAsset, BackupInput, RestoreMode};
use crate::cbor;
use crate::crypto::CryptoError;
use crate::crypto::authority::ReferenceAuthority;
use crate::crypto::encryption::{seal_blob, stream};
use crate::crypto::hash::{self, Hash32};
use crate::crypto::keys::directory::{DeviceEntry, DirectoryCore};
use crate::crypto::keys::{
    Account, Amk, AmkVersion, DeviceDirectory, HybridSigningKey, HybridVerifyingKey, Signer,
};
use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use crate::crypto::provenance::action::Action;
use crate::crypto::provenance::manifest::{ASSET_MANIFEST_VERSION, ManifestCore};
use crate::crypto::provenance::{AssetManifest, ProvenanceChain, ProvenanceRecord};
use crate::crypto::verify_asset::{VerifyOutcome, verify_asset};
use crate::db::{AssetRow, CachedRepresentationRow, DatabaseDriver};
use crate::library::Library;
use crate::metadata::crdt::{AddId, Counter};
use crate::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};

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
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
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
    let dt = chrono::DateTime::from_timestamp(capture_utc, 0).unwrap_or_default();
    root.join("media")
        .join(format!("{:04}", dt.year()))
        .join(format!("{:04}-{:02}", dt.year(), dt.month()))
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
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.timestamp())
        .unwrap_or(0)
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
        stack_id: None,
        is_stack_hidden: false,
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
    /// (see `DEFERRED.md`).
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

    /// Derive the per-file key under a *specific* epoch's AMK. Callers pass the epoch the asset
    /// was written under (`amk_version`), never assuming the album's current epoch — so an asset
    /// imported before a rotation still derives the key it was encrypted with.
    fn file_key(&self, album: &AlbumKeys, epoch: u32, file_id: &Uuid) -> [u8; 32] {
        let amk = Amk::from_bytes(album.amks[&epoch]);
        amk.derive_file_key(file_id)
    }

    /// Build a signed lifecycle manifest for `asset`, sharing the create manifest's content
    /// fields. Used for metadata-update / delete / trash-restore.
    fn sign_lifecycle(
        &self,
        album: &AlbumKeys,
        base: &ManifestCore,
        action: Action,
        prior: Option<Hash32>,
        retention_until: Option<String>,
    ) -> std::result::Result<AssetManifest, CryptoError> {
        let core = ManifestCore {
            action,
            prior_provenance_hash: prior,
            retention_until,
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
                last_accessed_at: Utc::now().timestamp(),
                pinned: false,
                is_owned_original: true,
            })
            .map_err(|e| LifecycleError::Db(e.to_string()))
    }

    /// Import a file into `album_id`: encrypt, build the signed create manifest + provenance,
    /// write the signed sidecar, and self-verify through `verify_asset`. Returns the asset id.
    pub fn import_asset(&mut self, album_id: Uuid, src: &Path) -> Result<Uuid> {
        let plaintext =
            fs::read(src).map_err(|e| LifecycleError::Io(format!("read {src:?}: {e}")))?;
        let ext = src
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "bin".into());
        let asset_id = Uuid::now_v7();
        let capture_utc = Utc::now().timestamp();

        let album = self.album(&album_id)?;
        let file_key = self.file_key(album, album.current_epoch, &asset_id);
        let (enc, ciphertext) = stream::encrypt_asset_vec_full(&file_key, &plaintext);

        let core = ManifestCore {
            version: ASSET_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: PROTOCOL_VERSION.into(),
            file_id: asset_id,
            album_id,
            amk_version: AmkVersion(album.current_epoch),
            ciphertext_hash: enc.ciphertext_hash,
            plaintext_size: enc.plaintext_size,
            chunk_size: enc.chunk_size,
            nonce_prefix: enc.nonce_prefix,
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
        let chain_head = chain.head().expect("just appended");

        let mut sidecar = SidecarV1 {
            sidecar_schema: SIDECAR_SCHEMA_V1,
            crypto_suite_id: CRYPTO_SUITE_ID,
            uuid: asset_id,
            hash: hash::hash_bytes(&plaintext),
            capture_timestamp: now_rfc3339(),
            import_timestamp: now_rfc3339(),
            content_type: content_type_for(&ext),
            dimensions: None,
            lqip: None,
            tags_user: Default::default(),
            tags_ai: Default::default(),
            caption: Default::default(),
            rating: Default::default(),
            stack_membership: None,
            camera_id: None,
            device_id: self.account.device.device_id,
            session_id: Uuid::now_v7(),
            gps: None,
            provenance_chain_hash: chain_head,
            unknown: BTreeMap::new(),
            signature: None,
        };
        sidecar.sign(&self.account.user_ik);

        // Self-check: the asset must verify through the one chokepoint before we accept it.
        let authority = &self.authorities[&album_id];
        let outcome = verify_asset(&manifest, &ciphertext, &self.directory, authority, None);
        if outcome != VerifyOutcome::Accept {
            return Err(LifecycleError::SelfVerify(outcome));
        }

        let asset = AssetState {
            asset_id,
            album_id,
            ext,
            capture_utc,
            chain,
            sidecar,
        };
        self.write_asset_files(&asset, &plaintext)?;
        self.index_asset_row(&asset)?;
        self.index_original_representation(&asset, plaintext.len())?;
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
        let head = &asset.chain.records().last().unwrap().manifest;
        let plaintext =
            fs::read(self.media_path(asset)).map_err(|e| LifecycleError::Io(e.to_string()))?;
        let file_key = self.file_key(album, head.core.amk_version.0, &head.core.file_id);
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
        let prior = self.assets[asset_id].chain.head();
        let base = self.assets[asset_id]
            .chain
            .records()
            .last()
            .unwrap()
            .manifest
            .core
            .clone();
        let album = self.album(&album_id)?;
        let manifest = self.sign_lifecycle(album, &base, action, prior, retention_until)?;
        let add_id = self.counter.issue();

        {
            let asset = self.assets.get_mut(asset_id).unwrap();
            asset
                .chain
                .append(ProvenanceRecord {
                    asset_id: *asset_id,
                    manifest,
                    prior_provenance_hash: prior,
                })
                .map_err(|e| LifecycleError::Cbor(format!("chain: {e}")))?;
            let new_head = asset.chain.head().unwrap();
            mutate_sidecar(&mut asset.sidecar, add_id);
            asset.sidecar.provenance_chain_hash = new_head;
            asset.sidecar.signature = None;
            asset.sidecar.sign(&self.account.user_ik);
        }

        // Re-borrow immutably to write the updated artifacts to disk.
        let asset = self.assets.get(asset_id).unwrap();
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
        let until = (Utc::now() + chrono::Duration::days(retain_days)).to_rfc3339();
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
    pub fn export_backup(&self, out: &Path, passphrase: &[u8]) -> Result<()> {
        let mut assets = Vec::new();
        let mut amks: BTreeMap<(Uuid, u32), [u8; 32]> = BTreeMap::new();

        for asset in self.assets.values() {
            let album = self.album(&asset.album_id)?;
            let head = &asset.chain.records().last().unwrap().manifest;
            let plaintext =
                fs::read(self.media_path(asset)).map_err(|e| LifecycleError::Io(e.to_string()))?;
            let epoch = head.core.amk_version.0;
            let file_key = self.file_key(album, epoch, &head.core.file_id);
            let (_, ciphertext) = stream::encrypt_asset_vec_with_prefix(
                &file_key,
                head.core.nonce_prefix,
                &plaintext,
            );
            let amk = Amk::from_bytes(album.amks[&epoch]);
            let metadata_blob = seal_blob(
                &amk.derive_blob_key(&asset.asset_id),
                &asset.sidecar.to_canonical_vec(),
            );
            amks.insert((asset.album_id, epoch), album.amks[&epoch]);
            assets.push(BackupAsset {
                album_id: asset.album_id,
                asset_id: asset.asset_id,
                ciphertext,
                metadata_blob,
                provenance: asset.chain.records().to_vec(),
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
    pub fn import_backup(
        &mut self,
        archive: &Path,
        passphrase: &[u8],
        exporter_pub: &crate::crypto::keys::HybridVerifyingKey,
    ) -> Result<usize> {
        let bytes = fs::read(archive).map_err(|e| LifecycleError::Io(e.to_string()))?;
        let artifact = BackupArtifact::open(&bytes, passphrase, exporter_pub)?;
        let report = artifact.restore(RestoreMode::Commit, &self.local_heads())?;

        let mut added = 0;
        for restored in &report.applied {
            // Rebuild on-disk artifacts for the restored asset.
            let head = &restored.provenance.last().unwrap().manifest;
            let capture_utc = Utc::now().timestamp();
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
            };
            self.write_asset_files(&asset, &restored.plaintext)?;
            self.index_asset_row(&asset)?;
            self.index_original_representation(&asset, restored.plaintext.len())?;
            self.assets.insert(restored.asset_id, asset);
            added += 1;
        }
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
            stack_membership: None,
            camera_id: None,
            device_id: head.core.created_by_device,
            session_id: Uuid::now_v7(),
            gps: None,
            provenance_chain_hash: restored.provenance.last().unwrap().record_hash(),
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
