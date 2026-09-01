//! Re-keying on rewrite — the writers that fold a fresh nonce into the key salt so a
//! same-epoch rewrite re-rolls **both** the key and the nonce, and refuse to reuse the
//! nonce of the record they supersede.
//!
//! A [`replace`] keeps a file's `file_id` and can land in the *same* AMK epoch as the bytes
//! it supersedes; a `metadata-update` keeps a blob's `blob_id`. Because the fresh
//! `nonce_prefix` / blob `nonce` is folded into the key salt
//! ([`Amk::derive_file_key`] / [`Amk::derive_blob_key`]), re-encrypting byte-identical
//! plaintext under the same id and epoch yields a different key, a different nonce, and
//! different ciphertext — so AES-GCM nonce reuse is structurally impossible.
//!
//! These writers **couple the nonce draw to derivation** (the salt depends on the nonce, so
//! the two cannot be separated) and add the defense-in-depth check that a freshly drawn
//! nonce never equals the one being replaced (on top of the CSPRNG draw). A first write
//! passes `replaces = None`; a rewrite passes the superseded nonce. SSoT: [Encryption §
//! Re-keying on Rewrite].
//!
//! [`replace`]: https://docs/design/cryptography/encryption/#re-keying-on-rewrite
//! [Encryption § Re-keying on Rewrite]: https://docs/design/cryptography/encryption/#re-keying-on-rewrite

use uuid::Uuid;

use super::blob::{NONCE_LEN, seal_blob_with_nonce};
use super::stream::{AssetEncryption, NONCE_PREFIX_LEN, encrypt_asset_vec_with_prefix};
use crate::crypto::keys::Amk;
use crate::crypto::{CryptoError, rng};

/// Encrypt `plaintext` as a first or re-rolled asset write under `amk` scoped to `file_id`:
/// draw a fresh `nonce_prefix`, fold it into the file-key salt, and STREAM-encrypt.
///
/// `replaces` is the `nonce_prefix` of the version being superseded (a same-epoch
/// [`replace`]), if any; the writer refuses to emit that exact prefix again. Returns the
/// [`AssetEncryption`] metadata, the ciphertext, and the derived file key.
///
/// [`replace`]: https://docs/design/cryptography/encryption/#re-keying-on-rewrite
pub fn encrypt_asset_rekey(
    amk: &Amk,
    file_id: &Uuid,
    plaintext: &[u8],
    replaces: Option<[u8; NONCE_PREFIX_LEN]>,
) -> Result<(AssetEncryption, Vec<u8>, [u8; 32]), CryptoError> {
    encrypt_asset_rekey_with_prefix(
        amk,
        file_id,
        plaintext,
        rng::random_array::<NONCE_PREFIX_LEN>(),
        replaces,
    )
}

/// [`encrypt_asset_rekey`] with an explicit `nonce_prefix` — the deterministic, testable
/// core of the fold and the reuse refusal.
pub fn encrypt_asset_rekey_with_prefix(
    amk: &Amk,
    file_id: &Uuid,
    plaintext: &[u8],
    nonce_prefix: [u8; NONCE_PREFIX_LEN],
    replaces: Option<[u8; NONCE_PREFIX_LEN]>,
) -> Result<(AssetEncryption, Vec<u8>, [u8; 32]), CryptoError> {
    if replaces == Some(nonce_prefix) {
        return Err(CryptoError::NonceReuse);
    }
    let file_key = amk.derive_file_key(file_id, &nonce_prefix);
    let (enc, ciphertext) = encrypt_asset_vec_with_prefix(&file_key, nonce_prefix, plaintext);
    Ok((enc, ciphertext, file_key))
}

/// Seal `plaintext` (canonical CBOR sidecar) into a metadata blob under `amk` scoped to
/// `blob_id`: draw a fresh blob `nonce`, fold it into the blob-key salt, and AEAD-seal.
///
/// `replaces` is the prior blob's `nonce` on a `metadata-update`, refused if drawn again.
/// Returns the wire bytes and the derived blob key (for the immediate binding self-check).
pub fn seal_metadata_blob(
    amk: &Amk,
    blob_id: &Uuid,
    plaintext: &[u8],
    replaces: Option<[u8; NONCE_LEN]>,
) -> Result<(Vec<u8>, [u8; 32]), CryptoError> {
    seal_metadata_blob_with_nonce(
        amk,
        blob_id,
        plaintext,
        rng::random_array::<NONCE_LEN>(),
        replaces,
    )
}

/// [`seal_metadata_blob`] with an explicit `nonce` — the deterministic, testable core.
pub fn seal_metadata_blob_with_nonce(
    amk: &Amk,
    blob_id: &Uuid,
    plaintext: &[u8],
    nonce: [u8; NONCE_LEN],
    replaces: Option<[u8; NONCE_LEN]>,
) -> Result<(Vec<u8>, [u8; 32]), CryptoError> {
    if replaces == Some(nonce) {
        return Err(CryptoError::NonceReuse);
    }
    let blob_key = amk.derive_blob_key(blob_id, &nonce);
    let wire = seal_blob_with_nonce(&blob_key, nonce, plaintext);
    Ok((wire, blob_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::encryption::blob::{blob_nonce, open_blob};
    use crate::crypto::encryption::stream::decrypt_asset_vec;

    /// The doc's rewrite re-roll case: a same-`file_id`, same-epoch `replace` of byte-identical
    /// plaintext produces a different key AND nonce, so no `(key, nonce)` pair repeats — and
    /// each version still round-trips under its own derived key + nonce.
    #[test]
    fn asset_replace_rerolls_key_and_nonce() {
        let amk = Amk::from_bytes([0x21; 32]);
        let file_id = Uuid::from_u128(0xF11E);
        let plaintext = b"identical plaintext across a same-epoch replace";

        let (e1, c1, k1) = encrypt_asset_rekey(&amk, &file_id, plaintext, None).unwrap();
        // The replace supersedes the first prefix and must not reuse it.
        let (e2, c2, k2) =
            encrypt_asset_rekey(&amk, &file_id, plaintext, Some(e1.nonce_prefix)).unwrap();

        assert_ne!(e1.nonce_prefix, e2.nonce_prefix, "a fresh nonce prefix");
        assert_ne!(
            k1, k2,
            "the folded salt re-rolls the key, not merely the nonce"
        );
        assert_ne!(c1, c2, "different key + nonce → different ciphertext");

        assert_eq!(
            decrypt_asset_vec(&k1, &e1.nonce_prefix, &c1).unwrap(),
            plaintext
        );
        assert_eq!(
            decrypt_asset_vec(&k2, &e2.nonce_prefix, &c2).unwrap(),
            plaintext
        );
    }

    #[test]
    fn asset_writer_refuses_to_reuse_the_replaced_prefix() {
        let amk = Amk::from_bytes([0x21; 32]);
        let file_id = Uuid::from_u128(1);
        let prefix = [7u8; NONCE_PREFIX_LEN];
        assert_eq!(
            encrypt_asset_rekey_with_prefix(&amk, &file_id, b"x", prefix, Some(prefix)),
            Err(CryptoError::NonceReuse),
        );
        // A prefix distinct from the one it replaces is accepted.
        assert!(
            encrypt_asset_rekey_with_prefix(
                &amk,
                &file_id,
                b"x",
                prefix,
                Some([8u8; NONCE_PREFIX_LEN])
            )
            .is_ok()
        );
        // A first write (nothing to replace) is always accepted.
        assert!(encrypt_asset_rekey_with_prefix(&amk, &file_id, b"x", prefix, None).is_ok());
    }

    /// The companion re-roll case: a `metadata-update` re-seals under the constant `blob_id`
    /// and the key and nonce both change.
    #[test]
    fn metadata_update_rerolls_key_and_nonce() {
        let amk = Amk::from_bytes([0x33; 32]);
        let blob_id = Uuid::from_u128(0xB10B);
        let cbor = b"deterministic sidecar CBOR bytes";

        let (w1, bk1) = seal_metadata_blob(&amk, &blob_id, cbor, None).unwrap();
        let n1 = blob_nonce(&w1).unwrap();
        let (w2, bk2) = seal_metadata_blob(&amk, &blob_id, cbor, Some(n1)).unwrap();
        let n2 = blob_nonce(&w2).unwrap();

        assert_ne!(n1, n2, "a fresh blob nonce");
        assert_ne!(bk1, bk2, "the folded salt re-rolls the blob key");
        assert_ne!(w1, w2, "different key + nonce → different blob bytes");

        assert_eq!(open_blob(&bk1, &w1).unwrap(), cbor);
        assert_eq!(open_blob(&bk2, &w2).unwrap(), cbor);
    }

    #[test]
    fn metadata_writer_refuses_to_reuse_the_replaced_nonce() {
        let amk = Amk::from_bytes([0x33; 32]);
        let blob_id = Uuid::from_u128(1);
        let nonce = [9u8; NONCE_LEN];
        assert_eq!(
            seal_metadata_blob_with_nonce(&amk, &blob_id, b"x", nonce, Some(nonce)),
            Err(CryptoError::NonceReuse),
        );
        assert!(
            seal_metadata_blob_with_nonce(&amk, &blob_id, b"x", nonce, Some([1u8; NONCE_LEN]))
                .is_ok()
        );
    }
}
