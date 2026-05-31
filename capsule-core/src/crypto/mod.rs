//! Capsule's cryptographic data plane.
//!
//! Every primitive identity (hash, KDF, AEAD, signature scheme, suite id) is the
//! single source of truth declared in the design docs under `design/cryptography/`
//! and pinned in code by [`primitives`]. Submodules are layered strictly in
//! dependency order:
//!
//! ```text
//! hash · primitives · rng · kdf · pwkdf        (foundation, no internal deps)
//!   └─ keys ─ encryption                        (key hierarchy + AEAD)
//!   └─ authority ─┐
//!   └─ provenance ┴─ verify_asset               (the single acknowledgement chokepoint)
//! ```
//!
//! The cryptographic layer is deliberately self-contained and side-effect-free
//! (no network, no global state), so the whole data plane is unit-testable offline
//! against RFC / FIPS known-answer vectors and exhaustive negative cases.

pub mod encryption;
pub mod hash;
pub mod kdf;
pub mod keys;
pub mod primitives;
pub mod pwkdf;
pub mod rng;

pub use hash::{Hash32, Sha256Hasher};
pub use primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION, SuiteId};

use thiserror::Error;

/// Errors surfaced by the cryptographic layer.
///
/// Variants are intentionally coarse and carry a `&'static str` reason code rather
/// than free-form strings, so a rejection is greppable in logs and stable across
/// refactors (see [Threat Model — Validation] structured reason codes).
///
/// [Threat Model — Validation]: https://docs/design/threat-model/validation/
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CryptoError {
    /// An AEAD open / signature verification failed authentication.
    #[error("authentication failed: {0}")]
    Auth(&'static str),

    /// A structure declared a `crypto_suite_id` this build does not implement.
    #[error("unknown crypto suite id: {0:#06x}")]
    UnknownSuite(u16),

    /// A wire/on-disk structure was malformed (wrong length, bad framing, ...).
    #[error("malformed input: {0}")]
    Malformed(&'static str),

    /// A key could not be derived, decoded, or reconstructed from bytes.
    #[error("key error: {0}")]
    Key(&'static str),
}
