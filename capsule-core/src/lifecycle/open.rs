//! Workspace construction and opening: account create/unlock, device-directory publication,
//! and the injected client / still-encoder seams.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use uuid::Uuid;
use walkdir::WalkDir;

use super::{LifecycleError, Result, Workspace, now_rfc3339};
use crate::cbor;
use crate::crypto::keys::directory::{DeviceEntry, DirectoryCore};
use crate::crypto::keys::{Account, AccountFile, DeviceDirectory, HybridVerifyingKey, Signer};
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
            let album = ws.create_album("Trip");
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
            let album = ws.create_album("Trip");
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
            let album = ws.create_album("Trip");
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
        let album = ws.create_album("Trip");
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
        let album = ws.create_album("Trip");
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
