//! Desktop (Linux / Windows) TPM 2.0 reference [`HardwareSigner`].
//!
//! Built only under the `tpm` feature (see `Cargo.toml`): it links the system `libtss2` through
//! [`tss-esapi`] and is **never** compiled in CI. Exercising it needs a real TPM or a software
//! TPM (`swtpm`); see `capsule-core/README-tpm.md` for the `swtpm` round-trip.
//!
//! # Algorithm caveat (the same one Secure Enclave has)
//!
//! The [`HardwareSigner`] contract is phrased in Ed25519 (a 32-byte public key, a 64-byte
//! signature) because that is the classical half of Capsule's hybrid DSK. Shipping TPMs expose
//! **ECDSA over NIST P-256**, not Ed25519, so this reference returns P-256 material: the `x‖y`
//! public point (64 bytes) and an ECDSA `r‖s` signature over `SHA-256(msg)` (64 bytes). It
//! therefore demonstrates the TPM key lifecycle and the non-exportability contract, but does
//! **not** yet plug into [`HardwareBackedSigner`](super::HardwareBackedSigner), which composes an
//! Ed25519 half. Wiring a TPM device in needs the P-256 hybrid-DSK variant tracked in
//! `DEFERRED.md` — exactly the follow-up the Secure Enclave note calls out.
//!
//! # Non-exportability
//!
//! The signing key is created with `fixedTPM | fixedParent | sensitiveDataOrigin` set, so its
//! private portion is generated inside the TPM and can never be duplicated out.
//! [`assert_non_exportable`](HardwareSigner::assert_non_exportable) re-reads the public area and
//! confirms those attributes, the TPM analogue of the Secure Enclave / StrongBox check.
//!
//! [`tss-esapi`]: https://docs.rs/tss-esapi

use std::collections::HashMap;
use std::sync::Mutex;

use sha2::{Digest as _, Sha256};
use tss_esapi::attributes::ObjectAttributesBuilder;
use tss_esapi::constants::SessionType;
use tss_esapi::handles::{KeyHandle, PersistentTpmHandle, TpmHandle};
use tss_esapi::interface_types::algorithm::{HashingAlgorithm, PublicAlgorithm};
use tss_esapi::interface_types::dynamic_handles::Persistent;
use tss_esapi::interface_types::ecc::EccCurve;
use tss_esapi::interface_types::resource_handles::{Hierarchy, Provision};
use tss_esapi::structures::{
    EccPoint, EccScheme, HashScheme, MaxBuffer, Public, PublicBuilder, PublicEccParametersBuilder,
    Signature, SignatureScheme, SymmetricDefinition, SymmetricDefinitionObject,
};
use tss_esapi::{Context, TctiNameConf};

use super::hardware::{HardwareSigner, HardwareSignerError};

/// Base of the owner persistent-handle range (`0x8100_0000`..`0x817F_FFFF`).
const PERSISTENT_BASE: u32 = 0x8100_0000;

/// A TPM 2.0–backed [`HardwareSigner`]. The classical signing key lives in the TPM under a
/// per-alias persistent handle and never leaves it.
pub struct TpmSigner {
    context: Mutex<Context>,
    /// Loaded key handles by alias, cached for the process lifetime.
    loaded: Mutex<HashMap<String, KeyHandle>>,
}

impl TpmSigner {
    /// Open a TPM context from the `TCTI` environment configuration and install a reusable HMAC
    /// session. For `swtpm`, set e.g. `TPM2TOOLS_TCTI=swtpm:host=127.0.0.1,port=2321`.
    pub fn from_environment() -> Result<Self, HardwareSignerError> {
        let tcti = TctiNameConf::from_environment_variable()
            .map_err(|e| HardwareSignerError::Backend(format!("TCTI config: {e}")))?;
        let mut context = Context::new(tcti)
            .map_err(|e| HardwareSignerError::Backend(format!("TPM open: {e}")))?;
        // One unbound, unsalted HMAC session, reused for the signer's lifetime (an empty-auth
        // owner hierarchy is the common desktop case). It is freed when the context drops.
        let session = context
            .start_auth_session(
                None,
                None,
                None,
                SessionType::Hmac,
                SymmetricDefinition::AES_128_CFB,
                HashingAlgorithm::Sha256,
            )
            .map_err(backend)?
            .ok_or_else(|| HardwareSignerError::Backend("no auth session".into()))?;
        context.set_sessions((Some(session), None, None));
        Ok(Self {
            context: Mutex::new(context),
            loaded: Mutex::new(HashMap::new()),
        })
    }

    /// Map an alias to a stable persistent handle in the owner range.
    fn persistent_handle(key_alias: &str) -> Result<PersistentTpmHandle, HardwareSignerError> {
        let digest = Sha256::digest(key_alias.as_bytes());
        let offset = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) & 0x000F_FFFF;
        PersistentTpmHandle::new(PERSISTENT_BASE + offset)
            .map_err(|e| HardwareSignerError::Backend(format!("persistent handle: {e}")))
    }

    /// The public-area template for the unrestricted ECDSA-P256 device signing key.
    fn signing_key_template() -> Result<Public, HardwareSignerError> {
        let attrs = ObjectAttributesBuilder::new()
            .with_fixed_tpm(true)
            .with_fixed_parent(true)
            .with_sensitive_data_origin(true)
            .with_user_with_auth(true)
            .with_sign_encrypt(true)
            .with_decrypt(false)
            .with_restricted(false)
            .build()
            .map_err(backend)?;
        PublicBuilder::new()
            .with_public_algorithm(PublicAlgorithm::Ecc)
            .with_name_hashing_algorithm(HashingAlgorithm::Sha256)
            .with_object_attributes(attrs)
            .with_ecc_parameters(
                PublicEccParametersBuilder::new()
                    .with_ecc_scheme(EccScheme::EcDsa(HashScheme::new(HashingAlgorithm::Sha256)))
                    .with_curve(EccCurve::NistP256)
                    .with_is_signing_key(true)
                    .with_is_decryption_key(false)
                    .with_restricted(false)
                    .with_symmetric(SymmetricDefinitionObject::Null)
                    .build()
                    .map_err(backend)?,
            )
            .with_ecc_unique_identifier(EccPoint::default())
            .build()
            .map_err(backend)
    }

    /// The storage-parent (primary) template under the owner hierarchy.
    fn primary_template() -> Result<Public, HardwareSignerError> {
        let attrs = ObjectAttributesBuilder::new()
            .with_fixed_tpm(true)
            .with_fixed_parent(true)
            .with_sensitive_data_origin(true)
            .with_user_with_auth(true)
            .with_decrypt(true)
            .with_sign_encrypt(false)
            .with_restricted(true)
            .build()
            .map_err(backend)?;
        PublicBuilder::new()
            .with_public_algorithm(PublicAlgorithm::Ecc)
            .with_name_hashing_algorithm(HashingAlgorithm::Sha256)
            .with_object_attributes(attrs)
            .with_ecc_parameters(
                PublicEccParametersBuilder::new()
                    .with_ecc_scheme(EccScheme::Null)
                    .with_curve(EccCurve::NistP256)
                    .with_is_decryption_key(true)
                    .with_is_signing_key(false)
                    .with_restricted(true)
                    .with_symmetric(SymmetricDefinitionObject::AES_128_CFB)
                    .build()
                    .map_err(backend)?,
            )
            .with_ecc_unique_identifier(EccPoint::default())
            .build()
            .map_err(backend)
    }

    /// Load (creating + persisting on first use) the key for `key_alias`, returning its handle.
    fn ensure_loaded(&self, key_alias: &str) -> Result<KeyHandle, HardwareSignerError> {
        if let Some(h) = self.loaded.lock().expect("loaded lock").get(key_alias) {
            return Ok(*h);
        }
        let mut ctx = self.context.lock().expect("ctx lock");
        let persistent = Self::persistent_handle(key_alias)?;

        // If the persistent key already exists, adopt its handle (idempotent across runs).
        if let Ok(handle) = ctx.tr_from_tpm_public(TpmHandle::Persistent(persistent)) {
            let key: KeyHandle = handle.into();
            self.loaded
                .lock()
                .expect("loaded lock")
                .insert(key_alias.to_owned(), key);
            return Ok(key);
        }

        // Otherwise create it under a fresh primary and evict it to the persistent handle.
        let primary = ctx
            .create_primary(
                Hierarchy::Owner,
                Self::primary_template()?,
                None,
                None,
                None,
                None,
            )
            .map_err(backend)?;
        let created = ctx
            .create(
                primary.key_handle,
                Self::signing_key_template()?,
                None,
                None,
                None,
                None,
            )
            .map_err(backend)?;
        let transient = ctx
            .load(primary.key_handle, created.out_private, created.out_public)
            .map_err(backend)?;
        let evicted = ctx
            .evict_control(
                Provision::Owner,
                transient.into(),
                Persistent::Persistent(persistent),
            )
            .map_err(backend)?;
        ctx.flush_context(transient.into()).ok();
        ctx.flush_context(primary.key_handle.into()).ok();
        let key = KeyHandle::from(evicted);

        self.loaded
            .lock()
            .expect("loaded lock")
            .insert(key_alias.to_owned(), key);
        Ok(key)
    }

    /// The uncompressed `x‖y` P-256 public point for an already-loaded key.
    fn public_point(ctx: &mut Context, key: KeyHandle) -> Result<Vec<u8>, HardwareSignerError> {
        let (public, _, _) = ctx.read_public(key).map_err(backend)?;
        match public {
            Public::Ecc { unique, .. } => {
                let mut out = Vec::with_capacity(64);
                out.extend_from_slice(unique.x().value());
                out.extend_from_slice(unique.y().value());
                Ok(out)
            }
            _ => Err(HardwareSignerError::Backend("not an ECC key".into())),
        }
    }
}

impl HardwareSigner for TpmSigner {
    fn enroll(&self, key_alias: String) -> Result<Vec<u8>, HardwareSignerError> {
        let key = self.ensure_loaded(&key_alias)?;
        let mut ctx = self.context.lock().expect("ctx lock");
        Self::public_point(&mut ctx, key)
    }

    fn classical_public_key(&self, key_alias: String) -> Result<Vec<u8>, HardwareSignerError> {
        let key = self.ensure_loaded(&key_alias)?;
        let mut ctx = self.context.lock().expect("ctx lock");
        Self::public_point(&mut ctx, key)
    }

    fn sign_classical(
        &self,
        key_alias: String,
        msg: Vec<u8>,
    ) -> Result<Vec<u8>, HardwareSignerError> {
        let key = self.ensure_loaded(&key_alias)?;
        let mut ctx = self.context.lock().expect("ctx lock");
        // Hash inside the TPM so it returns the validation ticket Sign needs. MaxBuffer caps a
        // single hash at 1024 bytes; a production adapter feeds longer inputs through a hash
        // sequence (TPM2_HashSequenceStart/Update/Complete).
        let data = MaxBuffer::try_from(msg).map_err(backend)?;
        let (digest, ticket) = ctx
            .hash(data, HashingAlgorithm::Sha256, Hierarchy::Owner)
            .map_err(backend)?;
        let sig = ctx
            .sign(key, digest, SignatureScheme::Null, ticket)
            .map_err(backend)?;
        match sig {
            Signature::EcDsa(ecdsa) => {
                let mut out = Vec::with_capacity(64);
                out.extend_from_slice(ecdsa.signature_r().value());
                out.extend_from_slice(ecdsa.signature_s().value());
                Ok(out)
            }
            _ => Err(HardwareSignerError::Backend(
                "unexpected signature scheme".into(),
            )),
        }
    }

    fn assert_non_exportable(&self, key_alias: String) -> Result<(), HardwareSignerError> {
        let key = self.ensure_loaded(&key_alias)?;
        let mut ctx = self.context.lock().expect("ctx lock");
        let (public, _, _) = ctx.read_public(key).map_err(backend)?;
        let attrs = match public {
            Public::Ecc {
                object_attributes, ..
            } => object_attributes,
            _ => return Err(HardwareSignerError::Backend("not an ECC key".into())),
        };
        if attrs.fixed_tpm() && attrs.fixed_parent() {
            Ok(())
        } else {
            Err(HardwareSignerError::Exportable)
        }
    }
}

/// Map any tss-esapi error into a [`HardwareSignerError::Backend`].
fn backend<E: std::fmt::Display>(e: E) -> HardwareSignerError {
    HardwareSignerError::Backend(e.to_string())
}
