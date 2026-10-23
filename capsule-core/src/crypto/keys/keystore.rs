//! The device keystore: an [`Account`] (master key + user identity key + this device's
//! keys) and its encrypted-at-rest form [`AccountFile`].
//!
//! The master key is wrapped under a passphrase via [`pwkdf`], and the device identity private
//! keys are sealed under the master key (the design's "master key wraps device identity private
//! keys"). SSoT: [Cryptography — Keys].
//!
//! ## The DEK's classical half (`S-F5` → `S-F8`)
//!
//! The design binds the **classical half** of each device key to a per-platform secure element.
//! For the Device Encryption Key that means ECDH-P256 inside a Secure Enclave / StrongBox / TPM,
//! composed with a software ML-KEM-768 half — the [`P256HybridDek`] the `S-F5` slice landed.
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
    /// half (`S-F5`'s [`P256HybridDek`]). The private P-256 scalar never leaves hardware, so this
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
    /// Both halves in software (X-Wing). The default, and what every pre-`S-F8` account file
    /// means by omitting the field.
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
    /// Where the DEK's classical half lives. Absent in pre-`S-F8` account files, which
    /// are software-DEK by construction — hence the `default`, which is what keeps every existing
    /// library on disk openable.
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
    /// under `key_alias` and never leaves it (`S-F5` composition, `S-F8` wiring). The ML-KEM-768
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
mod tests {
    use serde::Serialize;

    use super::*;
    use crate::crypto::keys::kem_p256::{
        DEK_P256_PUBLIC_LEN, MockP256KeyAgreement, encapsulate_to_p256_public,
    };

    // Fast Argon2 params keep keystore tests quick; the production tier table is asserted
    // in `primitives` without paying the 128–512 MiB hashing cost.
    fn fast() -> Argon2Params {
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        }
    }

    #[test]
    fn account_create_save_unlock_round_trip() {
        let acct = Account::create();
        let ik_vk = acct.user_ik.verifying_key();
        let dsk_vk = acct.device.dsk.verifying_key();
        let default_album = acct.master.derive_default_album_id();

        let file = acct.to_file_with(b"passphrase", fast()).unwrap();
        let restored = file.unlock(b"passphrase").unwrap();

        assert_eq!(restored.user_id, acct.user_id);
        assert_eq!(restored.device.device_id, acct.device.device_id);
        // Identity and device verifying keys survive the round trip.
        assert_eq!(restored.user_ik.verifying_key(), ik_vk);
        assert_eq!(restored.device.dsk.verifying_key(), dsk_vk);
        // The master key still derives the same default album id.
        assert_eq!(restored.master.derive_default_album_id(), default_album);
    }

    /// **#467.** An account assembled from recovered parts keeps every account-level key —
    /// `user_id`, master (proved by the default album id), user IK — and mints a fresh device,
    /// and it persists and unlocks like any other.
    #[test]
    fn from_parts_keeps_the_account_and_mints_a_new_device() {
        let original = Account::create();
        let sealed = original.sealed_identity();
        let user_ik = Account::open_sealed_identity(&original.master, &sealed).unwrap();

        let recovered = Account::from_parts(
            original.user_id,
            MasterKey::from_bytes(*original.master.as_bytes()),
            user_ik,
            DeviceDek::Software(DekKeypair::generate()),
        );
        assert_eq!(recovered.user_id, original.user_id);
        assert_eq!(
            recovered.master.derive_default_album_id(),
            original.master.derive_default_album_id()
        );
        assert_eq!(
            recovered.user_ik.verifying_key(),
            original.user_ik.verifying_key()
        );
        assert_ne!(recovered.device.device_id, original.device.device_id);
        assert_ne!(
            recovered.device.dsk.verifying_key(),
            original.device.dsk.verifying_key()
        );

        let file = recovered.to_file_with(b"pw", fast()).unwrap();
        let reopened = file.unlock(b"pw").unwrap();
        assert_eq!(reopened.user_id, original.user_id);
        assert_eq!(
            reopened.user_ik.verifying_key(),
            original.user_ik.verifying_key()
        );
    }

    /// A sealed identity opens only under the master key that sealed it.
    #[test]
    fn a_sealed_identity_refuses_a_foreign_master_key() {
        let acct = Account::create();
        let sealed = acct.sealed_identity();
        assert!(matches!(
            Account::open_sealed_identity(&MasterKey::generate(), &sealed),
            Err(CryptoError::Auth(_))
        ));
    }

    /// The domain prefix keeps another 64-byte seed sealed under the same master key — the
    /// account file's sealed DSK — from being accepted as the user IK.
    #[test]
    fn a_sealed_device_key_is_not_accepted_as_an_identity() {
        let acct = Account::create();
        let file = acct.to_file_with(b"pw", fast()).unwrap();
        for not_an_identity in [&file.sealed_dsk, &file.sealed_ik] {
            assert!(matches!(
                Account::open_sealed_identity(&acct.master, not_an_identity),
                Err(CryptoError::Malformed(_))
            ));
        }
    }

    #[test]
    fn wrong_passphrase_fails_to_unlock() {
        let acct = Account::create();
        let file = acct.to_file_with(b"right", fast()).unwrap();
        assert!(file.unlock(b"wrong").is_err());
    }

    // ── the DEK's classical half in a secure element ──────────────────────────────

    /// **The keystore half of the acceptance.** An account created against an element seals and
    /// unlocks: the published DEK public key is byte-identical across the round trip, and a
    /// secret encapsulated to it is recovered *through the element's ECDH*. That is the whole
    /// claim — the classical half was never in the account file, yet the DEK still works.
    #[test]
    fn hardware_bound_account_round_trips_lock_and_unlock_through_the_element() {
        let element = Arc::new(MockP256KeyAgreement::new([0x5F; 32], false));
        let acct = Account::create_with_hardware_dek(element.clone(), "device-dek".into()).unwrap();
        assert!(acct.device.dek.is_hardware_bound());
        let published = acct.device.dek.public_bytes();
        assert_eq!(
            published.len(),
            DEK_P256_PUBLIC_LEN,
            "the P-256 hybrid form"
        );

        let file = acct.to_file_with(b"pw", fast()).unwrap();
        assert_eq!(
            file.dek_binding,
            DekBinding::Hardware {
                key_alias: "device-dek".into()
            }
        );

        let restored = file.unlock_with_element(b"pw", Some(element)).unwrap();
        assert!(restored.device.dek.is_hardware_bound());
        assert_eq!(
            restored.device.dek.public_bytes(),
            published,
            "the same element + the same sealed ML-KEM seed republish the same DEK"
        );

        // The recovered DEK genuinely decapsulates: sender-side encapsulation is pure software,
        // the receiver side runs the classical half inside the element.
        let (ct, sent) = encapsulate_to_p256_public(&published).unwrap();
        assert_eq!(restored.device.dek.decapsulate(&ct).unwrap(), sent);
    }

    /// The binding is not decorative: without the element the account does not unlock at all,
    /// rather than quietly falling back to a software key that would decapsulate nothing.
    #[test]
    fn hardware_bound_account_refuses_to_unlock_without_the_element() {
        let element = Arc::new(MockP256KeyAgreement::new([0x11; 32], false));
        let file = Account::create_with_hardware_dek(element, "device-dek".into())
            .unwrap()
            .to_file_with(b"pw", fast())
            .unwrap();

        assert!(
            matches!(file.unlock(b"pw"), Err(CryptoError::Key(_))),
            "a hardware-bound DEK must not silently degrade to software"
        );
    }

    /// A *different* element cannot stand in for the enrolled one: it republishes a different
    /// public key, and a ciphertext sealed to the real device does not open under it. This is
    /// what "bound to hardware" means operationally.
    #[test]
    fn a_foreign_element_cannot_impersonate_the_enrolled_one() {
        let real = Arc::new(MockP256KeyAgreement::new([0x22; 32], false));
        let acct = Account::create_with_hardware_dek(real, "device-dek".into()).unwrap();
        let published = acct.device.dek.public_bytes();
        let (ct, sent) = encapsulate_to_p256_public(&published).unwrap();
        let file = acct.to_file_with(b"pw", fast()).unwrap();

        let impostor = Arc::new(MockP256KeyAgreement::new([0x33; 32], false));
        let wrong = file.unlock_with_element(b"pw", Some(impostor)).unwrap();
        assert_ne!(wrong.device.dek.public_bytes(), published);
        assert_ne!(
            wrong.device.dek.decapsulate(&ct).unwrap(),
            sent,
            "the ML-KEM half alone must not recover the secret"
        );
    }

    /// The software fallback is untouched: it stays the default, records itself as such, needs no
    /// element, and ignores one if handed a spurious one (the file's binding decides, not the
    /// caller).
    #[test]
    fn software_dek_accounts_are_unaffected_by_the_hardware_seam() {
        let acct = Account::create();
        assert!(!acct.device.dek.is_hardware_bound());
        let published = acct.device.dek.public_bytes();
        let file = acct.to_file_with(b"pw", fast()).unwrap();
        assert_eq!(file.dek_binding, DekBinding::Software);

        let restored = file.unlock(b"pw").unwrap();
        assert_eq!(restored.device.dek.public_bytes(), published);
        let (ct, sent) = crate::crypto::keys::encapsulate_to_public(&published).unwrap();
        assert_eq!(restored.device.dek.decapsulate(&ct).unwrap(), sent);

        // Handing a software account an element changes nothing.
        let element = Arc::new(MockP256KeyAgreement::new([0x44; 32], false));
        let with_element = file.unlock_with_element(b"pw", Some(element)).unwrap();
        assert!(!with_element.device.dek.is_hardware_bound());
        assert_eq!(with_element.device.dek.public_bytes(), published);
    }

    /// **Migration.** A pre-`S-F8` account file has no `dek_binding` field at all. It must still
    /// open, as software — proved by decoding a struct that literally lacks the field, which is
    /// what those bytes on disk are.
    #[test]
    fn a_pre_s_f8_account_file_without_a_binding_field_opens_as_software() {
        /// The exact pre-`S-F8` `AccountFile` shape.
        #[derive(Serialize)]
        struct LegacyAccountFile {
            user_id: Uuid,
            device_id: Uuid,
            wrapped_master: pwkdf::WrappedSecret,
            #[serde(with = "serde_bytes")]
            sealed_ik: Vec<u8>,
            #[serde(with = "serde_bytes")]
            sealed_dsk: Vec<u8>,
            #[serde(with = "serde_bytes")]
            sealed_dek: Vec<u8>,
        }

        let acct = Account::create();
        let published = acct.device.dek.public_bytes();
        let file = acct.to_file_with(b"pw", fast()).unwrap();
        let legacy = LegacyAccountFile {
            user_id: file.user_id,
            device_id: file.device_id,
            wrapped_master: file.wrapped_master.clone(),
            sealed_ik: file.sealed_ik.clone(),
            sealed_dsk: file.sealed_dsk.clone(),
            sealed_dek: file.sealed_dek.clone(),
        };

        let bytes = crate::cbor::to_canonical_vec(&legacy).unwrap();
        let decoded: AccountFile = crate::cbor::from_slice(&bytes).unwrap();
        assert_eq!(decoded.dek_binding, DekBinding::Software);
        assert_eq!(
            decoded.unlock(b"pw").unwrap().device.dek.public_bytes(),
            published
        );
    }

    #[test]
    fn account_file_serializes_canonically() {
        let acct = Account::create();
        let file = acct.to_file_with(b"pw", fast()).unwrap();
        let bytes = crate::cbor::to_canonical_vec(&file).unwrap();
        let back: AccountFile = crate::cbor::from_slice(&bytes).unwrap();
        assert_eq!(back.user_id, file.user_id);
        assert_eq!(back.sealed_dek, file.sealed_dek);
    }
}
