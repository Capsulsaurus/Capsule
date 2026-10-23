//! Desktop TPM 2.0 [`HardwareSigner`](capsule_core::crypto::keys::HardwareSigner)
//! implementations. They hold an open TPM context and persistent key handles, which is device
//! state, so they live here rather than in the stateless core that defines the trait.

// The Windows TPM 2.0 `HardwareSigner` over TBS (slice `S-F4`). The pure wire codec compiles on
// any host under `cfg(test)` (host-runnable mock tests); the TBS transport + signer are Windows
// only. Unlike `tpm` (the tss-esapi Linux reference), TBS needs no external crate — it links
// `tbs.dll` via `windows-sys`.
pub(crate) mod tbs;
// The tss-esapi reference signer: off by default and never built in CI, because it links the
// system `libtss2` and needs a real or software TPM to exercise.
#[cfg(feature = "tpm")]
pub(crate) mod tpm;

#[cfg(windows)]
pub use tbs::TbsTpmSigner;
#[cfg(feature = "tpm")]
pub use tpm::TpmSigner;
