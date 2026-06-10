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
pub mod keystore;
pub mod master;
pub mod signer;
pub mod software;
#[cfg(feature = "tpm")]
pub mod tpm;

pub use album::{Amk, AmkVersion};
pub use directory::{DeviceDirectory, DeviceEntry, DirectoryCore};
pub use hardware::{HardwareBackedSigner, HardwareSigner, HardwareSignerError};
pub use hybrid_sig::{HybridSignature, HybridSigningKey, HybridVerifyingKey};
pub use kem::DekKeypair;
pub use keystore::{Account, AccountFile, DeviceKeys};
pub use master::MasterKey;
pub use signer::Signer;
pub use software::SoftwareSigner;
#[cfg(feature = "tpm")]
pub use tpm::TpmSigner;
