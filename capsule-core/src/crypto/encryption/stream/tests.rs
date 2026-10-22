use std::io::Cursor;

use super::*;

const KEY: [u8; 32] = [0x11; 32];

fn round_trip(plaintext: &[u8]) {
    let (meta, ct) = encrypt_asset_vec_full(&KEY, plaintext);
    assert_eq!(meta.plaintext_size, plaintext.len() as u64);
    assert_eq!(meta.chunk_size, PLAINTEXT_CHUNK as u32);
    assert_eq!(ct.len() as u64, ciphertext_len(plaintext.len() as u64));
    let back = decrypt_asset_vec(&KEY, &meta.nonce_prefix, &ct).unwrap();
    assert_eq!(back, plaintext);
    // Content hash matches a fresh hash of the ciphertext.
    assert_eq!(meta.ciphertext_hash, crate::crypto::hash::hash_bytes(&ct));
}

/// Plaintext of `len` bytes whose every chunk differs, so a swapped chunk is visible.
fn patterned(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Decrypt `ct` through a [`StreamDecryptor`] fed `piece`-byte slices.
fn decrypt_in_pieces(
    key: &[u8; 32],
    prefix: &[u8; NONCE_PREFIX_LEN],
    ct: &[u8],
    piece: usize,
) -> Result<Vec<u8>, StreamError> {
    let mut decryptor = StreamDecryptor::new(key, prefix, Vec::new());
    for slice in ct.chunks(piece.max(1)) {
        decryptor.update(slice)?;
    }
    decryptor.finish()
}

#[test]
fn round_trip_various_sizes() {
    round_trip(b""); // empty
    round_trip(b"hello"); // < 1 chunk
    round_trip(&vec![0xABu8; PLAINTEXT_CHUNK]); // exactly one chunk
    round_trip(&vec![0xCDu8; PLAINTEXT_CHUNK + 1]); // one chunk + 1
    round_trip(&vec![0x77u8; PLAINTEXT_CHUNK * 3 + 123]); // multi-chunk, partial last
}

#[test]
fn full_chunk_ciphertext_is_64_kib() {
    let (_, ct) = encrypt_asset_vec_full(&KEY, &vec![0u8; PLAINTEXT_CHUNK * 2]);
    // Two full plaintext chunks → two 64 KiB ciphertext chunks.
    assert_eq!(ct.len(), CIPHERTEXT_CHUNK * 2);
}

#[test]
fn chunk_geometry() {
    assert_eq!(
        chunk_count(0),
        1,
        "an empty plaintext is one empty last chunk"
    );
    assert_eq!(chunk_count(1), 1);
    assert_eq!(chunk_count(PLAINTEXT_CHUNK as u64), 1);
    assert_eq!(chunk_count(PLAINTEXT_CHUNK as u64 + 1), 2);
    assert_eq!(ciphertext_len(0), TAG_LEN as u64);
    assert_eq!(
        ciphertext_len(PLAINTEXT_CHUNK as u64 * 2),
        CIPHERTEXT_CHUNK as u64 * 2
    );
    // 4 GiB: the size the server accepts, without overflow.
    let four_gib = 4u64 << 30;
    assert_eq!(
        ciphertext_len(four_gib),
        four_gib + four_gib.div_ceil(PLAINTEXT_CHUNK as u64) * TAG_LEN as u64
    );
}

/// Known-answer vectors, computed with an independent AES-256-GCM implementation (Python
/// `cryptography`) from the STREAM-BE32 nonce layout `prefix(7) ‖ counter_be32(4) ‖
/// last_flag(1)` and 65,520-byte plaintext chunks. They pin the format: any change to the chunk
/// size, the nonce layout, the last flag or the cipher fails here.
#[test]
fn known_answer_vectors() {
    let key: [u8; 32] = std::array::from_fn(|i| i as u8);
    let prefix: [u8; NONCE_PREFIX_LEN] = std::array::from_fn(|i| 0xa0 + i as u8);

    let (_, empty) = encrypt_asset_vec_with_prefix(&key, prefix, b"");
    assert_eq!(hex::encode(&empty), "4d34eaed9250e09e52907c89588102a2");

    let (_, short) = encrypt_asset_vec_with_prefix(&key, prefix, b"capsule");
    assert_eq!(
        hex::encode(&short),
        "1400396dcd6a85837fad72f96ca25730cf8f522b87764e"
    );

    let plaintext = patterned(PLAINTEXT_CHUNK + 100);
    let (meta, two) = encrypt_asset_vec_with_prefix(&key, prefix, &plaintext);
    assert_eq!(two.len(), 65_652);
    assert_eq!(
        meta.ciphertext_hash.to_hex(),
        "14e8b82470b8a3335f3552cb01ffccfd5db446ed71ae1c182669dc59bdfbc138"
    );
    assert_eq!(hex::encode(&two[..16]), "0ad01eaa9251433a642ace8b950d5864");
    assert_eq!(
        hex::encode(&two[two.len() - 16..]),
        "dbce2cbb8f31c10328c1407651a600d7"
    );
    assert_eq!(decrypt_asset_vec(&key, &prefix, &two).unwrap(), plaintext);
}

#[test]
fn ranged_read_matches_sequential() {
    let plaintext = patterned(PLAINTEXT_CHUNK * 3 + 500);
    let (meta, ct) = encrypt_asset_vec_full(&KEY, &plaintext);
    let n_chunks = ct.len().div_ceil(CIPHERTEXT_CHUNK);

    for i in 0..n_chunks {
        let start = i * CIPHERTEXT_CHUNK;
        let end = (start + CIPHERTEXT_CHUNK).min(ct.len());
        let is_last = i == n_chunks - 1;
        let pt =
            decrypt_chunk(&KEY, &meta.nonce_prefix, i as u32, is_last, &ct[start..end]).unwrap();

        let p_start = i * PLAINTEXT_CHUNK;
        let p_end = (p_start + PLAINTEXT_CHUNK).min(plaintext.len());
        assert_eq!(pt, &plaintext[p_start..p_end], "chunk {i} mismatch");
    }
}

#[test]
fn encrypt_chunk_matches_sequential() {
    let plaintext = patterned(PLAINTEXT_CHUNK * 2 + 7);
    let (meta, ct) = encrypt_asset_vec_full(&KEY, &plaintext);
    for (i, pt) in plaintext.chunks(PLAINTEXT_CHUNK).enumerate() {
        let is_last = i == 2;
        let start = i * CIPHERTEXT_CHUNK;
        let end = (start + CIPHERTEXT_CHUNK).min(ct.len());
        assert_eq!(
            encrypt_chunk(&KEY, &meta.nonce_prefix, i as u32, is_last, pt),
            &ct[start..end],
            "chunk {i}"
        );
    }
}

/// The re-derivation an upload resumes from: every window, at every alignment, is the same
/// bytes the sequential encryption produced.
#[test]
fn ciphertext_reader_windows_match_sequential() {
    let prefix = [3u8; NONCE_PREFIX_LEN];
    for size in [
        0,
        1,
        PLAINTEXT_CHUNK,
        PLAINTEXT_CHUNK + 1,
        PLAINTEXT_CHUNK * 3 + 17,
    ] {
        let plaintext = patterned(size);
        let (_, ct) = encrypt_asset_vec_with_prefix(&KEY, prefix, &plaintext);
        let mut reader = CiphertextReader::new(KEY, prefix, Cursor::new(&plaintext), size as u64);
        assert_eq!(reader.ciphertext_size(), ct.len() as u64);

        // A tiny window re-encrypts a whole chunk per read, so it only runs on tiny inputs.
        let tiny: &[usize] = if size <= 1 { &[1, 15] } else { &[] };
        let windows = [4093, CIPHERTEXT_CHUNK - 1, CIPHERTEXT_CHUNK, 200_000];
        for &window in tiny.iter().chain(&windows) {
            let mut offset = 0u64;
            let mut assembled = Vec::new();
            while offset < ct.len() as u64 {
                let got = reader.read_at(offset, window).unwrap();
                assert!(
                    !got.is_empty(),
                    "size {size} window {window} offset {offset}"
                );
                assembled.extend_from_slice(&got);
                offset += got.len() as u64;
            }
            assert_eq!(assembled, ct, "size {size} window {window}");
        }
        // Past the end is empty, and a window is clamped to the end.
        assert!(reader.read_at(ct.len() as u64, 10).unwrap().is_empty());
        let tail = reader.read_at(ct.len() as u64 - 3, 100).unwrap();
        assert_eq!(tail, &ct[ct.len() - 3..]);
    }
}

#[test]
fn ciphertext_reader_refuses_a_short_source() {
    let plaintext = patterned(100);
    let mut reader =
        CiphertextReader::new(KEY, [0; NONCE_PREFIX_LEN], Cursor::new(&plaintext), 200);
    let err = reader.read_at(0, 50).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn stream_decryptor_accepts_any_piece_size() {
    for size in [
        0,
        5,
        PLAINTEXT_CHUNK,
        PLAINTEXT_CHUNK * 2,
        PLAINTEXT_CHUNK * 3 + 9,
    ] {
        let plaintext = patterned(size);
        let (meta, ct) = encrypt_asset_vec_full(&KEY, &plaintext);
        for piece in [
            1,
            17,
            CIPHERTEXT_CHUNK - 1,
            CIPHERTEXT_CHUNK,
            CIPHERTEXT_CHUNK + 1,
            1 << 20,
        ] {
            let back = decrypt_in_pieces(&KEY, &meta.nonce_prefix, &ct, piece).unwrap();
            assert_eq!(back, plaintext, "size {size} piece {piece}");
        }
    }
}

#[test]
fn stream_decryptor_is_an_io_sink() {
    let plaintext = patterned(PLAINTEXT_CHUNK + 3);
    let (meta, ct) = encrypt_asset_vec_full(&KEY, &plaintext);
    let mut decryptor = StreamDecryptor::new(&KEY, &meta.nonce_prefix, Vec::new());
    io::copy(&mut ct.as_slice(), &mut decryptor).unwrap();
    assert_eq!(decryptor.finish().unwrap(), plaintext);

    let mut tampered = ct.clone();
    tampered[3] ^= 1;
    let mut decryptor = StreamDecryptor::new(&KEY, &meta.nonce_prefix, Vec::new());
    let err = io::copy(&mut tampered.as_slice(), &mut decryptor).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

/// Every way of damaging a ciphertext fails both the whole-buffer and the incremental path.
fn assert_rejected(prefix: &[u8; NONCE_PREFIX_LEN], ct: &[u8], what: &str) {
    assert!(
        decrypt_asset_vec(&KEY, prefix, ct).is_err(),
        "{what}: decrypt_asset"
    );
    for piece in [1, 4096, CIPHERTEXT_CHUNK + 1] {
        assert!(
            decrypt_in_pieces(&KEY, prefix, ct, piece).is_err(),
            "{what}: StreamDecryptor, piece {piece}"
        );
    }
}

#[test]
fn tamper_in_each_chunk_is_detected() {
    let plaintext = vec![0x5Au8; PLAINTEXT_CHUNK * 2 + 10];
    let (meta, ct) = encrypt_asset_vec_full(&KEY, &plaintext);

    let mut t = ct.clone();
    t[10] ^= 0x01;
    assert_rejected(&meta.nonce_prefix, &t, "bit flip in the first chunk");

    let mut t = ct.clone();
    let last = t.len() - 1;
    t[last] ^= 0x01;
    assert_rejected(&meta.nonce_prefix, &t, "bit flip in the last chunk");
}

#[test]
fn truncation_is_detected() {
    let plaintext = patterned(PLAINTEXT_CHUNK * 3);
    let (meta, ct) = encrypt_asset_vec_full(&KEY, &plaintext);
    let prefix = &meta.nonce_prefix;

    assert_rejected(prefix, &[], "empty ciphertext");
    assert_rejected(prefix, &ct[..CIPHERTEXT_CHUNK], "cut at a chunk boundary");
    assert_rejected(prefix, &ct[..CIPHERTEXT_CHUNK * 2], "final chunk dropped");
    assert_rejected(prefix, &ct[..ct.len() - 1], "one byte short");
    assert_rejected(prefix, &ct[..CIPHERTEXT_CHUNK + 5], "cut mid-chunk");

    // Extension: an extra byte, or a whole chunk appended after the last.
    let mut longer = ct.clone();
    longer.push(0);
    assert_rejected(prefix, &longer, "trailing byte");
    let mut doubled = ct.clone();
    doubled.extend_from_slice(&ct[..CIPHERTEXT_CHUNK]);
    assert_rejected(prefix, &doubled, "chunk appended after the last");
}

#[test]
fn chunk_reorder_and_drop_are_detected() {
    let plaintext = patterned(PLAINTEXT_CHUNK * 3);
    let (meta, ct) = encrypt_asset_vec_full(&KEY, &plaintext);
    let chunk = |i: usize| &ct[i * CIPHERTEXT_CHUNK..(i + 1) * CIPHERTEXT_CHUNK];

    // Swap the first two chunks (reorder) — STREAM counter mismatch.
    let swapped = [chunk(1), chunk(0), chunk(2)].concat();
    assert_rejected(&meta.nonce_prefix, &swapped, "first two chunks swapped");

    // Move the last chunk forward — last-block flag out of place.
    let moved = [chunk(0), chunk(2), chunk(1)].concat();
    assert_rejected(&meta.nonce_prefix, &moved, "last chunk moved forward");

    // Drop a middle chunk.
    let dropped = [chunk(0), chunk(2)].concat();
    assert_rejected(&meta.nonce_prefix, &dropped, "middle chunk dropped");

    // Duplicate a chunk in place of another.
    let duplicated = [chunk(0), chunk(0), chunk(2)].concat();
    assert_rejected(&meta.nonce_prefix, &duplicated, "chunk duplicated");
}

/// A chunk lifted from another file — even one under the same key at the same index — does not
/// open, because the nonce prefix differs per encryption.
#[test]
fn segment_swap_between_files_is_detected() {
    let a = patterned(PLAINTEXT_CHUNK * 2);
    let b: Vec<u8> = a.iter().map(|x| x ^ 0xff).collect();
    let (ma, ca) = encrypt_asset_vec_with_prefix(&KEY, [1; NONCE_PREFIX_LEN], &a);
    let (_, cb) = encrypt_asset_vec_with_prefix(&KEY, [2; NONCE_PREFIX_LEN], &b);

    let spliced = [&cb[..CIPHERTEXT_CHUNK], &ca[CIPHERTEXT_CHUNK..]].concat();
    assert_rejected(&ma.nonce_prefix, &spliced, "first chunk from another file");
    let spliced = [&ca[..CIPHERTEXT_CHUNK], &cb[CIPHERTEXT_CHUNK..]].concat();
    assert_rejected(&ma.nonce_prefix, &spliced, "last chunk from another file");
}

/// The last-block flag is authenticated: a non-final chunk does not open as final, nor the final
/// one as non-final.
#[test]
fn last_flag_toggle_is_detected() {
    let plaintext = patterned(PLAINTEXT_CHUNK * 2);
    let (meta, ct) = encrypt_asset_vec_full(&KEY, &plaintext);
    let prefix = &meta.nonce_prefix;
    assert!(decrypt_chunk(&KEY, prefix, 0, true, &ct[..CIPHERTEXT_CHUNK]).is_err());
    assert!(decrypt_chunk(&KEY, prefix, 1, false, &ct[CIPHERTEXT_CHUNK..]).is_err());
    assert!(decrypt_chunk(&KEY, prefix, 0, false, &ct[..CIPHERTEXT_CHUNK]).is_ok());
    assert!(decrypt_chunk(&KEY, prefix, 1, true, &ct[CIPHERTEXT_CHUNK..]).is_ok());
}

#[test]
fn stream_decryptor_stays_failed() {
    let plaintext = patterned(PLAINTEXT_CHUNK * 3);
    let (meta, mut ct) = encrypt_asset_vec_full(&KEY, &plaintext);
    ct[0] ^= 1;
    let mut decryptor = StreamDecryptor::new(&KEY, &meta.nonce_prefix, Vec::new());
    let split = CIPHERTEXT_CHUNK + 1;
    assert!(decryptor.update(&ct[..split]).is_err());
    assert!(decryptor.update(&ct[split..]).is_err());
    assert!(decryptor.finish().is_err());
}

#[test]
fn wrong_key_or_prefix_is_rejected() {
    let (meta, ct) = encrypt_asset_vec_full(&KEY, b"secret bytes");
    assert!(decrypt_asset_vec(&[0x22; 32], &meta.nonce_prefix, &ct).is_err());
    let mut bad_prefix = meta.nonce_prefix;
    bad_prefix[0] ^= 0xff;
    assert!(decrypt_asset_vec(&KEY, &bad_prefix, &ct).is_err());
}

#[test]
fn distinct_files_get_distinct_prefixes() {
    let a = encrypt_asset_vec(&KEY, b"x");
    let b = encrypt_asset_vec(&KEY, b"x");
    assert_ne!(a.nonce_prefix, b.nonce_prefix);
}

#[test]
fn fixed_prefix_regenerates_identical_ciphertext() {
    // The client stores plaintext; re-encrypting with the manifest's recorded prefix
    // reproduces the exact ciphertext (and hash) for backup/upload.
    let prefix = [9u8; NONCE_PREFIX_LEN];
    let plaintext = b"regenerable bytes";
    let (m1, c1) = encrypt_asset_vec_with_prefix(&KEY, prefix, plaintext);
    let (m2, c2) = encrypt_asset_vec_with_prefix(&KEY, prefix, plaintext);
    assert_eq!(c1, c2);
    assert_eq!(m1.ciphertext_hash, m2.ciphertext_hash);
    assert_eq!(m1.nonce_prefix, prefix);
    assert_eq!(decrypt_asset_vec(&KEY, &prefix, &c1).unwrap(), plaintext);
}
