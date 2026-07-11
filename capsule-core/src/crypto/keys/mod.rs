//! Capsule's key hierarchy (SSoT: [Cryptography — Keys]).
//!
//! One backed-up root (the account master key) wraps device identity private keys and
//! anchors the AMK escrow. Album keys (AMKs) are random per-epoch keys; per-file keys are
//! derived from them. Device signing/encryption keys are hybrid [`HybridSigningKey`] /
//! [`kem::DekKeypair`].
//!
//! [Cryptography — Keys]: https://docs/design/cryptography/keys/

pub mod album;
pub mod directory;
pub mod hardware;
pub mod hybrid_sig;
pub mod kem;
pub mod kem_p256;
pub mod keystore;
pub mod master;
pub mod p256;
pub mod signer;
pub mod software;
#[cfg(feature = "tpm")]
pub mod tpm;

pub use album::{Amk, AmkVersion};
pub use directory::{DeviceDirectory, DeviceEntry, DirectoryCore};
pub use hardware::{
    HardwareBackedSigner, HardwareKeyAgreement, HardwareSigner, HardwareSignerError,
};
pub use hybrid_sig::{ClassicalAlgorithm, HybridSignature, HybridSigningKey, HybridVerifyingKey};
pub use kem::{DEK_CIPHERTEXT_LEN, DEK_PUBLIC_LEN, DekKeypair, encapsulate_to_public};
pub use kem_p256::{
    DEK_P256_CIPHERTEXT_LEN, DEK_P256_PUBLIC_LEN, P256HybridDek, encapsulate_to_p256_public,
};
pub use keystore::{Account, AccountFile, DeviceKeys};
pub use master::MasterKey;
pub use p256::P256HybridSigningKey;
pub use signer::Signer;
pub use software::SoftwareSigner;
#[cfg(feature = "tpm")]
pub use tpm::TpmSigner;
