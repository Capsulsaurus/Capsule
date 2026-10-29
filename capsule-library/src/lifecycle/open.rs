//! Workspace construction and opening: account create/unlock, device-directory publication,
//! and the injected client / still-encoder seams.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use capsule_core::cbor;
use capsule_core::crypto::CryptoError;
use capsule_core::crypto::keys::albumstore::AlbumStore;
use capsule_core::crypto::keys::directory::{DekPublic, DeviceEntry, DirectoryCore};
use capsule_core::crypto::keys::{
    Account, AccountFile, DeviceDirectory, HardwareKeyAgreement, HybridVerifyingKey, Signer,
};
use capsule_core::crypto::provenance::{Action, ProvenanceChain, ProvenanceRecord};
use capsule_core::metadata::crdt::Counter;
use capsule_core::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use uuid::Uuid;
use walkdir::WalkDir;

use super::unanchored::find_unanchored;
use super::{
    AssetState, LifecycleError, Result, StackPlacement, Workspace, media_dir, now_rfc3339,
};
use crate::album_store::AlbumStoreFile;

/// A device is treated as added far in the past so any import timestamp postdates it.
const DEVICE_ADDED_AT: &str = "2020-01-01T00:00:00Z";

/// The signed device directory's file under `{root}/.library/` (#562).
const DIRECTORY_FILE: &str = "directory.cbor";

/// A request to bind this workspace's **device encryption key** to a per-platform secure element:
/// the P-256 key-agreement element itself plus the alias its key is held under.
///
/// Supplying one makes the DEK's classical half hardware-held for the life of the account — see
/// [`DeviceDek`](capsule_core::crypto::keys::DeviceDek). Omitting it keeps the software X-Wing DEK,
/// which is the design's explicit fallback for hosts with no element, not a degraded mode.
///
/// The alias is used only when the account is **created**; on reopen the alias recorded in the
/// account file wins, so renaming it here can never silently re-bind an existing account to a
/// different hardware key.
#[derive(Clone)]
pub struct HardwareDekBinding {
    /// The secure element performing the P-256 ECDH (Secure Enclave / StrongBox / TPM).
    pub element: Arc<dyn HardwareKeyAgreement>,
    /// The element's alias for this device's key-agreement key.
    pub key_alias: String,
}

impl std::fmt::Debug for HardwareDekBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HardwareDekBinding")
            .field("key_alias", &self.key_alias)
            .finish_non_exhaustive()
    }
}

/// Recover `device`'s `add_id` high-water mark by sweeping the signed sidecars under
/// `root/media` — the maximum `add_id.counter` this device has ever written, across every
/// `AddId`-bearing OR-set of every sidecar.
///
/// `None` means the device provably issued nothing: no sidecar on disk bears its `device_id` in
/// any OR-set, so [`Counter::reseed_from_max`] may safely reset to zero. Anything else is one
/// past the maximum, which is what makes the counter monotonic over the `device_id`'s lifetime
/// rather than merely within one process (SSoT: [Metadata — Add-id Binding § Counter durability
/// across restarts]).
///
/// The sweep reads the sidecars **without re-verifying their signatures**, for the same reason
/// [`rebuild_index`](crate::library::rebuild_index) does not: these are the device's own
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
        // The signed sidecar is exactly `{uuid}.cbor`; the sibling `{uuid}.provenance.cbor`,
        // `{uuid}.receipts.cbor` and `{uuid}.attestations.cbor` logs and the
        // `derivatives/{uuid}.derivatives.cbor` bundle carry no OR-sets and must not be parsed
        // as sidecars.
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !crate::library::paths::is_sidecar_file_name(&name) {
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
                // Every `AddId`-bearing OR-set on the sidecar — user and AI tags and each
                // region's labels. The LWW registers (caption, rating, cull, hidden, stack)
                // stamp `(ts, device_id)` and issue no `add_id`.
                if let Some(candidate) = sidecar.max_add_counter_for(device) {
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
/// `{uuid}.{ext}` that is not one of the sidecar / provenance / receipts / attestations /
/// metadata-blob artifacts the lifecycle writes beside it.
pub(super) fn original_extension(dir: &Path, asset_id: &Uuid) -> Option<String> {
    let prefix = format!("{}.", asset_id.simple());
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.filter_map(std::result::Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(ext) = name.strip_prefix(&prefix) else {
            continue;
        };
        if matches!(
            ext,
            "cbor" | "provenance.cbor" | "receipts.cbor" | "attestations.cbor" | "metadata.bin"
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
pub(super) fn month_dir_timestamp(dir: &Path) -> i64 {
    let parse = || -> Option<i64> {
        let name = dir.file_name()?.to_string_lossy().into_owned();
        let (year, month) = name.split_once('-')?;
        let date = Date::new(year.parse().ok()?, month.parse().ok()?, 1).ok()?;
        Some(date.to_zoned(TimeZone::UTC).ok()?.timestamp().as_second())
    };
    parse().unwrap_or(0)
}

/// Read a `{uuid}.provenance.cbor` file, replayed through `append` so the chain's own link
/// invariants are re-checked rather than assumed.
fn read_chain(provenance_path: &Path) -> Result<ProvenanceChain> {
    let bytes = fs::read(provenance_path).map_err(|e| LifecycleError::Io(e.to_string()))?;
    let records: Vec<ProvenanceRecord> =
        cbor::from_slice(&bytes).map_err(|e| LifecycleError::Cbor(e.to_string()))?;
    let mut chain = ProvenanceChain::new();
    for rec in records {
        chain
            .append(rec)
            .map_err(|e| LifecycleError::Cbor(format!("provenance chain: {e}")))?;
    }
    Ok(chain)
}

/// The asset id and kept chain head of a purged asset: a provenance chain that reads whole, is
/// headed by a `delete`, and has no sidecar beside it. `None` for anything else, which is
/// damage (a lost sidecar of a live or restored asset, a corrupt chain) rather than a purge.
fn purged_head(provenance_path: &Path, stem: &str) -> Option<(Uuid, ProvenanceRecord)> {
    if provenance_path
        .with_file_name(format!("{stem}.cbor"))
        .exists()
    {
        return None;
    }
    let asset_id = Uuid::parse_str(stem).ok()?;
    let head = read_chain(provenance_path).ok()?.records().last()?.clone();
    (head.manifest.core.action == Action::Delete).then_some((asset_id, head))
}

impl Workspace {
    /// Create a fresh workspace: initialise the library directory and a new account, and
    /// publish a device directory. `passphrase` guards the on-disk account; `tier` sets the
    /// Argon2id cost.
    pub fn create(
        root: &Path,
        passphrase: &[u8],
        tier: capsule_core::crypto::primitives::DeviceTier,
    ) -> Result<Self> {
        Self::create_with_params(root, passphrase, tier.params())
    }

    /// As [`create`](Self::create) but with explicit Argon2id parameters (tests use a fast cost).
    pub fn create_with_params(
        root: &Path,
        passphrase: &[u8],
        params: capsule_core::crypto::primitives::Argon2Params,
    ) -> Result<Self> {
        Self::create_inner(root, passphrase, params, None, None)
    }

    /// As [`create_with_params`](Self::create_with_params) but signs with a caller-supplied
    /// device signer — e.g. a [hardware-backed key](capsule_core::crypto::keys::HardwareBackedSigner)
    /// (Secure Enclave / StrongBox / TPM). The published device directory and every asset
    /// manifest are then signed by `device_signer`, and its public half is what peers trust.
    pub fn create_with_hardware_signer(
        root: &Path,
        passphrase: &[u8],
        params: capsule_core::crypto::primitives::Argon2Params,
        device_signer: Box<dyn Signer>,
    ) -> Result<Self> {
        Self::create_inner(root, passphrase, params, Some(device_signer), None)
    }

    /// As [`create_with_params`](Self::create_with_params) but with the device **encryption** key's
    /// classical half generated inside `binding`'s secure element and never leaving it.
    /// The ML-KEM-768 half is software-sealed under the master key as usual, so the account file
    /// round-trips; reopening the library needs the same element back, via
    /// [`open_with_hardware_dek`](Self::open_with_hardware_dek).
    ///
    /// This is the DEK counterpart of
    /// [`create_with_hardware_signer`](Self::create_with_hardware_signer); the two are independent
    /// (an element may back one, the other, or both — see
    /// [`create_with_hardware_keys`](Self::create_with_hardware_keys)).
    pub fn create_with_hardware_dek(
        root: &Path,
        passphrase: &[u8],
        params: capsule_core::crypto::primitives::Argon2Params,
        binding: HardwareDekBinding,
    ) -> Result<Self> {
        Self::create_inner(root, passphrase, params, None, Some(binding))
    }

    /// The general hardware-binding constructor: bind the device signing key, the device
    /// encryption key, both, or neither. `None`/`None` is exactly
    /// [`create_with_params`](Self::create_with_params) — the software fallback the design keeps
    /// for hosts with no secure element.
    pub fn create_with_hardware_keys(
        root: &Path,
        passphrase: &[u8],
        params: capsule_core::crypto::primitives::Argon2Params,
        device_signer: Option<Box<dyn Signer>>,
        dek_binding: Option<HardwareDekBinding>,
    ) -> Result<Self> {
        Self::create_inner(root, passphrase, params, device_signer, dek_binding)
    }

    /// Create a fresh library on a **new device** that opens *as an existing account*: the
    /// recovery path ("bootstrap a new device via passphrase and escrow", design/backup-recovery).
    ///
    /// - `master` is the account master key, e.g. from
    ///   [`recover_master_key`](capsule_core::backup::recover_master_key) over the server escrow;
    /// - `user_id` is the account's id — the **server's** id for a registered account, so the
    ///   directory and every manifest this library authors name the account the server knows;
    /// - `sealed_identity` is the account's user identity key sealed under `master`
    ///   ([`Workspace::sealed_identity`] on a device that has it). The IK is random, not derived
    ///   from the master key, and the server pins it on first publish, so a recovered device
    ///   must carry the same one rather than mint its own.
    ///
    /// The device is new — a fresh `device_id`, software DSK and software DEK — and the default
    /// album id is the account's, since it derives from `master`. Album keys are not here: they
    /// arrive through [`import_backup`](Self::import_backup) or MLS, exactly as on any new device.
    ///
    /// Fails with [`CryptoError::Auth`] (before anything is written under `root`) when
    /// `sealed_identity` was not sealed under `master` — which is also the check that `master`
    /// is this account's key at all.
    ///
    /// The directory this returns lists only the new device, at `directory_version` 1. That is
    /// **not** publishable on an account that already has a directory: the server refuses it as
    /// stale, and accepting it would drop every other device. Before publishing, fetch the
    /// account's signed directory and pass it to
    /// [`extend_account_directory`](Self::extend_account_directory), then publish what that
    /// returns.
    #[tracing::instrument(skip_all, fields(root = %root.display(), user_id = %user_id))]
    pub fn create_from_master(
        root: &Path,
        passphrase: &[u8],
        params: capsule_core::crypto::primitives::Argon2Params,
        master: capsule_core::crypto::keys::MasterKey,
        user_id: Uuid,
        sealed_identity: &[u8],
    ) -> Result<Self> {
        let user_ik = Account::open_sealed_identity(&master, sealed_identity).inspect_err(|e| {
            tracing::warn!(error = %e, "workspace create from master: the sealed identity did not open");
        })?;
        let account = Account::from_parts(
            crate::ambient::system_ids(),
            user_id,
            master,
            user_ik,
            capsule_core::crypto::keys::DeviceDek::Software(
                capsule_core::crypto::keys::DekKeypair::generate(),
            ),
        );
        let library = crate::library::init::init_library(root, "Capsule")
            .map_err(|e| LifecycleError::Io(format!("init library: {e}")))?;
        tracing::info!(
            device = %account.device.device_id,
            "workspace create from master: a new device of an existing account"
        );
        Self::finish_create(root, library, passphrase, params, account, None)
    }

    /// Rebind this library to the account id the **server** assigned at registration.
    ///
    /// A library mints its own `user_id` when it is created; a server mints another when the
    /// account registers, and refuses a device directory or a manifest that names any id but its
    /// own. This adopts the server's id: the account file is rewritten (the id is stored beside
    /// the wrapped keys, not inside them, so no passphrase is needed) and the device directory is
    /// re-signed at the next `directory_version` naming it.
    ///
    /// Refused with [`LifecycleError::AccountRebind`] once the library holds any asset: every
    /// manifest already signed names the old id as `created_by_user`, and rebinding under them
    /// would leave a library whose history contradicts its own account. Binding the same id again
    /// is a no-op.
    #[tracing::instrument(skip_all, fields(from = %self.account.user_id, to = %user_id))]
    pub fn bind_account_id(&mut self, user_id: Uuid) -> Result<()> {
        if self.account.user_id == user_id {
            // An earlier bind may have rewritten the account file and failed to persist the
            // re-signed directory; this retry is what writes it, so the next open restores it
            // rather than re-signing the stale one.
            if Self::persisted_directory_is(&self.root, &self.directory) {
                tracing::debug!("account id already bound; nothing to do");
                return Ok(());
            }
            tracing::info!(
                directory_version = self.directory.core.directory_version,
                "account id already bound; persisting the directory an earlier bind failed to write"
            );
            return Self::persist_directory(&self.root, &self.directory);
        }
        if !self.assets.is_empty() {
            tracing::warn!(
                assets = self.assets.len(),
                "account rebind refused: the library already authored assets under its old id"
            );
            return Err(LifecycleError::AccountRebind(format!(
                "{} asset(s) already name {} as created_by_user",
                self.assets.len(),
                self.account.user_id
            )));
        }
        let path = self.root.join(".library").join("account.cbor");
        let bytes = fs::read(&path).map_err(|e| LifecycleError::Io(e.to_string()))?;
        let mut file: AccountFile =
            cbor::from_slice(&bytes).map_err(|e| LifecycleError::Cbor(e.to_string()))?;
        file.user_id = user_id;
        let bytes =
            cbor::to_canonical_vec(&file).map_err(|e| LifecycleError::Cbor(e.to_string()))?;
        // Temp-then-rename: this file is the only local wrapped copy of the master and identity
        // keys, so a crash mid-write must leave the old file intact rather than truncated.
        let tmp = crate::utils::paths::tmp_path(&path);
        fs::write(&tmp, &bytes).map_err(|e| LifecycleError::Io(e.to_string()))?;
        fs::rename(&tmp, &path).map_err(|e| LifecycleError::Io(e.to_string()))?;

        self.account.user_id = user_id;
        let mut core = self.directory.core.clone();
        core.user_id = user_id;
        core.directory_version += 1;
        core.updated_at = now_rfc3339();
        // Adopted in memory before it is persisted, unlike the other directory changes: the
        // account file above already names the new id, so a retry is a no-op and memory must not
        // keep a directory naming the old one. Should this write fail, a retry with the same id
        // writes it; failing that, the next open finds the directory naming the old id, signed
        // by this account's IK, and re-signs it under the new id at the next version, keeping
        // every device it lists (`restore_directory`).
        self.directory = core.sign(&self.account.user_ik);
        tracing::info!(
            directory_version = self.directory.core.directory_version,
            "account rebound to the server-assigned id"
        );
        Self::persist_directory(&self.root, &self.directory)
    }

    /// Build this device's directory on the account's **published** one instead of replacing it
    /// (#550): the step a device that joined an existing account — a recovered device from
    /// [`create_from_master`](Self::create_from_master) — takes before it publishes.
    ///
    /// `published` is the account's current signed directory as the server serves it. It is
    /// verified under this workspace's user IK (the one opened from the sealed identity) and must
    /// name this workspace's account; then this device's own entry is appended at
    /// `published.directory_version + 1`, the result is re-signed under the user IK, adopted as
    /// this workspace's directory, and returned for publication. Every device `published` lists
    /// is kept, revoked entries included.
    ///
    /// Returns `Ok(None)` when `published` already lists this device under the signing key it
    /// signs with: the published directory is adopted unchanged and there is nothing to publish.
    /// A recovered device that extends again after publishing lands here. Either way the adopted
    /// directory is persisted, so [`open`](Self::open) restores it.
    ///
    /// Refused, leaving this workspace's directory unchanged, with
    /// [`LifecycleError::UntrustedDirectory`] when `published` does not verify or names another
    /// account, and with [`LifecycleError::Directory`] when it lists this device's id under
    /// another key (or revoked), lists this device's signing key under another id, or its version
    /// cannot advance.
    #[tracing::instrument(
        skip_all,
        fields(
            user_id = %self.account.user_id,
            device_id = %self.account.device.device_id,
            published_version = published.core.directory_version,
        )
    )]
    pub fn extend_account_directory(
        &mut self,
        published: &DeviceDirectory,
    ) -> Result<Option<DeviceDirectory>> {
        if !published.verify(&self.account.user_ik.verifying_key()) {
            tracing::warn!("account directory refused: it does not verify under the user IK");
            return Err(LifecycleError::UntrustedDirectory(
                "the signature does not verify under this account's user identity key".into(),
            ));
        }
        if published.core.user_id != self.account.user_id {
            tracing::warn!(
                published_user = %published.core.user_id,
                "account directory refused: it names another account"
            );
            return Err(LifecycleError::UntrustedDirectory(format!(
                "it names account {}, not {}",
                published.core.user_id, self.account.user_id
            )));
        }

        let device_id = self.account.device.device_id;
        let own = self.directory.device(&device_id).cloned().ok_or_else(|| {
            LifecycleError::NotFound(format!("device {device_id} in its own directory"))
        })?;
        if published.device(&device_id).is_some_and(|listed| {
            listed.dsk_public == own.dsk_public && listed.revoked_at.is_none()
        }) {
            self.adopt_directory(published.clone())?;
            tracing::info!(
                directory_version = published.core.directory_version,
                devices = published.core.devices.len(),
                "account directory already lists this device; adopted unchanged"
            );
            return Ok(None);
        }

        let mut core = published.core.clone();
        core.add_device(own, now_rfc3339()).inspect_err(|e| {
            tracing::warn!(error = %e, "account directory refused: this device cannot be appended");
        })?;
        self.adopt_directory(core.sign(&self.account.user_ik))?;
        tracing::info!(
            directory_version = self.directory.core.directory_version,
            devices = self.directory.core.devices.len(),
            "account directory extended with this device"
        );
        Ok(Some(self.directory.clone()))
    }

    fn create_inner(
        root: &Path,
        passphrase: &[u8],
        params: capsule_core::crypto::primitives::Argon2Params,
        device_signer: Option<Box<dyn Signer>>,
        dek_binding: Option<HardwareDekBinding>,
    ) -> Result<Self> {
        let library = crate::library::init::init_library(root, "Capsule")
            .map_err(|e| LifecycleError::Io(format!("init library: {e}")))?;
        let account = match dek_binding {
            Some(binding) => {
                tracing::info!(
                    key_alias = %binding.key_alias,
                    "workspace create: enrolling the device encryption key's classical half in the \
                     secure element"
                );
                Account::create_with_hardware_dek(
                    crate::ambient::system_ids(),
                    binding.element,
                    binding.key_alias,
                )
                .map_err(|e| {
                    // The element's own reason (unavailable / cancelled / backend) is the only
                    // thing that tells an operator whether to retry, so it is logged before it
                    // is flattened into the `&'static str` the crypto error carries.
                    tracing::error!(error = %e, "workspace create: hardware DEK enrollment failed");
                    CryptoError::Key("hardware DEK enrollment failed")
                })?
            }
            None => Account::create(crate::ambient::system_ids()),
        };
        Self::finish_create(root, library, passphrase, params, account, device_signer)
    }

    /// The tail every `create*` constructor shares once it holds an initialised `library` and an
    /// unlocked `account`: persist the account and an empty album keystore, sign the device
    /// directory, and assemble the workspace.
    fn finish_create(
        root: &Path,
        library: crate::library::Library,
        passphrase: &[u8],
        params: capsule_core::crypto::primitives::Argon2Params,
        account: Account,
        device_signer: Option<Box<dyn Signer>>,
    ) -> Result<Self> {
        let file = account.to_file_with(passphrase, params)?;
        let acct_bytes =
            cbor::to_canonical_vec(&file).map_err(|e| LifecycleError::Cbor(e.to_string()))?;
        fs::write(root.join(".library").join("account.cbor"), &acct_bytes)
            .map_err(|e| LifecycleError::Io(e.to_string()))?;

        // The album keystore exists from day one, even empty: a library whose `albums.cbor` is
        // simply absent is indistinguishable from one written before keys were durable, and
        // `open` treats that as "keys were never persisted" rather than "there are no albums
        // yet".
        AlbumStore::new().save(root, &account.master)?;

        // Default to the account's own software DSK; a hardware signer overrides it.
        let device_signer: Box<dyn Signer> =
            device_signer.unwrap_or_else(|| Box::new(account.device.dsk.clone()));
        let directory = Self::build_directory(&account, device_signer.verifying_key());
        Self::persist_directory(root, &directory)?;
        // A freshly minted `device_id` over a freshly initialised (provably empty) library has
        // provably issued no `add_id`, so it starts at zero with no sweep — the one case the
        // add-id durability rule licenses a reset. Every *re*-open goes through
        // [`open`](Self::open), which reseeds from the sidecars on disk.
        let counter = Counter::new(account.device.device_id);
        Ok(Self {
            root: root.to_path_buf(),
            account,
            device_signer,
            directory,
            client_version: capsule_core::client_build::core_client_version(),
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
            unanchored: Vec::new(),
            purged: BTreeMap::new(),
        })
    }

    /// Set the reporting client identity every manifest and derivative this workspace authors
    /// carries: `client_id` names the product (`capsule-cli`, `capsule-ios`, …) and
    /// `semver` is that client's own version. The build-embedded commit + dirty flag are appended
    /// per the [`client_version` grammar](capsule_core::client_build), so an in-repo app supplies only
    /// its id and version. Without this, a workspace reports the bare-core
    /// `capsule-core/{semver}+{commit}` default.
    #[must_use]
    pub fn with_client_id(mut self, client_id: &str, semver: &str) -> Self {
        self.client_version = capsule_core::client_build::client_version(client_id, semver);
        self
    }

    /// Open an **existing** library at `root` as a signed workspace, unlocking (or, on first use,
    /// creating + persisting) the account under `passphrase`. `params` sets the Argon2id cost for
    /// a first-time account and the share-link wrap tier.
    ///
    /// This restores the workspace's durable state rather than starting empty:
    ///
    /// - **album keys + authorities** from the sealed [`AlbumStore`], so a reopened library can
    ///   decrypt and keep writing into the albums it already has;
    /// - **every managed asset** from its signed artifacts on disk (chain, sidecar, sealed
    ///   metadata blob), a `warn`-and-skip per asset rather than a failed open;
    /// - **the `add_id` counter**, reseeded past everything this device has ever written;
    /// - **the signed device directory** from `{root}/.library/directory.cbor` (#562), once it
    ///   verifies under the user IK and lists this device live (re-signed under this account's
    ///   id at the next version when it still names the id from before a rebind); otherwise a
    ///   fresh version-1 directory listing only this device, with a `warn` naming why.
    ///
    /// A library with no `albums.cbor` predates this and opens with zero albums plus a `warn`
    /// naming backup restore — see [`AlbumStore::load`].
    ///
    /// A library holding sidecars **no provenance chain anchors** — an interrupted write, a
    /// torn or foreign file, a retired unsigned pre-signed-path record — still opens: those
    /// assets are not restored, but each is reported through
    /// [`unanchored_sidecars`](Self::unanchored_sidecars) with a `warn`, and never touched.
    ///
    /// Still session-scoped by design, and dropped on close: the federation group assertions
    /// (re-delivered by the feed), the pending guest-drop inbox (server-authoritative), and the
    /// issued share/upload link records — see the [`Workspace`] fields for why each is deferred.
    pub fn open(
        root: &Path,
        passphrase: &[u8],
        params: capsule_core::crypto::primitives::Argon2Params,
    ) -> Result<Self> {
        Self::open_inner(root, passphrase, params, None)
    }

    /// As [`open`](Self::open), re-attaching the secure element that holds this device's
    /// hardware-bound DEK. Required for a library created by
    /// [`create_with_hardware_dek`](Self::create_with_hardware_dek): its DEK's classical half is
    /// non-exportable, so plain [`open`](Self::open) refuses it rather than degrading to software.
    ///
    /// Safe to call for a software-DEK library too — the binding recorded in the account file
    /// decides, and a software account simply ignores the element. On a **first** open of an
    /// empty library (no account yet) the fresh account is created hardware-bound under
    /// `binding.key_alias`.
    pub fn open_with_hardware_dek(
        root: &Path,
        passphrase: &[u8],
        params: capsule_core::crypto::primitives::Argon2Params,
        binding: HardwareDekBinding,
    ) -> Result<Self> {
        Self::open_inner(root, passphrase, params, Some(binding))
    }

    fn open_inner(
        root: &Path,
        passphrase: &[u8],
        params: capsule_core::crypto::primitives::Argon2Params,
        dek_binding: Option<HardwareDekBinding>,
    ) -> Result<Self> {
        let library = crate::library::open_library(root)?;
        let account_path = root.join(".library").join("account.cbor");
        let account = if account_path.exists() {
            let bytes = fs::read(&account_path).map_err(|e| LifecycleError::Io(e.to_string()))?;
            let file: AccountFile =
                cbor::from_slice(&bytes).map_err(|e| LifecycleError::Cbor(e.to_string()))?;
            file.unlock_with_element(passphrase, dek_binding.map(|b| b.element))?
        } else {
            let account = match dek_binding {
                Some(binding) => Account::create_with_hardware_dek(
                    crate::ambient::system_ids(),
                    binding.element,
                    binding.key_alias,
                )
                .map_err(|e| {
                    tracing::error!(error = %e, "workspace open: hardware DEK enrollment failed");
                    CryptoError::Key("hardware DEK enrollment failed")
                })?,
                None => Account::create(crate::ambient::system_ids()),
            };
            let file = account.to_file_with(passphrase, params)?;
            let acct_bytes =
                cbor::to_canonical_vec(&file).map_err(|e| LifecycleError::Cbor(e.to_string()))?;
            fs::write(&account_path, &acct_bytes).map_err(|e| LifecycleError::Io(e.to_string()))?;
            // First use of this library: seed an empty album keystore alongside the account, so
            // the next open sees "no albums yet" rather than the keys-never-persisted migration
            // case.
            AlbumStore::new().save(root, &account.master)?;
            account
        };

        let device_signer: Box<dyn Signer> = Box::new(account.device.dsk.clone());
        let dsk_public = device_signer.verifying_key();
        let built = Self::build_directory(&account, dsk_public.clone());
        // #562: the directory this device last signed — cross-signed devices, a rebound id, an
        // extension of the account's published one — survives the close.
        let directory = Self::restore_directory(root, &account, &dsk_public, built);
        let user_id = account.user_id;
        // Reseed the `add_id` counter to one past the maximum this device has ever
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
            client_version: capsule_core::client_build::core_client_version(),
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
            unanchored: Vec::new(),
            purged: BTreeMap::new(),
        };
        // Album keys, authorities, and every managed asset come back from disk here.
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

    /// Restore the durable half of the workspace after unlocking the account:
    /// album key material + authorities from `{root}/.library/albums.cbor`, then every managed
    /// asset's in-memory state from the artifacts on disk.
    ///
    /// **Migration.** A library with no `albums.cbor` predates durable album keys: its keys
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
                "no album keystore: this library predates durable album keys, so it opens \
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
    /// recovery-first reason [`rebuild_index`](crate::library::rebuild_index) exists: an
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
        let mut purged = 0usize;
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
                // A purge keeps the chain, ends it with a `delete`, and removes the sidecar and
                // the original (`Workspace::purge`): a chain headed by a `delete` with no
                // sidecar beside it is that record, not damage. Anything else falls through to
                // the warning below.
                Err(e) => {
                    if let Some((asset_id, head)) = purged_head(path, stem) {
                        purged += 1;
                        tracing::trace!(provenance = %path.display(), %asset_id, restore_error = %e, "workspace open: purged asset; only its provenance chain is kept");
                        self.purged.insert(asset_id, head);
                    } else {
                        skipped += 1;
                        tracing::warn!(
                            provenance = %path.display(),
                            error = %e,
                            "workspace open: could not restore this asset; skipping it"
                        );
                    }
                }
            }
        }
        tracing::info!(
            restored,
            skipped,
            purged,
            "workspace open: assets restored from disk"
        );

        // Second pass: the sidecars no chain anchors. A signed sidecar without one is a write
        // interrupted before its chain landed; anything else is torn, foreign, or a retired
        // unsigned record. None is restorable here — open must not author signed records
        // unasked — so each is recorded and named, never silently dropped.
        self.unanchored = find_unanchored(&self.root);
        for found in &self.unanchored {
            tracing::warn!(
                sidecar = %found.path.display(),
                asset_id = ?found.asset_id,
                shape = ?found.shape,
                "workspace open: sidecar with no provenance chain; the asset is invisible"
            );
        }
        if !self.unanchored.is_empty() {
            tracing::info!(
                unanchored = self.unanchored.len(),
                "workspace open: sidecars with no provenance chain found; left untouched"
            );
        }
    }

    fn restore_one_asset(&self, provenance_path: &Path, stem: &str) -> Result<AssetState> {
        let asset_id = Uuid::parse_str(stem)
            .map_err(|e| LifecycleError::NotFound(format!("asset id in {stem}: {e}")))?;
        let dir = provenance_path
            .parent()
            .ok_or_else(|| LifecycleError::Io("provenance file has no parent".into()))?;

        // (1) The provenance chain — the anchor.
        let chain = read_chain(provenance_path)?;
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

        // (5) The sealed metadata blob. Absence is tolerated: libraries written before album
        // keys were durable never persisted it. Such an asset reads and verifies fine; only
        // `export_backup` and upload need the blob, and both fail loudly on the empty value
        // rather than shipping a wrong one.
        let blob_path = dir.join(format!("{}.metadata.bin", asset_id.simple()));
        let metadata_blob = match fs::read(&blob_path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    asset_id = %asset_id,
                    "no sealed metadata blob on disk (asset written before sealed metadata existed); it cannot be exported or \
                     uploaded until its metadata is rewritten"
                );
                Vec::new()
            }
            Err(e) => return Err(LifecycleError::Io(format!("metadata blob: {e}"))),
        };

        // (6) Stack placement. The durable record is the sidecar's signed
        // `stack_membership` register, so a written register is authoritative — `Some` is a
        // placement, a stamped `None` an explicit departure from a stack. Reading the index is
        // the compatibility path for an asset imported before that register existed, whose
        // placement was written only as `assets.stack_id` / `is_stack_hidden` and exists
        // nowhere else.
        let stack = match sidecar.stack_membership.get() {
            Some(membership) => membership.as_ref().map(StackPlacement::from_membership),
            None => self
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
                }),
        };

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
                // The published half of this device's encryption key, so a peer can encapsulate
                // to it using the directory alone. `public_bytes()` is the single seam for both
                // compositions and the recipient tells them apart by length, which is why
                // nothing here says which one it is holding.
                dek_public: Some(DekPublic(account.device.dek.public_bytes())),
                added_at: DEVICE_ADDED_AT.into(),
                revoked_at: None,
                unknown: Default::default(),
            }],
            unknown: Default::default(),
        }
        .sign(&account.user_ik)
    }

    /// Where this library keeps its signed device directory: beside `account.cbor` (#562).
    pub(super) fn directory_path(root: &Path) -> PathBuf {
        root.join(".library").join(DIRECTORY_FILE)
    }

    /// Write `directory` to [`directory_path`](Self::directory_path), temp-then-rename like
    /// `account.cbor`, so a crash mid-write leaves the previous directory intact rather than a
    /// truncated one that would no longer verify.
    fn persist_directory(root: &Path, directory: &DeviceDirectory) -> Result<()> {
        let path = Self::directory_path(root);
        let bytes =
            cbor::to_canonical_vec(directory).map_err(|e| LifecycleError::Cbor(e.to_string()))?;
        let tmp = crate::utils::paths::tmp_path(&path);
        fs::write(&tmp, &bytes).map_err(|e| LifecycleError::Io(e.to_string()))?;
        fs::rename(&tmp, &path).map_err(|e| LifecycleError::Io(e.to_string()))?;
        tracing::debug!(
            directory_version = directory.core.directory_version,
            devices = directory.core.devices.len(),
            "device directory persisted"
        );
        Ok(())
    }

    /// Whether the directory file at `root` already holds exactly `directory`.
    fn persisted_directory_is(root: &Path, directory: &DeviceDirectory) -> bool {
        let Ok(on_disk) = fs::read(Self::directory_path(root)) else {
            return false;
        };
        cbor::to_canonical_vec(directory).is_ok_and(|bytes| bytes == on_disk)
    }

    /// Persist `directory` and only then adopt it as this workspace's directory, so a failed
    /// write leaves memory and disk agreeing on the previous one.
    pub(super) fn adopt_directory(&mut self, directory: DeviceDirectory) -> Result<()> {
        Self::persist_directory(&self.root, &directory).inspect_err(|e| {
            tracing::warn!(error = %e, "device directory not adopted: persisting it failed");
        })?;
        self.directory = directory;
        Ok(())
    }

    /// The directory a reopened workspace starts from (#562): the persisted one when it is
    /// trustworthy and at least as new as `built` (this device's freshly signed version-1
    /// directory), otherwise `built`.
    ///
    /// The persisted file is trusted only when it verifies under the account's user IK and lists
    /// this device live under the signing key it signs with. One that names another account id
    /// under this account's own IK is the directory a failed [`bind_account_id`](Self::bind_account_id)
    /// left behind, and is re-signed under the bound id at the next version. Anything else —
    /// missing (a library written before #562), unreadable, tampered, foreign, or one that
    /// revoked this device — falls back to `built` with a log line naming why, the behaviour
    /// every reopen had before the directory was persisted. The fallback is not written back,
    /// so the refused file stays on disk for inspection until the next directory change.
    fn restore_directory(
        root: &Path,
        account: &Account,
        dsk_public: &HybridVerifyingKey,
        built: DeviceDirectory,
    ) -> DeviceDirectory {
        let path = Self::directory_path(root);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!("workspace open: no persisted device directory; using a fresh one");
                return built;
            }
            Err(e) => {
                tracing::warn!(error = %e, "workspace open: persisted device directory unreadable; using a fresh one");
                return built;
            }
        };
        let persisted: DeviceDirectory = match cbor::from_slice(&bytes) {
            Ok(directory) => directory,
            Err(e) => {
                tracing::warn!(error = %e, "workspace open: persisted device directory does not decode; using a fresh one");
                return built;
            }
        };
        if let Some(reason) = Self::refuse_persisted_directory(&persisted, account, dsk_public) {
            tracing::warn!(
                reason,
                persisted_version = persisted.core.directory_version,
                "workspace open: persisted device directory refused; using a fresh one"
            );
            return built;
        }
        let persisted = if persisted.core.user_id == account.user_id {
            persisted
        } else {
            // Signed by this account's own IK yet naming another id: the directory from before a
            // `bind_account_id` whose directory write never landed (the account file is the
            // authority on the id). Re-sign it under the bound id at the next version, as that
            // bind did, rather than dropping every device it lists for a version-1 rebuild.
            tracing::warn!(
                persisted_version = persisted.core.directory_version,
                from = %persisted.core.user_id,
                to = %account.user_id,
                "workspace open: persisted device directory names the pre-rebind account id; re-signing it under the bound id"
            );
            let mut core = persisted.core;
            core.user_id = account.user_id;
            core.directory_version += 1;
            core.updated_at = now_rfc3339();
            core.sign(&account.user_ik)
        };
        if persisted.core.directory_version < built.core.directory_version {
            tracing::warn!(
                persisted_version = persisted.core.directory_version,
                "workspace open: persisted device directory is older than a fresh one; using the fresh one"
            );
            return built;
        }
        tracing::info!(
            directory_version = persisted.core.directory_version,
            devices = persisted.core.devices.len(),
            "workspace open: persisted device directory restored"
        );
        persisted
    }

    /// Why a persisted directory may not be adopted on open, or `None` when it may.
    fn refuse_persisted_directory(
        persisted: &DeviceDirectory,
        account: &Account,
        dsk_public: &HybridVerifyingKey,
    ) -> Option<&'static str> {
        if !persisted.verify(&account.user_ik.verifying_key()) {
            return Some("it does not verify under the user identity key");
        }
        match persisted.device(&account.device.device_id) {
            None => Some("it does not list this device"),
            Some(entry) if entry.dsk_public != *dsk_public => {
                Some("it lists this device under another signing key")
            }
            Some(entry) if entry.revoked_at.is_some() => Some("it lists this device as revoked"),
            Some(_) => None,
        }
    }
}

#[cfg(test)]
mod tests;
