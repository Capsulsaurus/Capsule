//! Capsule's key hierarchy (SSoT: [Cryptography — Keys]).
//!
//! One backed-up root (the account master key) wraps device identity private keys and
//! anchors the AMK escrow. Album keys (AMKs) are random per-epoch keys; per-file keys are
//! derived from them. Device signing/encryption keys are hybrid [`HybridSigningKey`] /
//! [`kem::DekKeypair`].
//!
//! [Cryptography — Keys]: https://docs/design/cryptography/keys/

pub mod album;
pub mod hybrid_sig;
pub mod kem;
pub mod keystore;
pub mod master;

pub use album::{Amk, AmkVersion};
pub use hybrid_sig::{HybridSignature, HybridSigningKey, HybridVerifyingKey};
pub use kem::DekKeypair;
pub use keystore::{Account, AccountFile, DeviceKeys};
pub use master::MasterKey;
