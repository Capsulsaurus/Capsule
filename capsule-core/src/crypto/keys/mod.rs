//! Capsule's key hierarchy (SSoT: [Cryptography — Keys]).
//!
//! One backed-up root (the account master key) wraps device identity private keys and
//! anchors the AMK escrow. Album keys (AMKs) are random per-epoch keys; per-file keys are
//! derived from them. Device signing/encryption keys are hybrid [`HybridSigningKey`] /
//! [`kem::DekKeypair`].
//!
//! [Cryptography — Keys]: https://docs/design/cryptography/keys/

pub mod album;
// The sealed, durable album-key store (slice `S-A10`) is filesystem-backed — it writes through
// `utils::paths::tmp_path` for atomic replace — so it is gated with the rest of the native
// surface. Leaving it ungated broke the `wasm32-unknown-unknown` sealing build (`S-A6`), which
// no gate builds: `check-rust` compiles the host triple only.
#[cfg(feature = "native")]
pub mod albumstore;
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
// The Windows TPM 2.0 `HardwareSigner` over TBS (slice `S-F4`). The pure wire codec compiles on
// any host under `cfg(test)` (host-runnable mock tests); the TBS transport + signer are Windows
// only. Unlike `tpm` (the tss-esapi Linux reference), TBS needs no external crate — it links
// `tbs.dll` via `windows-sys`.
pub mod tbs;
#[cfg(feature = "tpm")]
pub mod tpm;

pub use album::{Amk, AmkVersion};
#[cfg(feature = "native")]
pub use albumstore::{AlbumStore, AlbumStoreError, AmkRow, PersistedAlbum, PersistedAuthority};
pub use directory::{DekPublic, DeviceDirectory, DeviceEntry, DirectoryCore};
pub use hardware::{
    HardwareBackedSigner, HardwareKeyAgreement, HardwareSigner, HardwareSignerError,
};
pub use hybrid_sig::{ClassicalAlgorithm, HybridSignature, HybridSigningKey, HybridVerifyingKey};
pub use kem::{DEK_CIPHERTEXT_LEN, DEK_PUBLIC_LEN, DekKeypair, encapsulate_to_public};
// Derandomized encapsulation + its eseed length: crate-internal, for the deterministic
// drop-seal known-answer path (`drop::seal_drop_derand`). Never on the public API.
pub(crate) use kem::{ESEED_LEN, encapsulate_to_public_derand};
pub use kem_p256::{
    DEK_P256_CIPHERTEXT_LEN, DEK_P256_PUBLIC_LEN, P256HybridDek, encapsulate_to_p256_public,
};
pub use keystore::{Account, AccountFile, DekBinding, DeviceDek, DeviceKeys};
pub use master::MasterKey;
pub use p256::P256HybridSigningKey;
pub use signer::Signer;
pub use software::SoftwareSigner;
#[cfg(windows)]
pub use tbs::TbsTpmSigner;
#[cfg(feature = "tpm")]
pub use tpm::TpmSigner;
