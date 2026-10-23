//! Workspace construction and opening: account create/unlock, device-directory publication,
//! and the injected client / still-encoder seams.

use std::collections::HashMap;
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
use capsule_core::crypto::provenance::{ProvenanceChain, ProvenanceRecord};
use capsule_core::metadata::crdt::Counter;
use capsule_core::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use uuid::Uuid;
use walkdir::WalkDir;

use super::migrate_unsigned::find_unanchored;
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
                Account::create_with_hardware_dek(binding.element, binding.key_alias).map_err(
                    |e| {
                        // The element's own reason (unavailable / cancelled / backend) is the only
                        // thing that tells an operator whether to retry, so it is logged before it
                        // is flattened into the `&'static str` the crypto error carries.
                        tracing::error!(error = %e, "workspace create: hardware DEK enrollment failed");
                        CryptoError::Key("hardware DEK enrollment failed")
                    },
                )?
            }
            None => Account::create(),
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
            unmigrated: Vec::new(),
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
    /// A library holding **unsigned pre-signed-path sidecars** still opens: those
    /// assets have no provenance chain, so they are not restored, but each is reported through
    /// [`unmigrated_sidecars`](Self::unmigrated_sidecars) with a `warn` naming
    /// [`migrate_unsigned_sidecars`](Self::migrate_unsigned_sidecars) as the way in.
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
                    binding.element,
                    binding.key_alias,
                )
                .map_err(|e| {
                    tracing::error!(error = %e, "workspace open: hardware DEK enrollment failed");
                    CryptoError::Key("hardware DEK enrollment failed")
                })?,
                None => Account::create(),
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
            unmigrated: Vec::new(),
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

        // Second pass: the sidecars no chain anchors. An unsigned pre-signed-path
        // record has never had one; a signed sidecar without one is an interrupted migration.
        // Neither is restorable here — open holds no album write capability and must not author
        // signed records unasked — so each is recorded and named, never silently dropped.
        self.unmigrated = find_unanchored(&self.root);
        for found in &self.unmigrated {
            tracing::warn!(
                sidecar = %found.path.display(),
                asset_id = ?found.asset_id,
                shape = ?found.shape,
                "workspace open: sidecar with no provenance chain; the asset is invisible until \
                 `Workspace::migrate_unsigned_sidecars` runs"
            );
        }
        if !self.unmigrated.is_empty() {
            tracing::info!(
                unmigrated = self.unmigrated.len(),
                "workspace open: unmigrated sidecars found; run \
                 `Workspace::migrate_unsigned_sidecars` to admit them as signed assets"
            );
        }
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
mod tests {
    use capsule_core::crypto::keys::encapsulate_to_p256_public;
    use capsule_core::crypto::primitives::Argon2Params;
    use capsule_core::crypto::verify_asset::VerifyOutcome;
    use tempfile::TempDir;

    use super::super::{FAST_PARAMS, fast_workspace};
    use super::*;

    /// **The core claim.** An asset imported in one session is fully usable in the next:
    /// its album key comes back from the sealed keystore, so the workspace can re-derive the file
    /// key (proved by `verify_asset` accepting, which regenerates the ciphertext and re-checks its
    /// content address) and can open the sealed metadata blob it persisted to disk.
    #[test]
    fn reopened_workspace_decrypts_an_asset_imported_before_close() {
        use capsule_core::crypto::encryption::{blob_nonce, open_blob};
        use capsule_core::crypto::keys::Amk;

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

        let ws2 = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();

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

    /// A reopened library keeps *writing* into the album it already has — the write-tier
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

        let mut ws2 = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
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

    /// The authority — not just the content key — is durable: the admin-signed epoch
    /// ledger comes back verifying, with the epoch ceiling and per-epoch write-tier keys intact,
    /// and its local-only `amk_present` flags restored from the epochs actually held.
    #[test]
    fn reopened_workspace_restores_the_reference_authority_ledger() {
        use capsule_core::crypto::authority::AlbumAuthority;
        use capsule_core::crypto::keys::AmkVersion;

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

        let ws2 = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
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

    /// **Migration.** A library written before durable album keys has no `albums.cbor`. It
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

        // Simulate the on-disk shape before album keys were durable: signed artifacts present,
        // keystore absent.
        let store = capsule_core::crypto::keys::AlbumStore::path(lib.path());
        assert!(store.exists());
        fs::remove_file(&store).unwrap();

        let ws2 = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
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

    /// `export_backup` reads each asset's album AMK *and* its sealed metadata blob, so
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
        let ws2 = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
        let archive = src.path().join("backup.tar");
        ws2.export_backup_with_params(&archive, b"recovery-pass", FAST_PARAMS)
            .unwrap();
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
        ws3.export_backup_with_params(&again, b"pass-two", FAST_PARAMS)
            .unwrap();
        assert!(again.exists());

        // And those recovered keys are durable in ws3 too: reopening it keeps them.
        drop(ws3);
        let ws4 = Workspace::open(fresh.path(), b"passphrase", FAST_PARAMS).unwrap();
        assert!(
            ws4.has_album(&album),
            "recovered AMKs were persisted, not just held for the session"
        );
        assert_eq!(ws4.read_plaintext(&id).unwrap(), bytes);
    }

    /// An album recovered purely from a backup artifact holds content keys and no
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
            ws.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
                .unwrap();
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

    /// `create_album_with_id` refuses to mint over an album that already holds key
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

    /// **#467.** A library created from a recovered master key, the account id, and the sealed
    /// identity *is* that account on a new device: the same id, default album and identity key,
    /// a fresh device, a directory that verifies under the account's IK — and all of it survives
    /// a reopen under the new device's passphrase.
    #[test]
    fn a_library_created_from_the_master_key_opens_as_the_account() {
        use capsule_core::crypto::keys::MasterKey;

        let lib_a = TempDir::new().unwrap();
        let lib_b = TempDir::new().unwrap();
        let a = fast_workspace(lib_a.path());
        let master = MasterKey::from_bytes(*a.account.master.as_bytes());

        let b = Workspace::create_from_master(
            lib_b.path(),
            b"device b passphrase",
            FAST_PARAMS,
            master,
            a.user_id(),
            &a.sealed_identity(),
        )
        .unwrap();
        assert_eq!(b.user_id(), a.user_id());
        assert_eq!(b.default_album_id(), a.default_album_id());
        assert_eq!(b.user_ik_public(), a.user_ik_public());
        assert_ne!(
            b.device_id(),
            a.device_id(),
            "a recovered device is a new device"
        );

        let directory = b.device_directory();
        assert!(directory.verify(&a.user_ik_public()));
        assert_eq!(directory.core.user_id, a.user_id());
        assert!(directory.device(&b.device_id()).is_some());

        let device_b = b.device_id();
        drop(b);
        let reopened = Workspace::open(lib_b.path(), b"device b passphrase", FAST_PARAMS).unwrap();
        assert_eq!(reopened.user_id(), a.user_id());
        assert_eq!(reopened.device_id(), device_b);
        assert_eq!(reopened.user_ik_public(), a.user_ik_public());
    }

    /// **#467.** A master key that did not seal the identity is refused before anything is
    /// written: the sealed identity is also the proof that the master key is the account's.
    #[test]
    fn creating_from_a_foreign_master_key_is_refused_and_writes_nothing() {
        use capsule_core::crypto::keys::MasterKey;

        let lib_a = TempDir::new().unwrap();
        let lib_b = TempDir::new().unwrap();
        let a = fast_workspace(lib_a.path());
        let root_b = lib_b.path().join("library");

        let result = Workspace::create_from_master(
            &root_b,
            b"pw",
            FAST_PARAMS,
            MasterKey::generate(),
            a.user_id(),
            &a.sealed_identity(),
        );
        assert!(matches!(
            result,
            Err(LifecycleError::Crypto(CryptoError::Auth(_)))
        ));
        assert!(
            !root_b.exists(),
            "no library is initialised for a refused key"
        );
    }

    /// Account A with a second device cross-signed in (so its directory is past version 1 and
    /// lists more than one device), plus a library recovered onto a new device of that account.
    fn account_with_two_devices_and_a_recovered_one(
        lib_a: &Path,
        lib_b: &Path,
    ) -> (Workspace, Workspace, DeviceEntry) {
        use capsule_core::crypto::keys::{HybridSigningKey, MasterKey};

        let mut a = fast_workspace(lib_a);
        let other = DeviceEntry {
            device_id: Uuid::now_v7(),
            dsk_public: HybridSigningKey::generate().verifying_key(),
            dek_public: None,
            added_at: DEVICE_ADDED_AT.into(),
            revoked_at: None,
            unknown: Default::default(),
        };
        a.cross_sign_device(other.clone()).unwrap();
        let master = MasterKey::from_bytes(*a.account.master.as_bytes());
        let b = Workspace::create_from_master(
            lib_b,
            b"device b passphrase",
            FAST_PARAMS,
            master,
            a.user_id(),
            &a.sealed_identity(),
        )
        .unwrap();
        (a, b, other)
    }

    /// **#550.** A recovered device builds on the account's published directory: it keeps every
    /// device already listed, appends its own entry at `stored + 1`, and signs under the account's
    /// IK — so the result is both fresh (the server's stale-version check passes) and complete.
    #[test]
    fn a_recovered_device_extends_the_published_directory_at_the_next_version() {
        let lib_a = TempDir::new().unwrap();
        let lib_b = TempDir::new().unwrap();
        let (a, mut b, other) =
            account_with_two_devices_and_a_recovered_one(lib_a.path(), lib_b.path());
        let published = a.device_directory().clone();
        assert!(published.core.directory_version >= 1);
        assert_eq!(
            b.device_directory().core.directory_version,
            1,
            "before extending, B holds only the version-1 directory naming itself"
        );

        let extended = b
            .extend_account_directory(&published)
            .unwrap()
            .expect("B is not yet listed, so there is a new version to publish");
        assert_eq!(
            extended.core.directory_version,
            published.core.directory_version + 1
        );
        assert!(
            extended.verify(&a.user_ik_public()),
            "signed under the account's IK"
        );
        assert_eq!(extended.core.user_id, a.user_id());
        assert_eq!(extended.core.devices.len(), 3);
        assert_eq!(
            extended.device(&a.device_id()),
            published.device(&a.device_id())
        );
        assert_eq!(extended.device(&other.device_id), Some(&other));
        let own = extended.device(&b.device_id()).expect("B lists itself");
        assert!(own.dek_public.is_some(), "B's entry carries its DEK");
        assert_eq!(
            b.device_directory(),
            &extended,
            "B adopted what it returned"
        );
    }

    /// **#550.** Extending again from a directory that already lists this device under its own
    /// key adopts it unchanged and has nothing to publish — the reopened-recovered-device case.
    #[test]
    fn extending_a_directory_that_already_lists_this_device_adopts_it_unchanged() {
        let lib_a = TempDir::new().unwrap();
        let lib_b = TempDir::new().unwrap();
        let (a, mut b, _) =
            account_with_two_devices_and_a_recovered_one(lib_a.path(), lib_b.path());
        let extended = b
            .extend_account_directory(a.device_directory())
            .unwrap()
            .unwrap();

        let device_b = b.device_id();
        drop(b);
        let mut reopened =
            Workspace::open(lib_b.path(), b"device b passphrase", FAST_PARAMS).unwrap();
        assert_eq!(reopened.device_id(), device_b);
        assert_eq!(
            reopened.device_directory(),
            &extended,
            "#562: the extended directory is restored, not rebuilt at version 1"
        );
        assert_eq!(reopened.extend_account_directory(&extended).unwrap(), None);
        assert_eq!(reopened.device_directory(), &extended);
    }

    /// **#550.** A directory that does not verify under the account's IK, or that names another
    /// account, is refused and nothing is adopted: building on it would let whoever served it
    /// choose the devices this account trusts.
    #[test]
    fn extending_an_untrusted_directory_is_refused_and_changes_nothing() {
        let lib_a = TempDir::new().unwrap();
        let lib_b = TempDir::new().unwrap();
        let lib_x = TempDir::new().unwrap();
        let (a, mut b, _) =
            account_with_two_devices_and_a_recovered_one(lib_a.path(), lib_b.path());
        let before = b.device_directory().clone();

        // Another account's directory, signed under its own IK.
        let foreign = fast_workspace(lib_x.path()).device_directory().clone();
        assert!(matches!(
            b.extend_account_directory(&foreign),
            Err(LifecycleError::UntrustedDirectory(_))
        ));

        // A's directory with its core tampered after signing.
        let mut tampered = a.device_directory().clone();
        tampered.core.devices.pop();
        assert!(matches!(
            b.extend_account_directory(&tampered),
            Err(LifecycleError::UntrustedDirectory(_))
        ));

        // Correctly signed under the account's IK, but naming another account id.
        let mut core = a.device_directory().core.clone();
        core.user_id = Uuid::now_v7();
        let misnamed = core.sign(&a.account.user_ik);
        assert!(matches!(
            b.extend_account_directory(&misnamed),
            Err(LifecycleError::UntrustedDirectory(_))
        ));

        assert_eq!(b.device_directory(), &before, "nothing was adopted");
    }

    /// **#550.** A published directory that lists this device's id under a different signing key
    /// is a conflict, not something to paper over: refused, nothing adopted.
    #[test]
    fn extending_a_directory_listing_this_device_under_another_key_is_refused() {
        use capsule_core::crypto::keys::{DirectoryError, HybridSigningKey};

        let lib_a = TempDir::new().unwrap();
        let lib_b = TempDir::new().unwrap();
        let (mut a, mut b, _) =
            account_with_two_devices_and_a_recovered_one(lib_a.path(), lib_b.path());
        let impostor = DeviceEntry {
            device_id: b.device_id(),
            dsk_public: HybridSigningKey::generate().verifying_key(),
            dek_public: None,
            added_at: DEVICE_ADDED_AT.into(),
            revoked_at: None,
            unknown: Default::default(),
        };
        let published = a.cross_sign_device(impostor).unwrap();
        let before = b.device_directory().clone();

        assert!(matches!(
            b.extend_account_directory(&published),
            Err(LifecycleError::Directory(DirectoryError::DeviceAlreadyListed(id))) if id == b.device_id()
        ));
        assert_eq!(b.device_directory(), &before);
    }

    /// **#550.** A published directory that lists this device's id under its own signing key but
    /// **revoked** is refused, not adopted: a revoked device must not re-admit itself by
    /// extending the directory that revoked it, nor treat that directory as already listing it.
    #[test]
    fn extending_a_directory_listing_this_device_revoked_is_refused() {
        use capsule_core::crypto::keys::DirectoryError;

        let lib_a = TempDir::new().unwrap();
        let lib_b = TempDir::new().unwrap();
        let (a, mut b, _) =
            account_with_two_devices_and_a_recovered_one(lib_a.path(), lib_b.path());
        let mut revoked = b
            .device_directory()
            .device(&b.device_id())
            .cloned()
            .expect("B lists itself");
        revoked.revoked_at = Some(DEVICE_ADDED_AT.into());
        // `cross_sign_device` refuses a revoked entry, so the account signs one in directly, as a
        // directory that later revoked B would read.
        let mut core = a.device_directory().core.clone();
        core.devices.push(revoked);
        core.directory_version += 1;
        let published = core.sign(&a.account.user_ik);
        assert!(published.verify(&b.user_ik_public()));
        let before = b.device_directory().clone();

        assert!(matches!(
            b.extend_account_directory(&published),
            Err(LifecycleError::Directory(DirectoryError::DeviceAlreadyListed(id))) if id == b.device_id()
        ));
        assert_eq!(b.device_directory(), &before, "nothing was adopted");
    }

    /// **#467.** A fresh library adopts the server's account id: the account file, the in-memory
    /// account, and a re-signed directory at the next version all name it, and a reopen keeps it.
    #[test]
    fn binding_the_server_account_id_persists_and_re_signs_the_directory() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let before = ws.device_directory().core.directory_version;
        let server_id = Uuid::now_v7();

        ws.bind_account_id(server_id).unwrap();
        assert_eq!(ws.user_id(), server_id);
        let account_path = lib.path().join(".library").join("account.cbor");
        assert!(
            !crate::utils::paths::tmp_path(&account_path).exists(),
            "the staged account file was renamed into place, not left beside it"
        );
        let directory = ws.device_directory();
        assert_eq!(directory.core.user_id, server_id);
        assert_eq!(directory.core.directory_version, before + 1);
        assert!(directory.verify(&ws.user_ik_public()));
        assert!(directory.device(&ws.device_id()).is_some());

        // Binding the same id again changes nothing.
        ws.bind_account_id(server_id).unwrap();
        assert_eq!(ws.device_directory().core.directory_version, before + 1);

        drop(ws);
        let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
        assert_eq!(reopened.user_id(), server_id);
        assert_eq!(reopened.device_directory().core.user_id, server_id);
        assert_eq!(
            reopened.device_directory().core.directory_version,
            before + 1,
            "#562: the bumped version survives the reopen"
        );
    }

    /// A workspace at `lib` that has cross-signed one other device, so its directory is at
    /// version 2 and lists two devices.
    fn workspace_with_a_cross_signed_device(lib: &Path) -> Workspace {
        use capsule_core::crypto::keys::HybridSigningKey;

        let mut ws = fast_workspace(lib);
        ws.cross_sign_device(DeviceEntry {
            device_id: Uuid::now_v7(),
            dsk_public: HybridSigningKey::generate().verifying_key(),
            dek_public: None,
            added_at: DEVICE_ADDED_AT.into(),
            revoked_at: None,
            unknown: Default::default(),
        })
        .unwrap();
        assert_eq!(ws.device_directory().core.directory_version, 2);
        ws
    }

    /// **#562.** A bind whose directory write fails still binds (the account file names the new
    /// id), and a retry with the same id is what writes the re-signed directory, so a reopen
    /// restores it with every device it lists rather than rebuilding at version 1.
    #[test]
    fn a_bind_retry_persists_the_directory_an_earlier_bind_failed_to_write() {
        let lib = TempDir::new().unwrap();
        let mut ws = workspace_with_a_cross_signed_device(lib.path());
        let server_id = Uuid::now_v7();
        // A directory where the file belongs makes the rename fail.
        let directory_path = Workspace::directory_path(lib.path());
        fs::remove_file(&directory_path).unwrap();
        fs::create_dir(&directory_path).unwrap();

        let err = ws.bind_account_id(server_id).unwrap_err();
        assert!(matches!(err, LifecycleError::Io(_)), "{err:?}");
        assert_eq!(ws.user_id(), server_id, "the account file was rewritten");
        let bound = ws.device_directory().clone();
        assert_eq!(bound.core.user_id, server_id);
        assert_eq!(bound.core.directory_version, 3);

        fs::remove_dir(&directory_path).unwrap();
        ws.bind_account_id(server_id).unwrap();
        assert_eq!(ws.device_directory(), &bound, "the retry re-signs nothing");
        drop(ws);

        let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
        assert_eq!(reopened.device_directory(), &bound);
        assert_eq!(reopened.device_directory().core.devices.len(), 2);
    }

    /// **#562.** A bind whose directory write never landed, and that nobody retried, leaves the
    /// pre-rebind directory on disk: signed by this account's IK but naming the old id. The
    /// reopen re-signs it under the bound id at the next version and keeps every device it
    /// lists, instead of a version-1 rebuild that is stale against what was published.
    #[test]
    fn a_directory_naming_the_pre_rebind_id_is_re_signed_on_open() {
        let lib = TempDir::new().unwrap();
        let mut ws = workspace_with_a_cross_signed_device(lib.path());
        let pre_rebind = ws.device_directory().clone();
        let server_id = Uuid::now_v7();
        ws.bind_account_id(server_id).unwrap();
        let user_ik = ws.user_ik_public();
        drop(ws);
        // As though the bind's directory write had failed after the account file was rewritten.
        plant_directory(lib.path(), &pre_rebind);

        let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
        let opened = reopened.device_directory();
        assert_eq!(opened.core.user_id, server_id);
        assert_eq!(opened.core.directory_version, 3);
        assert_eq!(opened.core.devices, pre_rebind.core.devices);
        assert!(opened.verify(&user_ik));
        let on_disk: DeviceDirectory =
            cbor::from_slice(&fs::read(Workspace::directory_path(lib.path())).unwrap()).unwrap();
        assert_eq!(on_disk, pre_rebind, "the open writes nothing back");
    }

    /// Replace the persisted directory of the library at `lib` with `directory`.
    fn plant_directory(lib: &Path, directory: &DeviceDirectory) {
        fs::write(
            Workspace::directory_path(lib),
            cbor::to_canonical_vec(directory).unwrap(),
        )
        .unwrap();
    }

    /// **#562.** A library written before the directory was persisted has no `directory.cbor`:
    /// it opens with a fresh version-1 directory listing only this device, as before.
    #[test]
    fn a_library_without_a_persisted_directory_opens_with_a_fresh_one() {
        let lib = TempDir::new().unwrap();
        let ws = fast_workspace(lib.path());
        let device = ws.device_id();
        drop(ws);
        fs::remove_file(Workspace::directory_path(lib.path())).unwrap();

        let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
        let directory = reopened.device_directory();
        assert_eq!(directory.core.directory_version, 1);
        assert_eq!(directory.core.devices.len(), 1);
        assert!(directory.device(&device).is_some());
        assert!(directory.verify(&reopened.user_ik_public()));
    }

    /// **#562.** A persisted directory that is undecodable, tampered, foreign, or that does not
    /// list this device live under its own key is never adopted: the reopen falls back to a
    /// fresh version-1 directory, and the refused file is left where it was.
    #[test]
    fn an_untrustworthy_persisted_directory_is_refused_on_open() {
        use capsule_core::crypto::keys::HybridSigningKey;

        let lib = TempDir::new().unwrap();
        let lib_x = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        ws.cross_sign_device(DeviceEntry {
            device_id: Uuid::now_v7(),
            dsk_public: HybridSigningKey::generate().verifying_key(),
            dek_public: None,
            added_at: DEVICE_ADDED_AT.into(),
            revoked_at: None,
            unknown: Default::default(),
        })
        .unwrap();
        let genuine = ws.device_directory().clone();
        assert_eq!(genuine.core.directory_version, 2);
        let device = ws.device_id();
        let ik = ws.account.user_ik.clone();
        drop(ws);

        let mut tampered = genuine.clone();
        tampered.core.directory_version += 1;

        let foreign = fast_workspace(lib_x.path()).device_directory().clone();

        let mut without_me = genuine.core.clone();
        without_me.devices.retain(|d| d.device_id != device);
        let without_me = without_me.sign(&ik);

        let mut rekeyed = genuine.core.clone();
        for d in &mut rekeyed.devices {
            if d.device_id == device {
                d.dsk_public = HybridSigningKey::generate().verifying_key();
            }
        }
        let rekeyed = rekeyed.sign(&ik);

        let mut revoked = genuine.core.clone();
        for d in &mut revoked.devices {
            if d.device_id == device {
                d.revoked_at = Some(DEVICE_ADDED_AT.into());
            }
        }
        let revoked = revoked.sign(&ik);

        let mut older = genuine.core.clone();
        older.directory_version = 0;
        let older = older.sign(&ik);

        let refused = [
            ("older than a fresh one", older),
            ("tampered", tampered),
            ("foreign", foreign),
            ("without this device", without_me),
            ("this device rekeyed", rekeyed),
            ("this device revoked", revoked),
        ];
        for (case, directory) in &refused {
            plant_directory(lib.path(), directory);
            let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
            let opened = reopened.device_directory();
            assert_eq!(opened.core.directory_version, 1, "{case}");
            assert_eq!(opened.core.devices.len(), 1, "{case}");
            assert!(opened.device(&device).is_some(), "{case}");
            let on_disk: DeviceDirectory =
                cbor::from_slice(&fs::read(Workspace::directory_path(lib.path())).unwrap())
                    .unwrap();
            assert_eq!(
                &on_disk, directory,
                "{case}: the refused file is left in place"
            );
        }

        fs::write(Workspace::directory_path(lib.path()), b"not cbor").unwrap();
        let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
        assert_eq!(reopened.device_directory().core.directory_version, 1);
        drop(reopened);

        // And the genuine one is restored once it is back.
        plant_directory(lib.path(), &genuine);
        let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
        assert_eq!(reopened.device_directory(), &genuine);
    }

    /// **#467.** Once an asset's manifest names the library's id, rebinding is refused — and the
    /// refusal changes nothing, on disk or in memory.
    #[test]
    fn rebinding_after_an_import_is_refused() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(&img, b"\xFF\xD8\xFF authored before the rebind").unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();
        let asset = ws.import_asset(album, &img).unwrap();
        let original = ws.user_id();

        assert!(matches!(
            ws.bind_account_id(Uuid::now_v7()),
            Err(LifecycleError::AccountRebind(_))
        ));
        assert_eq!(ws.user_id(), original);
        assert_eq!(ws.device_directory().core.user_id, original);
        assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
        drop(ws);
        let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
        assert_eq!(reopened.user_id(), original);
    }

    /// A reopened library must never reissue an `add_id` counter it has already
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
        let ws2 = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
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

    /// The reset-to-zero case is the *only* one the durability rule licenses: a device
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

        let ws2 = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
        assert_eq!(
            ws2.counter.peek(),
            0,
            "a device with no written add_id starts at zero"
        );
    }

    /// The high-water mark is per-device: another device's much larger counters in the
    /// same sidecar must not inflate this device's next `add_id` (they are a different key in the
    /// `(device, counter)` space, and consuming them would burn 500 of our own counters for
    /// nothing).
    #[test]
    fn add_id_reseed_ignores_other_devices_counters() {
        use capsule_core::metadata::crdt::AddId;

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

        let ws2 = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
        assert_eq!(
            ws2.counter.peek(),
            1,
            "our next counter is one past OUR max (0), not one past the peer's 500"
        );
    }

    /// An injected `client_id` flows onto every write the workspace authors — the create
    /// manifest and a later metadata-update record both report the app's identity (not the bare
    /// `capsule-core` default), and each value parses as the normative grammar.
    #[test]
    fn injected_client_id_flows_to_every_write() {
        use capsule_core::client_build::ClientVersion;

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
        use capsule_core::client_build::ClientVersion;

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

        use capsule_core::crypto::keys::{HardwareBackedSigner, MockHardwareSigner};

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

    // ── the device encryption key bound to a secure element, in a real workspace ──

    /// The mock element + alias every hardware-bound workspace test binds to. A real Secure
    /// Enclave / StrongBox / TPM adapter substitutes for the mock without a change above this
    /// line — that is the point of the [`HardwareKeyAgreement`] seam.
    fn mock_dek_binding(scalar: u8) -> HardwareDekBinding {
        use capsule_core::crypto::keys::MockP256KeyAgreement;

        HardwareDekBinding {
            element: Arc::new(MockP256KeyAgreement::new([scalar; 32], false)),
            key_alias: "device-dek".into(),
        }
    }

    /// **The acceptance case.** A workspace created with a `HardwareKeyAgreement` round-trips
    /// lock/unlock: close it, reopen it with the same element, and the DEK is still the same
    /// hardware-bound key — a secret encapsulated to it *before* the close is recovered *after*
    /// the reopen, with the classical ECDH done inside the element both times.
    ///
    /// The asset import either side of the close is what makes this a workspace test rather than
    /// a keystore one: the hardware DEK must coexist with the whole durable-state
    /// restore, not merely survive in isolation.
    #[test]
    fn hardware_dek_workspace_round_trips_lock_and_unlock() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(&img, b"\xFF\xD8\xFF hardware-DEK asset").unwrap();
        let binding = mock_dek_binding(0x7A);

        let (published, ciphertext, sent_secret, asset) = {
            let mut ws = Workspace::create_with_hardware_dek(
                lib.path(),
                b"passphrase",
                FAST_PARAMS,
                binding.clone(),
            )
            .unwrap();
            assert!(
                ws.device_dek_is_hardware_bound(),
                "the workspace's DEK must report itself hardware-bound"
            );

            let published = ws.device_dek_public();
            let (ct, sent) = encapsulate_to_p256_public(&published).unwrap();
            assert_eq!(
                ws.device_dek_decapsulate(&ct).unwrap(),
                sent,
                "the live workspace decapsulates through the element"
            );

            let album = ws.create_album("Trip").unwrap();
            let asset = ws.import_asset(album, &img).unwrap();
            assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
            (published, ct, sent, asset)
        };

        // Reopen with the element re-attached: the same DEK comes back and still opens the
        // ciphertext sealed to it in the previous session.
        let reopened =
            Workspace::open_with_hardware_dek(lib.path(), b"passphrase", FAST_PARAMS, binding)
                .unwrap();
        assert!(reopened.device_dek_is_hardware_bound());
        assert_eq!(reopened.device_dek_public(), published);
        assert_eq!(
            reopened.device_dek_decapsulate(&ciphertext).unwrap(),
            sent_secret,
            "a secret sealed before the close must open after the reopen"
        );
        // The durable state is unaffected by the hardware binding.
        assert_eq!(reopened.verify(&asset).unwrap(), VerifyOutcome::Accept);
    }

    /// Reopening a hardware-bound library **without** the element fails loudly instead of handing
    /// back a workspace whose DEK silently decapsulates nothing.
    #[test]
    fn reopening_a_hardware_dek_library_without_the_element_is_refused() {
        let lib = TempDir::new().unwrap();
        drop(
            Workspace::create_with_hardware_dek(
                lib.path(),
                b"passphrase",
                FAST_PARAMS,
                mock_dek_binding(0x1D),
            )
            .unwrap(),
        );

        assert!(
            matches!(
                Workspace::open(lib.path(), b"passphrase", FAST_PARAMS),
                Err(LifecycleError::Crypto(CryptoError::Key(_)))
            ),
            "a hardware-bound library must not open without its secure element"
        );
    }

    /// **Existing software-DEK workspaces are unaffected.** A workspace created the ordinary way
    /// still reports a software DEK, still reopens with plain `open`, and its DEK still
    /// round-trips under the software X-Wing form — the fallback the design keeps for hosts with
    /// no secure element.
    #[test]
    fn software_dek_workspaces_are_unaffected() {
        let lib = TempDir::new().unwrap();

        let published = {
            let ws = fast_workspace(lib.path());
            assert!(!ws.device_dek_is_hardware_bound());
            ws.device_dek_public()
        };

        let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
        assert!(!reopened.device_dek_is_hardware_bound());
        assert_eq!(reopened.device_dek_public(), published);

        let (ct, sent) = capsule_core::crypto::keys::encapsulate_to_public(&published).unwrap();
        assert_eq!(reopened.device_dek_decapsulate(&ct).unwrap(), sent);

        // A P-256-hybrid ciphertext is length-rejected by the software DEK, so the two
        // compositions can never be confused for one another.
        let hw_lib = TempDir::new().unwrap();
        let hw_ws = Workspace::create_with_hardware_dek(
            hw_lib.path(),
            b"passphrase",
            FAST_PARAMS,
            mock_dek_binding(0x2E),
        )
        .unwrap();
        let (hw_ct, _) = encapsulate_to_p256_public(&hw_ws.device_dek_public()).unwrap();
        assert!(reopened.device_dek_decapsulate(&hw_ct).is_err());
    }

    /// **Newer-catalog refusal at the workspace boundary.** `Workspace::open` no longer
    /// stringifies a library-open failure into `Io`: a catalog newer than this build surfaces
    /// as `LifecycleError::Library(LibraryError::CatalogTooNew { .. })`, so a client can name
    /// both versions and tell the user to update rather than to check their disk.
    #[test]
    fn open_surfaces_a_too_new_catalog_as_a_typed_library_error() {
        use crate::db::schema::SCHEMA_VERSION;
        use crate::library::LibraryError;

        let lib = TempDir::new().unwrap();
        drop(fast_workspace(lib.path()));

        let db_path = lib.path().join("index/library.sqlite");
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(&format!("PRAGMA user_version = {};", SCHEMA_VERSION + 1))
                .unwrap();
        }
        let before = fs::read(&db_path).unwrap();

        match Workspace::open(lib.path(), b"passphrase", FAST_PARAMS) {
            Err(LifecycleError::Library(LibraryError::CatalogTooNew { found, supported })) => {
                assert_eq!(found, SCHEMA_VERSION + 1);
                assert_eq!(supported, SCHEMA_VERSION);
            }
            Ok(_) => panic!("a too-new catalog must not open"),
            Err(other) => panic!("expected Library(CatalogTooNew), got {other:?}"),
        }
        assert_eq!(fs::read(&db_path).unwrap(), before, "nothing was written");
        assert!(!lib.path().join(".library/lock").exists());
    }
}
