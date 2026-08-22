//! Workspace construction and opening: account create/unlock, device-directory publication,
//! and the injected client / still-encoder seams.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use uuid::Uuid;
use walkdir::WalkDir;

use super::{
    AssetState, LifecycleError, Result, StackPlacement, Workspace, media_dir, now_rfc3339,
};
use crate::cbor;
use crate::crypto::keys::albumstore::AlbumStore;
use crate::crypto::keys::directory::{DeviceEntry, DirectoryCore};
use crate::crypto::keys::{Account, AccountFile, DeviceDirectory, HybridVerifyingKey, Signer};
use crate::crypto::provenance::{ProvenanceChain, ProvenanceRecord};
use crate::metadata::crdt::Counter;
use crate::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};

/// A device is treated as added far in the past so any import timestamp postdates it.
const DEVICE_ADDED_AT: &str = "2020-01-01T00:00:00Z";

/// Recover `device`'s `add_id` high-water mark by sweeping the signed sidecars under
/// `root/media` — the maximum `add_id.counter` this device has ever written, across every
/// `AddId`-bearing OR-set of every sidecar (slice `S-A9`).
///
/// `None` means the device provably issued nothing: no sidecar on disk bears its `device_id` in
/// any OR-set, so [`Counter::reseed_from_max`] may safely reset to zero. Anything else is one
/// past the maximum, which is what makes the counter monotonic over the `device_id`'s lifetime
/// rather than merely within one process (SSoT: [Metadata — Add-id Binding § Counter durability
/// across restarts]).
///
/// The sweep reads the sidecars **without re-verifying their signatures**, for the same reason
/// [`rebuild_index`](crate::library::rebuild::rebuild_index) does not: these are the device's own
/// local plaintext files, and the value recovered is only ever a *floor* on the next counter.
/// An unreadable or foreign-schema sidecar is warned about and skipped rather than failing the
/// open — but note that skipping can only lower the floor, so the warning is load-bearing for
/// after-the-fact recovery, not cosmetic.
///
/// [Metadata — Add-id Binding § Counter durability across restarts]: https://docs/design/metadata/#add-id-binding
#[tracing::instrument(skip_all, fields(root = %root.display(), device = %device))]
fn sweep_max_add_counter(root: &Path, device: &Uuid) -> Option<u64> {
    let media = root.join("media");
    if !media.exists() {
        tracing::debug!("add-id reseed: no media directory; device has issued nothing");
        return None;
    }

    let mut max: Option<u64> = None;
    let mut scanned = 0usize;
    let mut skipped = 0usize;
    for entry in WalkDir::new(&media)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        // The signed sidecar is exactly `{uuid}.cbor`; the sibling `{uuid}.provenance.cbor` and
        // `{uuid}.receipts.cbor` logs carry no OR-sets and must not be parsed as sidecars.
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !name.ends_with(".cbor")
            || name.ends_with(".provenance.cbor")
            || name.ends_with(".receipts.cbor")
        {
            continue;
        }
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                skipped += 1;
                tracing::warn!(sidecar = %path.display(), error = %e, "add-id reseed: unreadable sidecar; skipping");
                continue;
            }
        };
        match SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1) {
            Ok(sidecar) => {
                scanned += 1;
                // Every `AddId`-bearing OR-set on the sidecar. The LWW registers (caption,
                // rating, cull, hidden, stack) stamp `(ts, device_id)` and issue no `add_id`.
                for candidate in [
                    sidecar.tags_user.max_add_counter_for(device),
                    sidecar.tags_ai.max_add_counter_for(device),
                ]
                .into_iter()
                .flatten()
                {
                    max = Some(max.map_or(candidate, |m: u64| m.max(candidate)));
                }
            }
            Err(e) => {
                skipped += 1;
                tracing::warn!(sidecar = %path.display(), error = %e, "add-id reseed: undecodable sidecar; skipping");
            }
        }
    }
    tracing::debug!(scanned, skipped, max_issued = ?max, "add-id reseed: sidecar sweep complete");
    max
}

/// The extension of an asset's **original** media file in `dir`: the sibling named
/// `{uuid}.{ext}` that is not one of the sidecar / provenance / receipts / metadata-blob
/// artifacts the lifecycle writes beside it.
fn original_extension(dir: &Path, asset_id: &Uuid) -> Option<String> {
    let prefix = format!("{}.", asset_id.simple());
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.filter_map(std::result::Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(ext) = name.strip_prefix(&prefix) else {
            continue;
        };
        if matches!(
            ext,
            "cbor" | "provenance.cbor" | "receipts.cbor" | "metadata.bin"
        ) || std::path::Path::new(ext)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("tmp"))
        {
            continue;
        }
        return Some(ext.to_string());
    }
    None
}

/// The UTC timestamp of the first instant of the `{YYYY}/{YYYY-MM}` bucket `dir` names — a value
/// that provably resolves back to `dir` through [`media_dir`], used as the fallback when an
/// asset's recorded capture time does not.
fn month_dir_timestamp(dir: &Path) -> i64 {
    let parse = || -> Option<i64> {
        let name = dir.file_name()?.to_string_lossy().into_owned();
        let (year, month) = name.split_once('-')?;
        let date = Date::new(year.parse().ok()?, month.parse().ok()?, 1).ok()?;
        Some(date.to_zoned(TimeZone::UTC).ok()?.timestamp().as_second())
    };
    parse().unwrap_or(0)
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

        // The album keystore exists from day one, even empty: a library whose `albums.cbor` is
        // simply absent is indistinguishable from a pre-`S-A10` one, and `open` treats that as
        // "keys were never persisted" rather than "there are no albums yet".
        AlbumStore::new().save(root, &account.master)?;

        // Default to the account's own software DSK; a hardware signer overrides it.
        let device_signer: Box<dyn Signer> =
            device_signer.unwrap_or_else(|| Box::new(account.device.dsk.clone()));
        let directory = Self::build_directory(&account, device_signer.verifying_key());
        // A freshly minted `device_id` over a freshly initialised (provably empty) library has
        // provably issued no `add_id`, so it starts at zero with no sweep — the one case the
        // add-id durability rule licenses a reset (`S-A9`). Every *re*-open goes through
        // [`open`](Self::open), which reseeds from the sidecars on disk.
        let counter = Counter::new(account.device.device_id);
        Ok(Self {
            root: root.to_path_buf(),
            account,
            device_signer,
            directory,
            client_version: crate::client_build::core_client_version(),
            counter,
            albums: HashMap::new(),
            authorities: HashMap::new(),
            assets: HashMap::new(),
            group_assertions: HashMap::new(),
            library,
            argon2_params: params,
            share_links: HashMap::new(),
            upload_links: HashMap::new(),
            inbox: HashMap::new(),
            #[cfg(feature = "media")]
            still_encoder: None,
        })
    }

    /// Set the reporting client identity every manifest and derivative this workspace authors
    /// carries (S-D15): `client_id` names the product (`capsule-cli`, `capsule-ios`, …) and
    /// `semver` is that client's own version. The build-embedded commit + dirty flag are appended
    /// per the [`client_version` grammar](crate::client_build), so an in-repo app supplies only
    /// its id and version. Without this, a workspace reports the bare-core
    /// `capsule-core/{semver}+{commit}` default.
    #[must_use]
    pub fn with_client_id(mut self, client_id: &str, semver: &str) -> Self {
        self.client_version = crate::client_build::client_version(client_id, semver);
        self
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
    /// a first-time account and the share-link wrap tier.
    ///
    /// Since `S-A10` this restores the workspace's durable state rather than starting empty:
    ///
    /// - **album keys + authorities** from the sealed [`AlbumStore`], so a reopened library can
    ///   decrypt and keep writing into the albums it already has;
    /// - **every managed asset** from its signed artifacts on disk (chain, sidecar, sealed
    ///   metadata blob), a `warn`-and-skip per asset rather than a failed open;
    /// - **the `add_id` counter**, reseeded past everything this device has ever written
    ///   (`S-A9`).
    ///
    /// A library with no `albums.cbor` predates this and opens with zero albums plus a `warn`
    /// naming backup restore — see [`AlbumStore::load`].
    ///
    /// Still session-scoped by design, and dropped on close: the federation group assertions
    /// (re-delivered by the feed), the pending guest-drop inbox (server-authoritative), and the
    /// issued share/upload link records — see the [`Workspace`] fields for why each is deferred.
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
            // First use of this library: seed an empty album keystore alongside the account, so
            // the next open sees "no albums yet" rather than the pre-`S-A10` migration case.
            AlbumStore::new().save(root, &account.master)?;
            account
        };

        let device_signer: Box<dyn Signer> = Box::new(account.device.dsk.clone());
        let directory = Self::build_directory(&account, device_signer.verifying_key());
        let user_id = account.user_id;
        // `S-A9`: reseed the `add_id` counter to one past the maximum this device has ever
        // written, recovered from its own signed sidecars. Without this a reopened library
        // reissues counters from zero and aliases two distinct OR-set adds.
        let device_id = account.device.device_id;
        let mut counter = Counter::new(device_id);
        counter.reseed_from_max(sweep_max_add_counter(root, &device_id));
        tracing::info!(
            device = %device_id,
            next_add_counter = counter.peek(),
            "workspace open: add-id counter reseeded"
        );
        let mut ws = Self {
            root: root.to_path_buf(),
            account,
            device_signer,
            directory,
            client_version: crate::client_build::core_client_version(),
            counter,
            albums: HashMap::new(),
            authorities: HashMap::new(),
            assets: HashMap::new(),
            group_assertions: HashMap::new(),
            library,
            argon2_params: params,
            share_links: HashMap::new(),
            upload_links: HashMap::new(),
            inbox: HashMap::new(),
            #[cfg(feature = "media")]
            still_encoder: None,
        };
        // `S-A10`: album keys, authorities, and every managed asset come back from disk here.
        // Without this the reopened workspace would hold an unlocked account and nothing else.
        ws.restore_durable_state()?;
        tracing::info!(
            user_id = %user_id,
            albums = ws.albums.len(),
            assets = ws.assets.len(),
            "workspace opened"
        );
        Ok(ws)
    }

    /// Restore the durable half of the workspace after unlocking the account (`S-A10`):
    /// album key material + authorities from `{root}/.library/albums.cbor`, then every managed
    /// asset's in-memory state from the artifacts on disk.
    ///
    /// **Migration.** A library with no `albums.cbor` is a pre-`S-A10` library whose album keys
    /// were session-scoped and therefore never written anywhere. It opens with zero albums and a
    /// `warn` naming backup restore as the recovery path. That is not a regression introduced
    /// here: those assets were already undecryptable across a restart, because the only copy of
    /// their AMK died with the process that minted it.
    #[tracing::instrument(skip_all, fields(root = %self.root.display()))]
    fn restore_durable_state(&mut self) -> Result<()> {
        // A read failure here — wrong master key, tampered file, unsupported version — is fatal
        // rather than a warn: silently continuing with zero albums would look exactly like the
        // migration case above and would let a subsequent `create_album` overwrite the store.
        if let Some(store) = AlbumStore::load(&self.root, &self.account.master)? {
            self.apply_album_store(&store);
        } else {
            tracing::warn!(
                path = %AlbumStore::path(&self.root).display(),
                "no album keystore: this library predates durable album keys (S-A10), so it opens \
                 with zero albums. Its existing assets can only be recovered through a backup \
                 artifact (`import_backup`), which escrows their AMKs."
            );
        }
        self.restore_assets();
        Ok(())
    }

    /// Rebuild `self.assets` by walking the signed artifacts under `root/media`.
    ///
    /// The provenance chain is the per-asset anchor: one `{uuid}.provenance.cbor` is one managed
    /// asset, and its head manifest names the owning album. The sidecar, the sealed metadata blob,
    /// and the original bytes are read from its siblings; the queryable index row supplies only
    /// stack placement, which lives nowhere else.
    ///
    /// The filesystem, not the SQLite index, is the source of truth here for the same
    /// recovery-first reason [`rebuild_index`](crate::library::rebuild::rebuild_index) exists: an
    /// index can be rebuilt from the signed artifacts, but an artifact the index has forgotten is
    /// gone. A missing or undecodable piece for one asset is a `warn` and a skip — never a failed
    /// open, which would take the whole library down for one bad file.
    fn restore_assets(&mut self) {
        let media = self.root.join("media");
        if !media.exists() {
            return;
        }
        let mut restored = 0usize;
        let mut skipped = 0usize;
        for entry in WalkDir::new(&media)
            .into_iter()
            .filter_map(std::result::Result::ok)
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let Some(stem) = name.strip_suffix(".provenance.cbor") else {
                continue;
            };
            match self.restore_one_asset(path, stem) {
                Ok(asset) => {
                    restored += 1;
                    tracing::trace!(asset_id = %asset.asset_id, album_id = %asset.album_id, "asset restored");
                    self.assets.insert(asset.asset_id, asset);
                }
                Err(e) => {
                    skipped += 1;
                    tracing::warn!(
                        provenance = %path.display(),
                        error = %e,
                        "workspace open: could not restore this asset; skipping it"
                    );
                }
            }
        }
        tracing::info!(
            restored,
            skipped,
            "workspace open: assets restored from disk"
        );
    }

    fn restore_one_asset(&self, provenance_path: &Path, stem: &str) -> Result<AssetState> {
        let asset_id = Uuid::parse_str(stem)
            .map_err(|e| LifecycleError::NotFound(format!("asset id in {stem}: {e}")))?;
        let dir = provenance_path
            .parent()
            .ok_or_else(|| LifecycleError::Io("provenance file has no parent".into()))?;

        // (1) The provenance chain — the anchor. Replayed through `append` so the chain's own
        // link invariants are re-checked rather than assumed.
        let bytes = fs::read(provenance_path).map_err(|e| LifecycleError::Io(e.to_string()))?;
        let records: Vec<ProvenanceRecord> =
            cbor::from_slice(&bytes).map_err(|e| LifecycleError::Cbor(e.to_string()))?;
        let mut chain = ProvenanceChain::new();
        for rec in records {
            chain
                .append(rec)
                .map_err(|e| LifecycleError::Cbor(format!("provenance chain: {e}")))?;
        }
        let head = &chain
            .records()
            .last()
            .ok_or_else(|| LifecycleError::Cbor("empty provenance chain".into()))?
            .manifest;
        let album_id = head.core.album_id;

        // (2) The signed sidecar.
        let sidecar_bytes = fs::read(dir.join(format!("{}.cbor", asset_id.simple())))
            .map_err(|e| LifecycleError::Io(format!("sidecar: {e}")))?;
        let sidecar = SidecarV1::from_canonical_slice(&sidecar_bytes, SIDECAR_SCHEMA_V1)
            .map_err(LifecycleError::Cbor)?;

        // (3) `capture_utc` decides which media directory every subsequent write for this asset
        // resolves to, so it must reproduce the directory the files were actually found in. The
        // sidecar's own capture timestamp is the value the import used; if it disagrees with the
        // directory (a library written by an older build), fall back to the directory itself so
        // the paths keep resolving, and say so.
        let mut capture_utc = sidecar
            .capture_timestamp
            .parse::<jiff::Timestamp>()
            .map_or(0, |t: jiff::Timestamp| t.as_second());
        if media_dir(&self.root, capture_utc) != dir {
            let from_dir = month_dir_timestamp(dir);
            tracing::warn!(
                asset_id = %asset_id,
                sidecar_capture = %sidecar.capture_timestamp,
                dir = %dir.display(),
                "asset's capture timestamp does not resolve to its own media directory; \
                 using the directory's month so its files stay reachable"
            );
            capture_utc = from_dir;
        }

        // (4) The original's extension: the sibling that is neither a sidecar, a provenance
        // chain, a receipt log, nor the sealed metadata blob.
        let ext = original_extension(dir, &asset_id).ok_or_else(|| {
            LifecycleError::NotFound(format!("original media file for asset {asset_id}"))
        })?;

        // (5) The sealed metadata blob. Absence is tolerated: libraries written before `S-A10`
        // never persisted it. Such an asset reads and verifies fine; only `export_backup` and
        // upload need the blob, and both fail loudly on the empty value rather than shipping a
        // wrong one.
        let blob_path = dir.join(format!("{}.metadata.bin", asset_id.simple()));
        let metadata_blob = match fs::read(&blob_path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    asset_id = %asset_id,
                    "no sealed metadata blob on disk (pre-S-A10 asset); it cannot be exported or \
                     uploaded until its metadata is rewritten"
                );
                Vec::new()
            }
            Err(e) => return Err(LifecycleError::Io(format!("metadata blob: {e}"))),
        };

        // (6) Stack placement lives only in the queryable index.
        let stack = self
            .library
            .db
            .find_by_uuid(&asset_id.to_string())
            .ok()
            .flatten()
            .and_then(|row| {
                row.stack_id.map(|stack_id| StackPlacement {
                    stack_id,
                    hidden: row.is_stack_hidden,
                })
            });

        Ok(AssetState {
            asset_id,
            album_id,
            ext,
            capture_utc,
            chain,
            sidecar,
            metadata_blob,
            stack,
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
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::super::fast_workspace;
    use super::*;
    use crate::crypto::primitives::Argon2Params;
    use crate::crypto::verify_asset::VerifyOutcome;

    /// Fast-Argon2 params for a reopen in these tests (the production cost would dominate).
    fn fast_params() -> Argon2Params {
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        }
    }

    /// **S-A10, the core claim.** An asset imported in one session is fully usable in the next:
    /// its album key comes back from the sealed keystore, so the workspace can re-derive the file
    /// key (proved by `verify_asset` accepting, which regenerates the ciphertext and re-checks its
    /// content address) and can open the sealed metadata blob it persisted to disk.
    #[test]
    fn reopened_workspace_decrypts_an_asset_imported_before_close() {
        use crate::crypto::encryption::{blob_nonce, open_blob};
        use crate::crypto::keys::Amk;

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        let bytes = b"\xFF\xD8\xFF durable album key round trip".to_vec();
        fs::write(&img, &bytes).unwrap();

        let (album, id, blob_before) = {
            let mut ws = fast_workspace(lib.path());
            let album = ws.create_album("Trip").unwrap();
            let id = ws.import_asset(album, &img).unwrap();
            ws.tag_add(&id, "coast").unwrap();
            (album, id, ws.asset(&id).unwrap().metadata_blob.clone())
        };
        assert!(!blob_before.is_empty());

        let ws2 = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();

        // The album's key material is back...
        assert!(ws2.has_album(&album), "the album survived the close");
        assert_eq!(ws2.albums(), vec![(album, "Trip".to_string())]);
        // ...and so is the asset, with the exact sealed blob bytes the manifest committed to.
        let restored = ws2.asset(&id).expect("asset restored from disk");
        assert_eq!(restored.album_id, album);
        assert_eq!(restored.metadata_blob, blob_before);
        assert_eq!(ws2.read_plaintext(&id).unwrap(), bytes);
        assert_eq!(restored.sidecar.tags_user.value().len(), 1);

        // The real proof: `verify_asset` re-derives the file key from the restored AMK,
        // re-encrypts, and matches the manifest's committed ciphertext hash. A wrong (or fresh)
        // AMK cannot produce that.
        assert_eq!(ws2.verify(&id).unwrap(), VerifyOutcome::Accept);

        // And the sealed metadata blob opens under the restored AMK's derived blob key.
        let head = &restored.chain.records().last().unwrap().manifest;
        let amk = Amk::from_bytes(ws2.album(&album).unwrap().amks[&head.core.amk_version.0]);
        let nonce = blob_nonce(&restored.metadata_blob).unwrap();
        let blob_key = amk.derive_blob_key(&id, &nonce);
        let plaintext = open_blob(&blob_key, &restored.metadata_blob).unwrap();
        assert_eq!(
            plaintext,
            restored.sidecar.to_canonical_vec(),
            "the persisted blob decrypts to the signed sidecar under the restored AMK"
        );
    }

    /// **S-A10.** A reopened library keeps *writing* into the album it already has — the write-tier
    /// and admin keys and the attested authority all survive, so a second-session import self-
    /// verifies under the same epoch instead of needing a brand-new album.
    #[test]
    fn reopened_workspace_writes_into_the_same_album() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let first = src.path().join("first.jpg");
        let second = src.path().join("second.jpg");
        fs::write(&first, b"\xFF\xD8\xFF session one").unwrap();
        fs::write(&second, b"\xFF\xD8\xFF session two").unwrap();

        let (album, id1) = {
            let mut ws = fast_workspace(lib.path());
            let album = ws.create_album("Imports").unwrap();
            let id1 = ws.import_asset(album, &first).unwrap();
            (album, id1)
        };

        let mut ws2 = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
        // Resolve-or-create returns the SAME album rather than minting a second one.
        assert_eq!(ws2.ensure_album(album, "Imports").unwrap(), album);
        assert_eq!(ws2.albums().len(), 1, "no duplicate album was minted");

        let id2 = ws2.import_asset(album, &second).unwrap();
        assert_eq!(ws2.verify(&id2).unwrap(), VerifyOutcome::Accept);
        // The pre-close asset still verifies in the same session as the new one.
        assert_eq!(ws2.verify(&id1).unwrap(), VerifyOutcome::Accept);
        // Both landed under the same epoch of the same album.
        let epoch_of = |ws: &Workspace, id: &Uuid| {
            ws.asset(id).unwrap().chain.records()[0]
                .manifest
                .core
                .amk_version
        };
        assert_eq!(epoch_of(&ws2, &id1), epoch_of(&ws2, &id2));
        // Metadata edits work too (they need the write-tier key and the counter).
        ws2.tag_add(&id1, "kept").unwrap();
        assert_eq!(ws2.verify(&id1).unwrap(), VerifyOutcome::Accept);

        // A rotation still works after a reopen: it needs the persisted admin key.
        assert_eq!(ws2.rotate_epoch(album).unwrap(), 2);
    }

    /// **S-A10.** The authority — not just the content key — is durable: the admin-signed epoch
    /// ledger comes back verifying, with the epoch ceiling and per-epoch write-tier keys intact,
    /// and its local-only `amk_present` flags restored from the epochs actually held.
    #[test]
    fn reopened_workspace_restores_the_reference_authority_ledger() {
        use crate::crypto::authority::AlbumAuthority;
        use crate::crypto::keys::AmkVersion;

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(&img, b"\xFF\xD8\xFF written at epoch one").unwrap();

        let (album, ceiling, write_pubs) = {
            let mut ws = fast_workspace(lib.path());
            let album = ws.create_album("Trip").unwrap();
            ws.import_asset(album, &img).unwrap();
            assert_eq!(ws.rotate_epoch(album).unwrap(), 2);
            let authority = ws.authority(&album).unwrap();
            let pubs: Vec<_> = (1..=2)
                .map(|e| authority.write_tier_pubkey(AmkVersion(e)))
                .collect();
            (album, authority.epoch_ceiling(), pubs)
        };

        let ws2 = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
        let authority = ws2.authority(&album).expect("authority restored");
        assert!(
            authority.admin_chain_verifies(),
            "the persisted ledger must re-verify its admin signature chain"
        );
        assert_eq!(authority.epoch_ceiling(), ceiling);
        assert_eq!(authority.album_id(), album);
        for (i, expected) in write_pubs.iter().enumerate() {
            assert_eq!(
                &authority.write_tier_pubkey(AmkVersion(i as u32 + 1)),
                expected,
                "epoch {} write-tier key survives the reopen",
                i + 1
            );
        }
        // `amk_present` is local-only state, restored from the epochs whose AMK is actually held.
        assert!(authority.has_amk(AmkVersion(1)));
        assert!(authority.has_amk(AmkVersion(2)));
    }

    /// **S-A10, migration.** A library written before durable album keys has no `albums.cbor`. It
    /// must still open — with zero albums and a warning — rather than failing. This is not a
    /// regression: those assets' AMKs never existed anywhere but in the process that minted them.
    #[test]
    fn legacy_library_without_album_store_opens_empty() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(&img, b"\xFF\xD8\xFF pre-S-A10 asset").unwrap();

        let id = {
            let mut ws = fast_workspace(lib.path());
            let album = ws.create_album("Trip").unwrap();
            ws.import_asset(album, &img).unwrap()
        };

        // Simulate the pre-S-A10 on-disk shape: signed artifacts present, keystore absent.
        let store = crate::crypto::keys::AlbumStore::path(lib.path());
        assert!(store.exists());
        fs::remove_file(&store).unwrap();

        let ws2 = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
        assert!(ws2.albums().is_empty(), "no album keys are recoverable");
        // The asset itself is still tracked (its plaintext is on disk) — only its key is gone.
        assert!(ws2.asset(&id).is_some());
        assert_eq!(
            ws2.read_plaintext(&id).unwrap(),
            b"\xFF\xD8\xFF pre-S-A10 asset"
        );
        // And every key-bearing operation refuses with a typed error rather than panicking.
        assert!(matches!(ws2.verify(&id), Err(LifecycleError::NotFound(_))));
        assert!(matches!(
            ws2.export_backup(&src.path().join("b.tar"), b"pw"),
            Err(LifecycleError::NotFound(_))
        ));
    }

    /// **S-A10.** `export_backup` reads each asset's album AMK *and* its sealed metadata blob, so
    /// it is the sharpest test that both survived the close — and the restored artifact must be
    /// byte-identical to one exported before it.
    #[test]
    fn backup_export_round_trips_after_reopen() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        let bytes = b"\xFF\xD8\xFF exported after a reopen".to_vec();
        fs::write(&img, &bytes).unwrap();

        let id = {
            let mut ws = fast_workspace(lib.path());
            let album = ws.create_album("Trip").unwrap();
            let id = ws.import_asset(album, &img).unwrap();
            ws.tag_add(&id, "coast").unwrap();
            id
        };

        // Export from the *reopened* workspace.
        let ws2 = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
        let archive = src.path().join("backup.tar");
        ws2.export_backup(&archive, b"recovery-pass").unwrap();
        let exporter_pub = ws2.exporter_verifying_key();

        // Restore into a fresh library and confirm the bytes come back.
        let fresh = TempDir::new().unwrap();
        let mut ws3 = fast_workspace(fresh.path());
        assert_eq!(
            ws3.import_backup(&archive, b"recovery-pass", &exporter_pub)
                .unwrap(),
            1
        );
        assert_eq!(ws3.read_plaintext(&id).unwrap(), bytes);

        // The restore folded the escrowed AMK into ws3's durable keystore, so ws3 can itself
        // re-export the asset — the "keyless library made whole again" path.
        let album = ws3.asset(&id).unwrap().album_id;
        assert!(ws3.has_album(&album));
        let again = fresh.path().join("re-export.tar");
        ws3.export_backup(&again, b"pass-two").unwrap();
        assert!(again.exists());

        // And those recovered keys are durable in ws3 too: reopening it keeps them.
        drop(ws3);
        let ws4 = Workspace::open(fresh.path(), b"passphrase", fast_params()).unwrap();
        assert!(
            ws4.has_album(&album),
            "recovered AMKs were persisted, not just held for the session"
        );
        assert_eq!(ws4.read_plaintext(&id).unwrap(), bytes);
    }

    /// **S-A10.** An album recovered purely from a backup artifact holds content keys and no
    /// signing capability — the artifact escrows AMKs, never the write-tier/admin keys or the
    /// admin-signed ledger. Authoring a new write into it must be a typed refusal, not a fresh
    /// admin key minted behind the user's back.
    #[test]
    fn a_backup_recovered_album_is_readable_but_not_writable() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(&img, b"\xFF\xD8\xFF escrowed key only").unwrap();

        let archive = src.path().join("backup.tar");
        let (album, exporter_pub) = {
            let mut ws = fast_workspace(lib.path());
            let album = ws.create_album("Trip").unwrap();
            ws.import_asset(album, &img).unwrap();
            ws.export_backup(&archive, b"pw").unwrap();
            (album, ws.exporter_verifying_key())
        };

        let fresh = TempDir::new().unwrap();
        let mut ws2 = fast_workspace(fresh.path());
        ws2.import_backup(&archive, b"pw", &exporter_pub).unwrap();

        assert!(ws2.has_album(&album));
        let keys = ws2.album(&album).unwrap();
        assert!(keys.write_tier.is_none(), "no escrowed write capability");
        assert!(keys.admin.is_none(), "no escrowed admin capability");
        assert!(matches!(
            keys.write_tier_signer(),
            Err(LifecycleError::AlbumReadOnly(id)) if id == album
        ));
        // A new import into it refuses cleanly...
        let other = src.path().join("other.jpg");
        fs::write(&other, b"\xFF\xD8\xFF new write").unwrap();
        assert!(matches!(
            ws2.import_asset(album, &other),
            Err(LifecycleError::AlbumReadOnly(_))
        ));
        // ...as does a rotation, without half-mutating the AMK map.
        let epochs_before = ws2.album(&album).unwrap().amks.len();
        assert!(matches!(
            ws2.rotate_epoch(album),
            Err(LifecycleError::AlbumReadOnly(_))
        ));
        assert_eq!(ws2.album(&album).unwrap().amks.len(), epochs_before);
    }

    /// **S-A10.** `create_album_with_id` refuses to mint over an album that already holds key
    /// material: doing so would discard the AMKs every existing asset in it was encrypted under.
    #[test]
    fn creating_an_album_that_already_exists_is_refused() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let id = ws.default_album_id();
        assert_eq!(ws.create_album_with_id(id, "Imports").unwrap(), id);
        assert!(matches!(
            ws.create_album_with_id(id, "Imports again"),
            Err(LifecycleError::AlbumExists(dup)) if dup == id
        ));
        // Resolve-or-create is the safe verb.
        assert_eq!(ws.ensure_album(id, "Imports").unwrap(), id);
        assert_eq!(ws.albums(), vec![(id, "Imports".to_string())]);
    }

    /// **S-A9.** A reopened library must never reissue an `add_id` counter it has already
    /// written: `Workspace::open` reseeds from the maximum counter this device's own signed
    /// sidecars record, so the next issued counter is strictly greater than every prior one.
    #[test]
    fn add_id_counter_reseeds_strictly_greater_after_reopen() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(&img, b"\xFF\xD8\xFF add-id durability bytes").unwrap();

        // Session 1: two tag adds burn counters 0 and 1 into the signed sidecar.
        let (device, issued) = {
            let mut ws = fast_workspace(lib.path());
            let album = ws.create_album("Trip").unwrap();
            let id = ws.import_asset(album, &img).unwrap();
            ws.tag_add(&id, "vacation").unwrap();
            ws.tag_add(&id, "coast").unwrap();
            let issued: Vec<u64> = ws
                .asset(&id)
                .unwrap()
                .sidecar
                .tags_user
                .entries()
                .iter()
                .map(|(add_id, _)| add_id.counter)
                .collect();
            assert_eq!(issued, vec![0, 1], "session 1 issues 0 and 1");
            (ws.account.device.device_id, issued)
        };

        // Session 2: a brand-new process over the same library.
        let ws2 = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
        assert_eq!(
            ws2.account.device.device_id, device,
            "reopening resumes the same device identity"
        );
        let mut counter = ws2.counter.clone();
        let next = counter.issue().counter;
        assert!(
            next > *issued.iter().max().unwrap(),
            "reseeded counter {next} must be strictly greater than every written counter {issued:?}"
        );
        assert_eq!(next, 2);
    }

    /// **S-A9.** The reset-to-zero case is the *only* one the durability rule licenses: a device
    /// whose sidecars bear none of its `add_id`s has provably issued nothing.
    #[test]
    fn add_id_counter_resets_to_zero_for_a_device_that_issued_nothing() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(&img, b"\xFF\xD8\xFF no tags were ever added").unwrap();

        {
            // Import writes a sidecar, but an import issues no `add_id` — the OR-sets are empty.
            let mut ws = fast_workspace(lib.path());
            let album = ws.create_album("Trip").unwrap();
            ws.import_asset(album, &img).unwrap();
        }

        let ws2 = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
        assert_eq!(
            ws2.counter.peek(),
            0,
            "a device with no written add_id starts at zero"
        );
    }

    /// **S-A9.** The high-water mark is per-device: another device's much larger counters in the
    /// same sidecar must not inflate this device's next `add_id` (they are a different key in the
    /// `(device, counter)` space, and consuming them would burn 500 of our own counters for
    /// nothing).
    #[test]
    fn add_id_reseed_ignores_other_devices_counters() {
        use crate::metadata::crdt::AddId;

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(&img, b"\xFF\xD8\xFF two devices tagged this asset").unwrap();

        let asset_id = {
            let mut ws = fast_workspace(lib.path());
            let album = ws.create_album("Trip").unwrap();
            let id = ws.import_asset(album, &img).unwrap();
            ws.tag_add(&id, "ours").unwrap(); // our device: counter 0

            // Fold in a peer device's add at a far higher counter, exactly as a merge from that
            // peer's sidecar would, and re-persist the sidecar.
            let asset = ws.assets.get_mut(&id).unwrap();
            asset.sidecar.tags_user.add(
                "theirs".to_string(),
                AddId {
                    device: Uuid::from_u128(0xBEEF),
                    counter: 500,
                },
            );
            let path = ws.sidecar_path(ws.asset(&id).unwrap());
            fs::write(path, ws.asset(&id).unwrap().sidecar.to_canonical_vec()).unwrap();
            id
        };
        assert_ne!(asset_id, Uuid::nil());

        let ws2 = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
        assert_eq!(
            ws2.counter.peek(),
            1,
            "our next counter is one past OUR max (0), not one past the peer's 500"
        );
    }

    /// S-D15: an injected `client_id` flows onto every write the workspace authors — the create
    /// manifest and a later metadata-update record both report the app's identity (not the bare
    /// `capsule-core` default), and each value parses as the normative grammar.
    #[test]
    fn injected_client_id_flows_to_every_write() {
        use crate::client_build::ClientVersion;

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        std::fs::write(&img, b"\xFF\xD8\xFF client-id provenance bytes").unwrap();

        let mut ws = fast_workspace(lib.path()).with_client_id("capsule-ios", "9.9.9");
        let album = ws.create_album("Trip").unwrap();
        let id = ws.import_asset(album, &img).unwrap();

        // The create manifest reports the injected identity, grammar-conformant.
        let create_cv = ws
            .asset(&id)
            .unwrap()
            .chain
            .records()
            .last()
            .unwrap()
            .manifest
            .core
            .client_version
            .clone();
        assert!(create_cv.starts_with("capsule-ios/9.9.9+"), "{create_cv}");
        let parsed = ClientVersion::parse(&create_cv).expect("create client_version parses");
        assert_eq!(parsed.client_id, "capsule-ios");
        assert_eq!(parsed.semver, "9.9.9");

        // A metadata-update mints a fresh record that also carries the producing client.
        ws.tag_add(&id, "vacation").unwrap();
        let update_cv = &ws
            .asset(&id)
            .unwrap()
            .chain
            .records()
            .last()
            .unwrap()
            .manifest
            .core
            .client_version;
        assert!(update_cv.starts_with("capsule-ios/9.9.9+"), "{update_cv}");
        assert!(ClientVersion::parse(update_cv).is_some());
    }

    /// The default (un-injected) workspace reports the bare-core identity.
    #[test]
    fn default_workspace_reports_capsule_core() {
        use crate::client_build::ClientVersion;

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        std::fs::write(&img, b"\xFF\xD8\xFF default identity bytes").unwrap();

        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();
        let id = ws.import_asset(album, &img).unwrap();
        let cv = &ws
            .asset(&id)
            .unwrap()
            .chain
            .records()
            .last()
            .unwrap()
            .manifest
            .core
            .client_version;
        assert_eq!(ClientVersion::parse(cv).unwrap().client_id, "capsule-core");
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
        let album = ws.create_album("Trip").unwrap();
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
