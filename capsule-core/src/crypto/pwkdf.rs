//! Password-based key wrapping: Argon2id derives a wrapping key from a passphrase /
//! recovery code, which then seals a secret under AES-256-GCM. Used for the master-key
//! escrow and backup wrap key (SSoT: [Cryptography — Primitives § Password-based KDF]).
//!
//! Argon2id runs only at account recovery and device bootstrap — never on a hot path.
//! The cost parameters are recorded **inside** the wrapped blob, so a desktop-wrapped
//! blob unwraps correctly on a phone (slowly) and vice versa, and parameters can be
//! raised over time without a flag day.
//!
//! [Cryptography — Primitives § Password-based KDF]: https://docs/design/cryptography/primitives/#password-based-kdf

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use serde::{Deserialize, Serialize};

use super::primitives::{Argon2Params, DeviceTier};
use super::{CryptoError, rng};

/// A secret sealed under a passphrase-derived key, self-describing for unwrap.
///
/// The Argon2id parameters and salt travel with the ciphertext so any device can
/// reconstruct the wrapping key from the passphrase alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrappedSecret {
    /// Argon2id memory cost (KiB) used at wrap time.
    pub mem_kib: u32,
    /// Argon2id iteration cost `t`.
    pub t_cost: u32,
    /// Argon2id parallelism `p`.
    pub p_cost: u32,
    /// 32-byte CSPRNG salt for Argon2id.
    pub salt: [u8; 32],
    /// 12-byte AES-256-GCM nonce.
    pub nonce: [u8; 12],
    /// AES-256-GCM ciphertext of the secret (includes the 16-byte tag).
    pub ciphertext: Vec<u8>,
}

/// Derive a 32-byte wrapping key from a passphrase and salt via Argon2id. Exposed so the
/// backup artifact can use one passphrase-derived key for both its MANIFEST HMAC and its
/// AMK-ledger seal (SSoT: [Backup — Master-Key Escrow]).
///
/// [Backup — Master-Key Escrow]: https://docs/design/backup-recovery/#master-key-escrow
pub fn derive_wrap_key(
    passphrase: &[u8],
    salt: &[u8],
    p: Argon2Params,
) -> Result<[u8; 32], CryptoError> {
    let params = Params::new(p.mem_kib, p.t_cost, p.p_cost, Some(32))
        .map_err(|_| CryptoError::Key("invalid Argon2 parameters"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon
        .hash_password_into(passphrase, salt, &mut key)
        .map_err(|_| CryptoError::Key("Argon2id derivation failed"))?;
    Ok(key)
}

/// Wrap `secret` under `passphrase` with explicit Argon2id parameters (recorded in-band).
pub fn wrap_with(
    secret: &[u8],
    passphrase: &[u8],
    params: Argon2Params,
) -> Result<WrappedSecret, CryptoError> {
    let salt = rng::random_array::<32>();
    let nonce = rng::random_array::<12>();
    let wrap_key = derive_wrap_key(passphrase, &salt, params)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&wrap_key));
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), secret)
        .map_err(|_| CryptoError::Auth("AES-GCM wrap failed"))?;
    Ok(WrappedSecret {
        mem_kib: params.mem_kib,
        t_cost: params.t_cost,
        p_cost: params.p_cost,
        salt,
        nonce,
        ciphertext,
    })
}

/// Wrap `secret` under `passphrase` using the canonical parameters for `tier`.
pub fn wrap(
    secret: &[u8],
    passphrase: &[u8],
    tier: DeviceTier,
) -> Result<WrappedSecret, CryptoError> {
    wrap_with(secret, passphrase, tier.params())
}

/// Unwrap a [`WrappedSecret`], re-deriving the wrapping key from the in-band parameters.
/// Returns [`CryptoError::Auth`] on a wrong passphrase or a corrupt/tampered blob.
pub fn unwrap(blob: &WrappedSecret, passphrase: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let params = Argon2Params {
        mem_kib: blob.mem_kib,
        t_cost: blob.t_cost,
        p_cost: blob.p_cost,
    };
    let wrap_key = derive_wrap_key(passphrase, &blob.salt, params)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&wrap_key));
    cipher
        .decrypt(Nonce::from_slice(&blob.nonce), blob.ciphertext.as_slice())
        .map_err(|_| CryptoError::Auth("unwrap: wrong passphrase or corrupt blob"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tiny parameters keep the password-hash tests fast; the production tier table is
    // asserted in `primitives` without paying the 128–512 MiB hashing cost.
    fn fast() -> Argon2Params {
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        }
    }

    #[test]
    fn wrap_unwrap_round_trip() {
        let secret = [0x42u8; 32];
        let blob = wrap_with(&secret, b"correct horse battery staple", fast()).unwrap();
        let out = unwrap(&blob, b"correct horse battery staple").unwrap();
        assert_eq!(out, secret);
    }

    #[test]
    fn wrong_passphrase_is_rejected() {
        let blob = wrap_with(&[1u8; 32], b"right", fast()).unwrap();
        assert_eq!(
            unwrap(&blob, b"wrong"),
            Err(CryptoError::Auth(
                "unwrap: wrong passphrase or corrupt blob"
            ))
        );
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let mut blob = wrap_with(&[9u8; 16], b"pw", fast()).unwrap();
        blob.ciphertext[0] ^= 0x01;
        assert!(unwrap(&blob, b"pw").is_err());
    }

    #[test]
    fn parameters_recorded_in_band_enable_cross_tier_unwrap() {
        // Wrap with one parameter set; unwrap reads the params from the blob, not from a
        // caller-supplied tier — so a blob made on a "desktop" opens on a "phone".
        let desktopish = Argon2Params {
            mem_kib: 96,
            t_cost: 2,
            p_cost: 1,
        };
        let blob = wrap_with(b"master-key-bytes", b"passphrase", desktopish).unwrap();
        assert_eq!(blob.mem_kib, 96);
        assert_eq!(blob.t_cost, 2);
        assert_eq!(unwrap(&blob, b"passphrase").unwrap(), b"master-key-bytes");
    }

    #[test]
    fn distinct_salts_make_blobs_unique() {
        let a = wrap_with(&[0u8; 32], b"pw", fast()).unwrap();
        let b = wrap_with(&[0u8; 32], b"pw", fast()).unwrap();
        assert_ne!(a.salt, b.salt);
        assert_ne!(a.ciphertext, b.ciphertext);
    }
}
