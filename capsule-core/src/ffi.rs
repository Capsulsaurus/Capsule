//! uniffi bindings: a thin, `Mutex`-guarded wrapper over [`Workspace`] exposing the offline
//! asset lifecycle to Kotlin/Swift. The surface is deliberately minimal — ids and paths cross
//! as `String`, keys and blobs as `bytes` — so the Rust API stays free to evolve behind it.
//! SSoT for the operations is [`crate::lifecycle`].
//!
//! `Workspace` mutates through `&mut self`, but uniffi objects expose only `&self`; the `Mutex`
//! supplies the interior mutability and serializes concurrent foreign calls.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use crate::crypto::CryptoError;
use crate::crypto::VerifyOutcome;
use crate::crypto::keys::{HardwareBackedSigner, HardwareSigner, HybridVerifyingKey};
use crate::crypto::primitives::DeviceTier;
use crate::lifecycle::{LifecycleError, Workspace};

/// Result alias for the FFI layer (every fallible call surfaces a [`LifecycleError`]).
type FfiResult<T> = Result<T, LifecycleError>;

/// Parse a UUID string from foreign code, mapping a parse failure to a [`LifecycleError`]
/// rather than panicking across the FFI boundary.
fn parse_uuid(s: &str) -> FfiResult<Uuid> {
    Uuid::parse_str(s).map_err(|e| LifecycleError::NotFound(format!("invalid uuid {s:?}: {e}")))
}

/// The verification verdict, flattened for FFI (reasons as stable reason-code strings).
#[derive(uniffi::Enum)]
pub enum FfiVerifyOutcome {
    /// The asset verified and may enter the trusted set.
    Accept,
    /// The asset is permanently rejected (quarantined); `reason` is the reason code.
    TerminalReject {
        /// Stable reason-code string (e.g. `RemovedWriter`).
        reason: String,
    },
    /// The asset is recoverable-pending (held + retried); `reason` is the reason code.
    Pending {
        /// Stable reason-code string (e.g. `AmkNotYetLocal`).
        reason: String,
    },
}

impl From<VerifyOutcome> for FfiVerifyOutcome {
    fn from(o: VerifyOutcome) -> Self {
        match o {
            VerifyOutcome::Accept => Self::Accept,
            VerifyOutcome::TerminalReject(r) => Self::TerminalReject {
                reason: format!("{r:?}"),
            },
            VerifyOutcome::Pending(p) => Self::Pending {
                reason: format!("{p:?}"),
            },
        }
    }
}

/// An offline Capsule workspace, callable from Kotlin/Swift.
#[derive(uniffi::Object)]
pub struct FfiWorkspace {
    inner: Mutex<Workspace>,
}

impl FfiWorkspace {
    /// Run `f` under the workspace lock, surfacing a poisoned lock as an I/O error.
    fn with<T>(&self, f: impl FnOnce(&mut Workspace) -> FfiResult<T>) -> FfiResult<T> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| LifecycleError::Io("workspace lock poisoned".into()))?;
        f(&mut guard)
    }
}

#[uniffi::export]
impl FfiWorkspace {
    /// Create a brand-new workspace rooted at `root`, guarded by `passphrase` at the Argon2id
    /// cost for `tier`. (Re-opening an existing workspace is a separate core capability still
    /// pending; see `DEFERRED.md`.)
    #[uniffi::constructor]
    pub fn create(root: String, passphrase: Vec<u8>, tier: DeviceTier) -> FfiResult<Arc<Self>> {
        let ws = Workspace::create(&PathBuf::from(root), &passphrase, tier)?;
        Ok(Arc::new(Self {
            inner: Mutex::new(ws),
        }))
    }

    /// Create a workspace whose device signing key is **hardware-bound**: the Ed25519 half is
    /// produced by `hardware` (a native Secure Enclave / StrongBox / TPM implementation of
    /// [`HardwareSigner`]) under `key_alias`, while `ml_seed` (32 bytes) is the software-sealed
    /// ML-DSA-65 `ξ` half. The published device key and every manifest are signed by the
    /// composed hardware+software hybrid key.
    #[uniffi::constructor]
    pub fn create_with_hardware_signer(
        root: String,
        passphrase: Vec<u8>,
        tier: DeviceTier,
        hardware: Arc<dyn HardwareSigner>,
        key_alias: String,
        ml_seed: Vec<u8>,
    ) -> FfiResult<Arc<Self>> {
        let seed: [u8; 32] = ml_seed
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::Malformed("ml_seed must be 32 bytes"))?;
        let signer = HardwareBackedSigner::enroll(hardware, key_alias, &seed)?;
        let ws = Workspace::create_with_hardware_signer(
            &PathBuf::from(root),
            &passphrase,
            tier.params(),
            Box::new(signer),
        )?;
        Ok(Arc::new(Self {
            inner: Mutex::new(ws),
        }))
    }

    /// The account owner's user id (UUID string).
    pub fn user_id(&self) -> FfiResult<String> {
        self.with(|ws| Ok(ws.user_id().to_string()))
    }

    /// The derived default-album id (UUID string).
    pub fn default_album_id(&self) -> FfiResult<String> {
        self.with(|ws| Ok(ws.default_album_id().to_string()))
    }

    /// Create an album, returning its id (UUID string).
    pub fn create_album(&self, name: String) -> FfiResult<String> {
        self.with(|ws| Ok(ws.create_album(&name).to_string()))
    }

    /// Rotate an album to a fresh epoch; returns the new epoch.
    pub fn rotate_epoch(&self, album_id: String) -> FfiResult<u32> {
        self.with(|ws| ws.rotate_epoch(parse_uuid(&album_id)?))
    }

    /// Import the file at `src` into `album_id`; returns the asset id (UUID string).
    pub fn import_asset(&self, album_id: String, src: String) -> FfiResult<String> {
        self.with(|ws| {
            let id = ws.import_asset(parse_uuid(&album_id)?, &PathBuf::from(src))?;
            Ok(id.to_string())
        })
    }

    /// Verify a managed asset through the `verify_asset` chokepoint.
    pub fn verify(&self, asset_id: String) -> FfiResult<FfiVerifyOutcome> {
        self.with(|ws| Ok(ws.verify(&parse_uuid(&asset_id)?)?.into()))
    }

    /// Add a user tag and emit a `metadata-update` provenance record.
    pub fn tag_add(&self, asset_id: String, tag: String) -> FfiResult<()> {
        self.with(|ws| ws.tag_add(&parse_uuid(&asset_id)?, &tag))
    }

    /// Set the caption (LWW register) and emit a `metadata-update` provenance record.
    pub fn set_caption(&self, asset_id: String, caption: String) -> FfiResult<()> {
        self.with(|ws| ws.set_caption(&parse_uuid(&asset_id)?, &caption))
    }

    /// Soft-delete with a signed retention window of `retain_days`.
    pub fn soft_delete(&self, asset_id: String, retain_days: i64) -> FfiResult<()> {
        self.with(|ws| ws.soft_delete(&parse_uuid(&asset_id)?, retain_days))
    }

    /// Restore a soft-deleted asset.
    pub fn restore(&self, asset_id: String) -> FfiResult<()> {
        self.with(|ws| ws.restore(&parse_uuid(&asset_id)?))
    }

    /// The plaintext bytes of a managed asset.
    pub fn read_plaintext(&self, asset_id: String) -> FfiResult<Vec<u8>> {
        self.with(|ws| ws.read_plaintext(&parse_uuid(&asset_id)?))
    }

    /// All managed asset ids (UUID strings).
    pub fn asset_ids(&self) -> FfiResult<Vec<String>> {
        self.with(|ws| Ok(ws.asset_ids().iter().map(Uuid::to_string).collect()))
    }

    /// This device's exporter verifying-key bytes (a peer verifies a backup against it).
    pub fn exporter_verifying_key(&self) -> FfiResult<Vec<u8>> {
        self.with(|ws| Ok(ws.exporter_verifying_key().to_bytes()))
    }

    /// Export every managed asset to a portable backup artifact at `out`.
    pub fn export_backup(&self, out: String, passphrase: Vec<u8>) -> FfiResult<()> {
        self.with(|ws| ws.export_backup(&PathBuf::from(out), &passphrase))
    }

    /// Import a backup artifact, verifying it against `exporter_pub` bytes. Returns the number
    /// of assets added.
    pub fn import_backup(
        &self,
        archive: String,
        passphrase: Vec<u8>,
        exporter_pub: Vec<u8>,
    ) -> FfiResult<u64> {
        self.with(|ws| {
            let pubkey = HybridVerifyingKey::from_bytes(&exporter_pub)?;
            let n = ws.import_backup(&PathBuf::from(archive), &passphrase, &pubkey)?;
            Ok(n as u64)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::primitives::Argon2Params;
    use tempfile::TempDir;

    /// Build an `FfiWorkspace` over a fast-Argon2 `Workspace` (the FFI constructor uses the
    /// production cost; tests reach the private field to avoid the 64–512 MiB hashing).
    fn fast_ws(dir: &std::path::Path) -> FfiWorkspace {
        let ws = Workspace::create_with_params(
            dir,
            b"pw",
            Argon2Params {
                mem_kib: 64,
                t_cost: 1,
                p_cost: 1,
            },
        )
        .unwrap();
        FfiWorkspace {
            inner: Mutex::new(ws),
        }
    }

    #[test]
    fn ffi_round_trip_create_import_verify_tag_read() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        std::fs::write(&img, b"\xFF\xD8\xFF ffi e2e bytes").unwrap();

        let ws = fast_ws(lib.path());
        // The exported surface drives the full offline lifecycle: create → import → verify.
        let album = ws.create_album("Trip".into()).unwrap();
        let asset = ws
            .import_asset(album, img.to_string_lossy().into_owned())
            .unwrap();
        assert!(matches!(
            ws.verify(asset.clone()).unwrap(),
            FfiVerifyOutcome::Accept
        ));

        // CRDT edit + plaintext read round-trip through the wrapper.
        ws.tag_add(asset.clone(), "vacation".into()).unwrap();
        assert_eq!(
            ws.read_plaintext(asset).unwrap(),
            b"\xFF\xD8\xFF ffi e2e bytes"
        );
        assert_eq!(ws.asset_ids().unwrap().len(), 1);
        assert!(!ws.user_id().unwrap().is_empty());
    }

    #[test]
    fn ffi_surfaces_errors_instead_of_panicking() {
        let lib = TempDir::new().unwrap();
        let ws = fast_ws(lib.path());
        // A malformed UUID is a typed error, not a panic across the boundary.
        assert!(ws.verify("not-a-uuid".into()).is_err());
        // An unknown asset id surfaces NotFound.
        let missing = Uuid::now_v7().to_string();
        assert!(ws.read_plaintext(missing).is_err());
    }

    #[test]
    fn ffi_hardware_backed_workspace_round_trips() {
        use crate::crypto::keys::HardwareBackedSigner;
        use crate::crypto::keys::hardware::MockHardwareSigner;

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        std::fs::write(&img, b"\xFF\xD8\xFF hw ffi bytes").unwrap();

        // Build the FFI wrapper over a hardware-backed Workspace (fast Argon2 for the test).
        let hw = Arc::new(MockHardwareSigner::new([5; 32], false));
        let signer = HardwareBackedSigner::enroll(hw, "device-dsk".into(), &[6; 32]).unwrap();
        let ws = FfiWorkspace {
            inner: Mutex::new(
                Workspace::create_with_hardware_signer(
                    lib.path(),
                    b"pw",
                    Argon2Params {
                        mem_kib: 64,
                        t_cost: 1,
                        p_cost: 1,
                    },
                    Box::new(signer),
                )
                .unwrap(),
            ),
        };

        let album = ws.create_album("Trip".into()).unwrap();
        let asset = ws
            .import_asset(album, img.to_string_lossy().into_owned())
            .unwrap();
        assert!(matches!(
            ws.verify(asset).unwrap(),
            FfiVerifyOutcome::Accept
        ));
    }
}
