//! The device keystore: an [`Account`] (master key + user identity key + this device's
//! keys) and its encrypted-at-rest form [`AccountFile`].
//!
//! The master key is wrapped under a passphrase via [`pwkdf`], and the device identity private
//! keys are sealed under the master key (the design's "master key wraps device identity private
//! keys"). SSoT: [Cryptography — Keys].
//!
//! ## The DEK's classical half
//!
//! The design binds the **classical half** of each device key to a per-platform secure element.
//! For the Device Encryption Key that means ECDH-P256 inside a Secure Enclave / StrongBox / TPM,
//! composed with a software ML-KEM-768 half — the [`P256HybridDek`].
//! [`DeviceDek`] is the seam that puts it in a *real* account rather than only in the FFI smoke:
//! an account is created either [software-DEK](Account::create) (the fallback for hosts with no
//! element — explicitly retained by the design's "the software composition is what integrates
//! end-to-end today") or [hardware-DEK](Account::create_with_hardware_dek).
//!
//! The [`AccountFile`] records which, in [`DekBinding`]. Only the **software-sealed** half is
//! ever written: an X-Wing seed for the software DEK, the ML-KEM seed for the hardware one. The
//! hardware classical half is non-exportable by contract, so a hardware-bound account cannot be
//! unlocked without the element that holds it — [`AccountFile::unlock`] says so rather than
//! silently degrading to software, and [`AccountFile::unlock_with_element`] supplies it. That
//! asymmetry *is* the binding; without it the "hardware-bound" claim would be decorative.
//!
//! [Cryptography — Keys]: https://docs/design/cryptography/keys/

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::hardware::{HardwareKeyAgreement, HardwareSignerError};
use super::hybrid_sig::HybridSigningKey;
use super::kem::DekKeypair;
use super::kem_p256::P256HybridDek;
use super::master::MasterKey;
use crate::crypto::primitives::{Argon2Params, DeviceTier};
use crate::crypto::{CryptoError, pwkdf};

/// Prefix sealed in front of the user IK seeds by [`Account::sealed_identity`], so no other
/// 64-byte seed sealed under the same master key (an `AccountFile`'s DSK) opens as an identity.
const SEALED_IDENTITY_DOMAIN: &[u8] = b"capsule-sealed-user-ik/v1\0";

/// This device's Device Encryption Key, in whichever of the two compositions the design allows.
///
/// Both expose the same two operations — publish a public encapsulation key, decapsulate a
/// ciphertext sealed to it — so every caller is agnostic to where the classical half lives. The
/// two byte formats are length-disjoint — X-Wing's [`DekKeypair`] and the P-256 hybrid's
/// [`P256HybridDek`] publish and accept different lengths — so a ciphertext for one is rejected
/// outright by the other rather than silently recovering a wrong secret.
pub enum DeviceDek {
    /// **Software fallback.** X-Wing (X25519 + ML-KEM-768), both halves in software. The
    /// composition every host can run, including those with no secure element and the TPM-1.2 /
    /// no-TPM tail the design excludes from hardware binding by name.
    Software(DekKeypair),
    /// **Hardware-bound.** P-256 ECDH inside the secure element + a software-sealed ML-KEM-768
    /// half ([`P256HybridDek`]). The private P-256 scalar never leaves hardware, so this
    /// variant cannot be reconstructed from the account file alone.
    Hardware(P256HybridDek),
}

impl DeviceDek {
    /// The published public encapsulation-key bytes a sender wraps to. `pk_M ‖ pk_X` (1216 bytes)
    /// for the software X-Wing DEK; `ek_M ‖ pk_P` (1249 bytes) for the hardware P-256 hybrid.
    pub fn public_bytes(&self) -> Vec<u8> {
        match self {
            Self::Software(dek) => dek.public_bytes(),
            Self::Hardware(dek) => dek.public_bytes(),
        }
    }

    /// Recover the 32-byte shared secret from a ciphertext sealed to [`public_bytes`](Self::public_bytes).
    /// For [`Hardware`](Self::Hardware) the classical half of this call happens **inside the
    /// element**, so it can fail on a cancelled biometric or a missing key.
    pub fn decapsulate(&self, ciphertext: &[u8]) -> Result<[u8; 32], CryptoError> {
        match self {
            Self::Software(dek) => dek.decapsulate(ciphertext),
            Self::Hardware(dek) => dek.decapsulate(ciphertext),
        }
    }

    /// Whether the classical half of this DEK lives in a secure element.
    pub fn is_hardware_bound(&self) -> bool {
        matches!(self, Self::Hardware(_))
    }

    /// How this DEK is recorded in the [`AccountFile`].
    pub fn binding(&self) -> DekBinding {
        match self {
            Self::Software(_) => DekBinding::Software,
            Self::Hardware(dek) => DekBinding::Hardware {
                key_alias: dek.key_alias().to_owned(),
            },
        }
    }

    /// The 32-byte software seed sealed under the master key: the X-Wing seed for
    /// [`Software`](Self::Software), the ML-KEM seed for [`Hardware`](Self::Hardware).
    fn sealed_seed(&self) -> [u8; 32] {
        match self {
            Self::Software(dek) => dek.to_seed_bytes(),
            Self::Hardware(dek) => dek.to_ml_seed_bytes(),
        }
    }
}

impl std::fmt::Debug for DeviceDek {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Software(_) => f.write_str("DeviceDek::Software(****)"),
            Self::Hardware(dek) => {
                write!(f, "DeviceDek::Hardware(alias={}, ****)", dek.key_alias())
            }
        }
    }
}

/// Where the DEK's classical half lives, as recorded in the [`AccountFile`].
///
/// Persisted in the clear: it is a routing decision plus a lookup handle, never key material. It
/// is what lets `open` know an element must be attached *before* it tries — the alternative
/// (guess, fail to decapsulate later) turns a clear "attach the element" into an opaque
/// authentication error deep in a data-plane path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DekBinding {
    /// Both halves in software (X-Wing). The default, and what every account file
    /// that omits the field means.
    #[default]
    Software,
    /// The classical half is a hardware P-256 key under `key_alias`.
    Hardware {
        /// The secure element's alias for this device's P-256 key-agreement key.
        key_alias: String,
    },
}

/// This device's key material: a stable id, a hybrid Device Signing Key (DSK), and a
/// Device Encryption Key (DEK).
pub struct DeviceKeys {
    /// Stable per-device identifier (UUIDv7), published in the device directory.
    pub device_id: Uuid,
    /// Hybrid device signing key — signs asset manifests (`device_sig`).
    pub dsk: HybridSigningKey,
    /// Device encryption key (KEM) — receives key wraps. Software or hardware-bound; see
    /// [`DeviceDek`].
    pub dek: DeviceDek,
}

/// A fully unlocked account in memory.
pub struct Account {
    /// The account owner's user id (UUIDv7).
    pub user_id: Uuid,
    /// The backed-up root key.
    pub master: MasterKey,
    /// The user identity key (root of signing trust). Signs the device directory.
    pub user_ik: HybridSigningKey,
    /// This device's keys.
    pub device: DeviceKeys,
}

/// The encrypted-at-rest account: the master key wrapped under a passphrase, and the
/// device/identity private keys sealed under the master key. Safe to persist to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountFile {
    /// Account owner id.
    pub user_id: Uuid,
    /// This device's id.
    pub device_id: Uuid,
    /// Master key wrapped under the passphrase (Argon2id + AES-256-GCM).
    pub wrapped_master: pwkdf::WrappedSecret,
    /// User IK seeds (64 bytes) sealed under the master key.
    #[serde(with = "serde_bytes")]
    pub sealed_ik: Vec<u8>,
    /// Device DSK seeds (64 bytes) sealed under the master key.
    #[serde(with = "serde_bytes")]
    pub sealed_dsk: Vec<u8>,
    /// The DEK's **software-sealed** 32-byte seed under the master key: the X-Wing seed for a
    /// software DEK, the ML-KEM-768 seed for a hardware-bound one. A hardware classical half is
    /// never here — it is non-exportable by contract.
    #[serde(with = "serde_bytes")]
    pub sealed_dek: Vec<u8>,
    /// Where the DEK's classical half lives. Absent in account files that predate
    /// hardware DEKs, which are software-DEK by construction — hence the `default`, which is
    /// what keeps every existing library on disk openable.
    #[serde(default)]
    pub dek_binding: DekBinding,
}

impl Account {
    /// Create a brand-new account with a fresh master key, user IK, and first device, using the
    /// **software** DEK composition. The fallback the design keeps for hosts with no secure
    /// element; [`create_with_hardware_dek`](Self::create_with_hardware_dek) is the bound form.
    pub fn create() -> Self {
        Self::generate(DeviceDek::Software(DekKeypair::generate()))
    }

    /// Create a brand-new account whose **DEK's classical half is generated inside `element`**
    /// under `key_alias` and never leaves it. The ML-KEM-768
    /// half is drawn fresh here and sealed under the master key like any software seed.
    ///
    /// Fails only if the element refuses enrollment (unavailable, biometric cancelled, bad key) —
    /// deliberately *not* silently falling back to software, because a caller that asked for
    /// hardware binding and got software would believe a guarantee it does not have. A caller
    /// that wants best-effort binding probes the element first and calls
    /// [`create`](Self::create) itself.
    pub fn create_with_hardware_dek(
        element: Arc<dyn HardwareKeyAgreement>,
        key_alias: String,
    ) -> Result<Self, HardwareSignerError> {
        let ml_seed = crate::crypto::rng::random_array::<32>();
        let dek = P256HybridDek::enroll(element, key_alias, &ml_seed)?;
        Ok(Self::generate(DeviceDek::Hardware(dek)))
    }

    fn generate(dek: DeviceDek) -> Self {
        Self::from_parts(
            Uuid::now_v7(),
            MasterKey::generate(),
            HybridSigningKey::generate(),
            dek,
        )
    }

    /// Assemble an account from **existing** account-level material on a **new** device: the
    /// account's `user_id`, its `master` key (e.g. recovered from the server escrow with
    /// [`recover_master_key`](crate::backup::recover_master_key)), and its user identity key
    /// (e.g. opened from a [sealed identity](Self::sealed_identity) with
    /// [`open_sealed_identity`](Self::open_sealed_identity)).
    ///
    /// Everything per-device is fresh: a new `device_id` and DSK, plus the caller's `dek`. A
    /// recovered device is a new device — it never resumes a lost device's keys, whose
    /// classical halves may have been hardware-bound and are gone with it.
    pub fn from_parts(
        user_id: Uuid,
        master: MasterKey,
        user_ik: HybridSigningKey,
        dek: DeviceDek,
    ) -> Self {
        Self {
            user_id,
            master,
            user_ik,
            device: DeviceKeys {
                device_id: Uuid::now_v7(),
                dsk: HybridSigningKey::generate(),
                dek,
            },
        }
    }

    /// The user identity key's private seeds sealed under the master key, in a
    /// domain-separated form a client may store off-device (e.g. beside the master-key escrow).
    ///
    /// The user IK is drawn at random at account creation, not derived from the master key —
    /// rotating the master key must not rotate the identity, and the server pins the identity
    /// public key on the first directory publish. So a device that recovers only the
    /// master key would otherwise have to mint a *new* IK, which the server refuses for an
    /// account it has already anchored. This blob is a wrap *reachable from* the master key, not
    /// a second root: whoever recovers the master key opens it, nobody else can.
    pub fn sealed_identity(&self) -> Vec<u8> {
        let seed = self.user_ik.to_seed_bytes();
        let mut plaintext = Vec::with_capacity(SEALED_IDENTITY_DOMAIN.len() + seed.len());
        plaintext.extend_from_slice(SEALED_IDENTITY_DOMAIN);
        plaintext.extend_from_slice(&seed);
        self.master.seal(&plaintext)
    }

    /// Open a [`sealed_identity`](Self::sealed_identity) blob under `master`.
    ///
    /// Returns [`CryptoError::Auth`] when `master` is not the key that sealed it (or the blob was
    /// tampered with), and [`CryptoError::Malformed`] when it opens but is not a sealed user IK —
    /// e.g. an `AccountFile`'s sealed *device* key, which the domain prefix keeps from being
    /// accepted as an identity.
    pub fn open_sealed_identity(
        master: &MasterKey,
        sealed: &[u8],
    ) -> Result<HybridSigningKey, CryptoError> {
        let plaintext = master.open(sealed)?;
        let seed = plaintext
            .strip_prefix(SEALED_IDENTITY_DOMAIN)
            .ok_or(CryptoError::Malformed(
                "sealed identity lacks its domain prefix",
            ))?;
        Ok(HybridSigningKey::from_seed64(&seed64(seed.to_vec())?))
    }

    /// Encrypt the account for persistence: master under `passphrase` (cost = `tier`),
    /// identity/device private keys under the master key.
    pub fn to_file(&self, passphrase: &[u8], tier: DeviceTier) -> Result<AccountFile, CryptoError> {
        self.to_file_with(passphrase, tier.params())
    }

    /// As [`to_file`](Self::to_file) but with explicit Argon2id parameters (used by tests
    /// to avoid the multi-hundred-MiB production cost).
    pub fn to_file_with(
        &self,
        passphrase: &[u8],
        params: Argon2Params,
    ) -> Result<AccountFile, CryptoError> {
        Ok(AccountFile {
            user_id: self.user_id,
            device_id: self.device.device_id,
            wrapped_master: pwkdf::wrap_with(self.master.as_bytes(), passphrase, params)?,
            sealed_ik: self.master.seal(&self.user_ik.to_seed_bytes()),
            sealed_dsk: self.master.seal(&self.device.dsk.to_seed_bytes()),
            sealed_dek: self.master.seal(&self.device.dek.sealed_seed()),
            dek_binding: self.device.dek.binding(),
        })
    }
}

fn seed64(bytes: Vec<u8>) -> Result<[u8; 64], CryptoError> {
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::Malformed("sealed key seed wrong length"))
}

fn seed32(bytes: Vec<u8>) -> Result<[u8; 32], CryptoError> {
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::Malformed("sealed key seed wrong length"))
}

impl AccountFile {
    /// Decrypt the account with `passphrase`. Returns [`CryptoError::Auth`] on a wrong
    /// passphrase (master unwrap fails) or tampering.
    ///
    /// A **hardware-bound** account ([`DekBinding::Hardware`]) cannot be unlocked this way — its
    /// DEK's classical half lives in an element this call was given no handle to — and returns
    /// [`CryptoError::Key`]. Use [`unlock_with_element`](Self::unlock_with_element).
    pub fn unlock(&self, passphrase: &[u8]) -> Result<Account, CryptoError> {
        self.unlock_with_element(passphrase, None)
    }

    /// As [`unlock`](Self::unlock), re-binding a [`DekBinding::Hardware`] account's DEK to
    /// `element` (the same secure element that enrolled it, e.g. a real Secure Enclave or a test
    /// mock). The alias comes from the file, so the caller supplies only the element.
    ///
    /// Passing an element for a software-DEK account is harmless and ignored: the binding
    /// recorded at creation decides, never the caller, so an attacker who can hand us an element
    /// cannot thereby move a software account onto a key they control.
    pub fn unlock_with_element(
        &self,
        passphrase: &[u8],
        element: Option<Arc<dyn HardwareKeyAgreement>>,
    ) -> Result<Account, CryptoError> {
        let master_bytes: [u8; 32] = pwkdf::unwrap(&self.wrapped_master, passphrase)?
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::Malformed("master key wrong length"))?;
        let master = MasterKey::from_bytes(master_bytes);

        let user_ik = HybridSigningKey::from_seed64(&seed64(master.open(&self.sealed_ik)?)?);
        let dsk = HybridSigningKey::from_seed64(&seed64(master.open(&self.sealed_dsk)?)?);
        let dek_seed = seed32(master.open(&self.sealed_dek)?)?;
        let dek = match &self.dek_binding {
            DekBinding::Software => DeviceDek::Software(DekKeypair::from_seed(&dek_seed)),
            DekBinding::Hardware { key_alias } => {
                let element = element.ok_or(CryptoError::Key(
                    "this account's DEK is hardware-bound: a HardwareKeyAgreement element is \
                     required to unlock it",
                ))?;
                tracing::debug!(
                    device = %self.device_id,
                    key_alias = %key_alias,
                    "keystore unlock: re-binding the DEK's classical half to the secure element"
                );
                DeviceDek::Hardware(
                    P256HybridDek::enroll(element, key_alias.clone(), &dek_seed)
                        .map_err(|_| CryptoError::Key("hardware DEK re-binding failed"))?,
                )
            }
        };

        Ok(Account {
            user_id: self.user_id,
            master,
            user_ik,
            device: DeviceKeys {
                device_id: self.device_id,
                dsk,
                dek,
            },
        })
    }
}

#[cfg(test)]
mod tests;
