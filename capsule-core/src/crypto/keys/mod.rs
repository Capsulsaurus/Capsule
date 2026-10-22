//! Capsule's key hierarchy (SSoT: [Cryptography — Keys]).
//!
//! One backed-up root (the account master key) wraps device identity private keys and
//! anchors the AMK escrow. Album keys (AMKs) are random per-epoch keys; per-file keys are
//! derived from them. Device signing/encryption keys are hybrid [`HybridSigningKey`] /
//! [`kem::DekKeypair`].
//!
//! [Cryptography — Keys]: https://docs/design/cryptography/keys/

pub(crate) mod album;
// The sealed album-key store (slice `S-A10`): its format, sealing and escrow. Persisting it to
// `{root}/.library/albums.cbor` is `capsule-library`'s. Gated with the rest of the native surface,
// which no browser sealing path needs.
#[cfg(feature = "native")]
pub mod albumstore;
pub mod directory;
pub(crate) mod hardware;
pub(crate) mod hybrid_sig;
pub(crate) mod kem;
pub(crate) mod kem_p256;
pub(crate) mod keystore;
pub(crate) mod master;
pub(crate) mod p256;
pub(crate) mod safety_code;
pub(crate) mod signer;
pub(crate) mod software;

pub use album::{Amk, AmkVersion};
#[cfg(feature = "native")]
pub use albumstore::{
    ALBUM_STORE_FILE, AlbumStore, AlbumStoreError, AmkRow, EscrowCoverage, PersistedAlbum,
    PersistedAuthority,
};
pub use directory::{DekPublic, DeviceDirectory, DeviceEntry, DirectoryCore, DirectoryError};
// The in-memory secure-element doubles, for `capsule-library`'s and the FFI's tests. Never in a
// shipping build: `test-support` rides dev-dependencies only.
#[cfg(any(test, feature = "test-support"))]
pub use hardware::MockHardwareSigner;
pub use hardware::{
    HardwareBackedSigner, HardwareKeyAgreement, HardwareSigner, HardwareSignerError,
};
pub use hybrid_sig::{ClassicalAlgorithm, HybridSignature, HybridSigningKey, HybridVerifyingKey};
// Derandomized encapsulation: crate-internal, for the deterministic drop-seal known-answer path
// (`drop::seal_drop_derand`). Never on the public API. Its eseed length is public, because
// `seal_drop_derand` (already public, for the KAT generator) takes a seed of that length.
pub use kem::ESEED_LEN;
pub(crate) use kem::encapsulate_to_public_derand;
pub use kem::{DEK_CIPHERTEXT_LEN, DEK_PUBLIC_LEN, DekKeypair, encapsulate_to_public};
#[cfg(any(test, feature = "test-support"))]
pub use kem_p256::MockP256KeyAgreement;
pub use kem_p256::{
    DEK_P256_CIPHERTEXT_LEN, DEK_P256_PUBLIC_LEN, P256HybridDek, encapsulate_to_p256_public,
};
pub use keystore::{Account, AccountFile, DekBinding, DeviceDek, DeviceKeys};
pub use master::MasterKey;
pub use p256::P256HybridSigningKey;
pub use safety_code::{
    KEY_COMMITMENT_LEN, SAFETY_CODE_CHUNK, SAFETY_CODE_DIGITS, commitment_matches, key_commitment,
    safety_code,
};
pub use signer::Signer;
pub use software::SoftwareSigner;
