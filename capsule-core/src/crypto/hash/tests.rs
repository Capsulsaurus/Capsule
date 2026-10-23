use super::*;

// NIST FIPS 180-2 / RFC 6234 known-answer vectors.
const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
// SHA-256 of one million 'a' characters.
const MILLION_A: &str = "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0";

#[test]
fn kat_abc() {
    assert_eq!(hash_bytes(b"abc").to_hex(), ABC);
}

#[test]
fn kat_empty() {
    assert_eq!(hash_bytes(b"").to_hex(), EMPTY);
}

#[test]
fn kat_million_a() {
    let data = vec![b'a'; 1_000_000];
    assert_eq!(hash_bytes(&data).to_hex(), MILLION_A);
}

#[test]
fn chunked_equals_one_shot() {
    let data = vec![0xABu8; 65_520 * 3 + 17];
    let one_shot = hash_bytes(&data);

    // Feed in irregular pieces to prove order/chunking does not affect the digest.
    let mut h = Sha256Hasher::new();
    for chunk in data.chunks(65_520) {
        h.update(chunk);
    }
    assert_eq!(h.finalize(), one_shot);

    // And via the reader path.
    assert_eq!(hash_reader(&data[..]).unwrap(), one_shot);
}

#[test]
fn hex_round_trip() {
    let h = hash_bytes(b"capsule");
    let parsed = Hash32::from_hex(&h.to_hex()).unwrap();
    assert_eq!(h, parsed);
    assert_eq!(parsed.as_bytes(), h.as_bytes());
}

#[test]
fn from_hex_rejects_bad_length() {
    assert_eq!(Hash32::from_hex("dead"), Err(FromHexError));
    assert_eq!(
        Hash32::from_hex("zz".repeat(32).as_str()),
        Err(FromHexError)
    );
}

#[test]
fn serde_emits_cbor_byte_string() {
    // Major type 2 (byte string) of length 32 has initial byte 0x58 0x20.
    let h = hash_bytes(b"abc");
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&h, &mut buf).unwrap();
    assert_eq!(buf[0], 0x58, "expected CBOR byte-string major type");
    assert_eq!(buf[1], 0x20, "expected length 32");
    assert_eq!(&buf[2..], h.as_bytes());

    let decoded: Hash32 = ciborium::de::from_reader(buf.as_slice()).unwrap();
    assert_eq!(decoded, h);
}
