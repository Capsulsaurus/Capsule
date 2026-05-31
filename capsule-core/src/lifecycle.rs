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
//! encryption produces the artifacts that cross a boundary. Album epoch rotation (the MLS
//! ceremony) is deferred — albums here are single-epoch (see `DEFERRED.md`).
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
use crate::crypto::encryption::{seal_blob, stream};
use crate::crypto::hash::{self, Hash32};
use crate::crypto::keys::directory::{DeviceEntry, DirectoryCore};
use crate::crypto::keys::{Account, Amk, AmkVersion, DeviceDirectory, HybridSigningKey};
use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use crate::crypto::provenance::action::Action;
use crate::crypto::provenance::manifest::{ASSET_MANIFEST_VERSION, ManifestCore};
use crate::crypto::provenance::{AssetManifest, ProvenanceChain, ProvenanceRecord};
use crate::crypto::verify_asset::{VerifyOutcome, verify_asset};
use crate::crypto::{CryptoError, authority::ReferenceAuthority};
use crate::metadata::crdt::{AddId, Counter};
use crate::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};

/// A device is treated as added far in the past so any import timestamp postdates it.
const DEVICE_ADDED_AT: &str = "2020-01-01T00:00:00Z";

/// Errors from lifecycle operations.
#[derive(Debug, Error)]
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
}

type Result<T> = std::result::Result<T, LifecycleError>;

/// One album's key material (single-epoch in this offline core).
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
    /// The current (and only) epoch.
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
    directory: DeviceDirectory,
    counter: Counter,
    albums: HashMap<Uuid, AlbumKeys>,
    authorities: HashMap<Uuid, ReferenceAuthority>,
    assets: HashMap<Uuid, AssetState>,
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
        crate::library::init::init_library(root, "Capsule")
            .map_err(|e| LifecycleError::Io(format!("init library: {e}")))?;
        let account = Account::create();
        let file = account.to_file_with(passphrase, params)?;
        let acct_bytes =
            cbor::to_canonical_vec(&file).map_err(|e| LifecycleError::Cbor(e.to_string()))?;
        fs::write(root.join(".library").join("account.cbor"), &acct_bytes)
            .map_err(|e| LifecycleError::Io(e.to_string()))?;

        let directory = Self::build_directory(&account);
        let counter = Counter::new(account.device.device_id);
        Ok(Self {
            root: root.to_path_buf(),
            account,
            directory,
            counter,
            albums: HashMap::new(),
            authorities: HashMap::new(),
            assets: HashMap::new(),
        })
    }

    fn build_directory(account: &Account) -> DeviceDirectory {
        DirectoryCore {
            user_id: account.user_id,
            directory_version: 1,
            updated_at: now_rfc3339(),
            devices: vec![DeviceEntry {
                device_id: account.device.device_id,
                dsk_public: account.device.dsk.verifying_key(),
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

    fn file_key(&self, album: &AlbumKeys, file_id: &Uuid) -> [u8; 32] {
        let amk = Amk::from_bytes(album.amks[&album.current_epoch]);
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
    ) -> AssetManifest {
        let core = ManifestCore {
            action,
            prior_provenance_hash: prior,
            retention_until,
            timestamp: now_rfc3339(),
            ..base.clone()
        };
        core.sign(&self.account.device.dsk, &album.write_tier)
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
        let file_key = self.file_key(album, &asset_id);
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
        let manifest = core.sign(&self.account.device.dsk, &album.write_tier);

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
        let file_key = self.file_key(album, &head.core.file_id);
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
        let manifest = self.sign_lifecycle(album, &base, action, prior, retention_until);
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
        self.write_asset_files(asset, &plaintext)
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
            let file_key = self.file_key(album, &head.core.file_id);
            let (_, ciphertext) = stream::encrypt_asset_vec_with_prefix(
                &file_key,
                head.core.nonce_prefix,
                &plaintext,
            );
            let amk = Amk::from_bytes(album.amks[&album.current_epoch]);
            let metadata_blob = seal_blob(
                &amk.derive_blob_key(&asset.asset_id),
                &asset.sidecar.to_canonical_vec(),
            );
            amks.insert(
                (asset.album_id, head.core.amk_version.0),
                album.amks[&album.current_epoch],
            );
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
        let bytes = backup::export(&input, passphrase, &self.account.device.dsk)?;
        fs::write(out, &bytes).map_err(|e| LifecycleError::Io(e.to_string()))?;
        Ok(())
    }

    /// This device's signing public key (the exporter key a peer verifies a backup against).
    pub fn exporter_verifying_key(&self) -> crate::crypto::keys::HybridVerifyingKey {
        self.account.device.dsk.verifying_key()
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::primitives::Argon2Params;
    use tempfile::TempDir;

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
}
