//! Workspace construction and opening: account create/unlock, device-directory publication,
//! and the injected client / still-encoder seams.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::{LifecycleError, Result, Workspace, now_rfc3339};
use crate::cbor;
use crate::crypto::keys::directory::{DeviceEntry, DirectoryCore};
use crate::crypto::keys::{Account, AccountFile, DeviceDirectory, HybridVerifyingKey, Signer};
use crate::metadata::crdt::Counter;

/// A device is treated as added far in the past so any import timestamp postdates it.
const DEVICE_ADDED_AT: &str = "2020-01-01T00:00:00Z";

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
