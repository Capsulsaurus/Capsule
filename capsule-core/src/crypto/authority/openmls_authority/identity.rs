//! A device's MLS participation identity, and the **hybrid identity binding** that ties an MLS
//! LeafNode to Capsule's device identity layer.
//!
//! SSoT: [Cryptography — MLS](https://docs/design/cryptography/mls/) (leaf/identity binding) and
//! [Keys — Device Directory](https://docs/design/cryptography/keys/#device-directory).
//!
//! MLS binds LeafNode signatures to **Ed25519** in the X-Wing suite, so the post-quantum ML-DSA
//! half of Capsule's [hybrid signature scheme](https://docs/design/cryptography/primitives/#signature-scheme)
//! lives at the **identity layer**: a device's identity key (its DSK, published in the signed
//! [device directory](crate::crypto::keys::DeviceDirectory)) signs the device's MLS Ed25519 leaf
//! key with **both** Ed25519 and ML-DSA. Peers verify both halves before accepting the leaf into a
//! group — so a leaf whose Ed25519 key is not covered by a valid hybrid identity signature is
//! rejected, preserving PQ authentication end-to-end while MLS itself stays pure Ed25519.
//!
//! The binding rides the LeafNode's `BasicCredential` identity bytes as a canonical-CBOR
//! [`LeafBinding`]; [`verify_leaf_binding`] is the gate every membership ceremony runs before
//! admitting a KeyPackage or trusting a joiner.

use openmls::prelude::{
    BasicCredential, Credential, CredentialType, CredentialWithKey, KeyPackage, KeyPackageBundle,
};
use openmls_basic_credential::SignatureKeyPair;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::provider::CapsuleMlsProvider;
use super::{OpenMlsAuthorityError, PINNED_CIPHERSUITE};
use crate::crypto::keys::directory::{DeviceEntry, DirectoryCore};
use crate::crypto::keys::{DeviceDirectory, HybridSignature, HybridSigningKey};

/// The signed core of a [`LeafBinding`] — the bytes the device DSK's hybrid signature covers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LeafBindingCore {
    /// The account the device belongs to (must equal the device directory's `user_id`).
    pub user_id: Uuid,
    /// The device whose DSK signs this binding (looked up in the directory).
    pub device_id: Uuid,
    /// The device's MLS **Ed25519** leaf signature public key, as bound into the group.
    #[serde(with = "serde_bytes")]
    pub mls_signature_key: Vec<u8>,
}

impl LeafBindingCore {
    fn signing_bytes(&self) -> Result<Vec<u8>, OpenMlsAuthorityError> {
        crate::cbor::to_canonical_vec(self)
            .map_err(|e| OpenMlsAuthorityError::Binding(format!("encode: {e}")))
    }
}

/// The hybrid identity binding carried in an MLS LeafNode's `BasicCredential`: the device's DSK
/// (Ed25519 + ML-DSA) signature over its MLS Ed25519 leaf key. This is what makes a Capsule leaf
/// post-quantum-authenticated even though MLS signs leaves with Ed25519 alone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LeafBinding {
    /// The bound facts.
    pub core: LeafBindingCore,
    /// The device DSK's hybrid signature over [`LeafBindingCore::signing_bytes`].
    pub identity_sig: HybridSignature,
}

impl LeafBinding {
    /// Build a binding: sign `(user_id, device_id, mls_signature_key)` with the device DSK.
    fn create(
        user_id: Uuid,
        device_id: Uuid,
        mls_signature_key: Vec<u8>,
        dsk: &HybridSigningKey,
    ) -> Result<Self, OpenMlsAuthorityError> {
        let core = LeafBindingCore {
            user_id,
            device_id,
            mls_signature_key,
        };
        let identity_sig = dsk.sign(&core.signing_bytes()?);
        Ok(Self { core, identity_sig })
    }

    /// Canonical-CBOR encoding — the bytes placed in the LeafNode's `BasicCredential` identity.
    fn to_credential_bytes(&self) -> Result<Vec<u8>, OpenMlsAuthorityError> {
        crate::cbor::to_canonical_vec(self)
            .map_err(|e| OpenMlsAuthorityError::Binding(format!("encode: {e}")))
    }

    pub(super) fn from_credential_bytes(bytes: &[u8]) -> Result<Self, OpenMlsAuthorityError> {
        crate::cbor::from_slice(bytes)
            .map_err(|e| OpenMlsAuthorityError::Binding(format!("decode: {e}")))
    }
}

/// Verify a LeafNode's hybrid identity binding against a user's device directory.
///
/// Returns the bound `(user_id, device_id)` on success, or a [`OpenMlsAuthorityError::Binding`] if
/// **any** of the following fails — every one is a leaf that must not be admitted to the group:
/// 1. the credential is not a Capsule `BasicCredential` binding;
/// 2. the bound MLS key is not the LeafNode's actual signature key (transplant / substitution);
/// 3. the directory speaks for a different user than the binding claims;
/// 4. the claimed device is not in the directory;
/// 5. the device DSK's **hybrid** signature (Ed25519 **and** ML-DSA — both halves) does not verify.
///
/// `leaf_signature_key` is the LeafNode's own signature public key
/// (`key_package.leaf_node().signature_key().as_slice()`).
pub(crate) fn verify_leaf_binding(
    credential: &Credential,
    leaf_signature_key: &[u8],
    directory: &DeviceDirectory,
) -> Result<(Uuid, Uuid), OpenMlsAuthorityError> {
    if credential.credential_type() != CredentialType::Basic {
        return Err(OpenMlsAuthorityError::Binding(
            "leaf credential is not a Capsule basic-credential binding".into(),
        ));
    }
    let binding = LeafBinding::from_credential_bytes(credential.serialized_content())?;

    // The binding must cover *this* leaf's key, not some other key transplanted in.
    if binding.core.mls_signature_key != leaf_signature_key {
        return Err(OpenMlsAuthorityError::Binding(
            "leaf signature key is not the key covered by the identity binding".into(),
        ));
    }
    if directory.core.user_id != binding.core.user_id {
        return Err(OpenMlsAuthorityError::Binding(
            "binding user_id does not match the device directory".into(),
        ));
    }
    let Some(entry) = directory.device(&binding.core.device_id) else {
        return Err(OpenMlsAuthorityError::Binding(
            "binding device_id is not present in the device directory".into(),
        ));
    };
    // The load-bearing check: the device DSK's hybrid signature (both Ed25519 and ML-DSA halves,
    // enforced inside `HybridVerifyingKey::verify`) must cover the MLS leaf key.
    if !entry
        .dsk_public
        .verify(&binding.core.signing_bytes()?, &binding.identity_sig)
    {
        return Err(OpenMlsAuthorityError::Binding(
            "device DSK hybrid signature over the MLS leaf key did not verify".into(),
        ));
    }
    Ok((binding.core.user_id, binding.core.device_id))
}

/// One device's MLS participation identity: the owned OpenMLS provider (`CapsuleMlsProvider`)
/// (crypto + rand + serializable storage), the device's **MLS Ed25519 leaf signer**, and the
/// device's **hybrid DSK identity key** used to attest the leaf binding.
///
/// This is the unit that founds an album ([`OpenMlsAuthority::create_album`](super::OpenMlsAuthority::create_album))
/// or joins one ([`OpenMlsAuthority::join_via_welcome`](super::OpenMlsAuthority::join_via_welcome));
/// a would-be joiner uses it to publish a [`key_package`](Self::key_package) first.
pub struct MlsDeviceIdentity {
    pub(super) user_id: Uuid,
    pub(super) device_id: Uuid,
    /// The device identity key (published in the device directory); attests the MLS leaf binding.
    pub(super) dsk: HybridSigningKey,
    /// The MLS Ed25519 leaf signature keypair that authors this device's commits.
    pub(super) mls_signer: SignatureKeyPair,
    /// The owned crypto/rand/storage provider (its storage is the durable group state).
    pub(super) provider: CapsuleMlsProvider,
}

impl MlsDeviceIdentity {
    /// Create a device identity with a freshly-generated DSK (software Ed25519 + ML-DSA-65).
    pub fn generate(user_id: Uuid, device_id: Uuid) -> Result<Self, OpenMlsAuthorityError> {
        Self::with_dsk(user_id, device_id, HybridSigningKey::generate())
    }

    /// Create a device identity around a caller-supplied DSK — the device identity key that is (or
    /// will be) published in the user's [`DeviceDirectory`]. The MLS leaf signer is minted fresh
    /// and stored in the provider so the group can author commits with it.
    pub fn with_dsk(
        user_id: Uuid,
        device_id: Uuid,
        dsk: HybridSigningKey,
    ) -> Result<Self, OpenMlsAuthorityError> {
        let provider = CapsuleMlsProvider::new()?;
        let mls_signer = SignatureKeyPair::new(PINNED_CIPHERSUITE.signature_algorithm())
            .map_err(|e| OpenMlsAuthorityError::Signature(format!("{e:?}")))?;
        Ok(Self {
            user_id,
            device_id,
            dsk,
            mls_signer,
            provider,
        })
    }

    /// The account this device belongs to.
    pub fn user_id(&self) -> Uuid {
        self.user_id
    }

    /// This device's id.
    pub fn device_id(&self) -> Uuid {
        self.device_id
    }

    /// Persist the MLS leaf signer into the provider storage so the group can load it. Idempotent.
    pub(super) fn store_signer(&self) -> Result<(), OpenMlsAuthorityError> {
        self.mls_signer
            .store(<CapsuleMlsProvider as openmls_traits::OpenMlsProvider>::storage(&self.provider))
            .map_err(|e| OpenMlsAuthorityError::Signature(format!("store: {e:?}")))
    }

    /// The MLS [`CredentialWithKey`] for this device — a `BasicCredential` carrying the hybrid
    /// [`LeafBinding`] over this device's MLS Ed25519 leaf key.
    pub(super) fn credential_with_key(&self) -> Result<CredentialWithKey, OpenMlsAuthorityError> {
        let mls_pub = self.mls_signer.to_public_vec();
        let binding =
            LeafBinding::create(self.user_id, self.device_id, mls_pub.clone(), &self.dsk)?;
        let credential = BasicCredential::new(binding.to_credential_bytes()?);
        Ok(CredentialWithKey {
            credential: credential.into(),
            signature_key: mls_pub.into(),
        })
    }

    /// Publish a [`KeyPackage`] for this device so an admin can add it to an album. The bundle
    /// (with its private init/encryption keys) is stored in the provider for later Welcome
    /// processing; the returned public [`KeyPackage`] is what the admin consumes.
    pub fn key_package(&self) -> Result<KeyPackage, OpenMlsAuthorityError> {
        self.store_signer()?;
        let cwk = self.credential_with_key()?;
        let bundle: KeyPackageBundle = KeyPackage::builder()
            .build(PINNED_CIPHERSUITE, &self.provider, &self.mls_signer, cwk)
            .map_err(|e| OpenMlsAuthorityError::KeyPackage(format!("{e:?}")))?;
        Ok(bundle.key_package().clone())
    }

    /// This device's [`DeviceEntry`] (its DSK public half) for inclusion in a directory.
    pub fn directory_entry(&self, added_at: &str) -> DeviceEntry {
        DeviceEntry {
            device_id: self.device_id,
            dsk_public: self.dsk.verifying_key(),
            dek_public: None,
            added_at: added_at.to_string(),
            revoked_at: None,
        }
    }

    /// A single-device [`DeviceDirectory`] for this device, signed by the user IK `ik` — the trust
    /// anchor `verify_leaf_binding` (and `verify_asset`) resolve the device's DSK through.
    pub fn signed_directory(
        &self,
        ik: &HybridSigningKey,
        directory_version: u64,
        updated_at: &str,
    ) -> DeviceDirectory {
        DirectoryCore {
            user_id: self.user_id,
            directory_version,
            updated_at: updated_at.to_string(),
            devices: vec![self.directory_entry(updated_at)],
        }
        .sign(ik)
    }
}
