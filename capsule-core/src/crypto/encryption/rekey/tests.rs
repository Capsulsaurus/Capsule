use super::*;
use crate::crypto::encryption::{PLAINTEXT_CHUNK, blob_nonce, decrypt_asset_vec, open_blob};

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

/// The streaming writer is the slice writer's own core: for the same AMK, `file_id` and
/// prefix it yields the same metadata, ciphertext and file key, on either side of a chunk
/// boundary and for the empty file — which is what keeps a streamed import's signed manifest
/// byte-identical to a buffered one's.
#[test]
fn streaming_writer_matches_the_slice_writer() {
    let amk = Amk::from_bytes([0x44; 32]);
    let file_id = Uuid::from_u128(0x5EED);
    let prefix = [3u8; NONCE_PREFIX_LEN];
    for len in [
        0,
        1,
        PLAINTEXT_CHUNK - 1,
        PLAINTEXT_CHUNK,
        2 * PLAINTEXT_CHUNK + 17,
    ] {
        let plaintext: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let (enc, ciphertext, key) =
            encrypt_asset_rekey_with_prefix(&amk, &file_id, &plaintext, prefix, None).unwrap();

        let mut streamed = Vec::new();
        let (s_enc, s_key) = encrypt_asset_rekey_stream_with_prefix(
            &amk,
            &file_id,
            plaintext.as_slice(),
            &mut streamed,
            prefix,
            None,
        )
        .unwrap();

        assert_eq!(s_enc, enc, "metadata for {len} bytes");
        assert_eq!(streamed, ciphertext, "ciphertext for {len} bytes");
        assert_eq!(s_key, key, "file key for {len} bytes");
    }
}

#[test]
fn streaming_writer_refuses_to_reuse_the_replaced_prefix_before_reading() {
    struct Unread;
    impl Read for Unread {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("the reuse refusal must come before any plaintext is read")
        }
    }
    let amk = Amk::from_bytes([0x44; 32]);
    let prefix = [9u8; NONCE_PREFIX_LEN];
    let refused = encrypt_asset_rekey_stream_with_prefix(
        &amk,
        &Uuid::from_u128(2),
        Unread,
        std::io::sink(),
        prefix,
        Some(prefix),
    );
    assert!(matches!(
        refused,
        Err(RekeyStreamError::Crypto(CryptoError::NonceReuse))
    ));
}

/// A reader that fails mid-stream is an I/O error from the streaming writer, not a panic and
/// not a short ciphertext reported as success.
#[test]
fn streaming_writer_reports_a_failing_reader() {
    struct Failing;
    impl Read for Failing {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk went away"))
        }
    }
    let amk = Amk::from_bytes([0x44; 32]);
    let failed =
        encrypt_asset_rekey_stream(&amk, &Uuid::from_u128(3), Failing, std::io::sink(), None);
    assert!(matches!(
        failed,
        Err(RekeyStreamError::Stream(StreamError::Io(_)))
    ));
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
        seal_metadata_blob_with_nonce(&amk, &blob_id, b"x", nonce, Some([1u8; NONCE_LEN])).is_ok()
    );
}
