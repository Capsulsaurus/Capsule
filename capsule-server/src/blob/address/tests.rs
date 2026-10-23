use super::*;

fn address(seed: &str) -> ContentAddress {
    let mut hex = seed.to_owned();
    hex.push_str(&"0".repeat(CONTENT_ADDRESS_LEN - seed.len()));
    match ContentAddress::parse(&hex) {
        Ok(address) => address,
        Err(error) => panic!("the fixture must be an address: {error}"),
    }
}

#[test]
fn a_blob_is_addressed_two_levels_deep_and_never_flat() {
    let root = Path::new("/srv/capsule/blobs-root");
    let address = address("abcdef12");

    assert_eq!(address.shard(), ["ab", "cd"]);
    assert_eq!(
        blob_path(root, &address),
        root.join("blobs")
            .join("ab")
            .join("cd")
            .join(format!("{address}.bin")),
    );
    assert_ne!(
        blob_path(root, &address),
        root.join("blobs").join(format!("{address}.bin")),
        "the flat layout is what the shard overturned"
    );
}

#[test]
fn only_lowercase_hex_of_the_right_length_is_an_address() {
    assert!(ContentAddress::parse(&"a".repeat(64)).is_ok());
    assert_eq!(
        ContentAddress::parse(&"a".repeat(63)),
        Err(MalformedAddress::Length { actual: 63 })
    );
    assert_eq!(
        ContentAddress::parse(&"a".repeat(65)),
        Err(MalformedAddress::Length { actual: 65 })
    );
    assert_eq!(
        ContentAddress::parse(&format!("A{}", "a".repeat(63))),
        Err(MalformedAddress::NotLowercaseHex { position: 0 }),
        "case is the invariant, not a normalization"
    );
    assert_eq!(
        ContentAddress::parse(&format!("{}g", "a".repeat(63))),
        Err(MalformedAddress::NotLowercaseHex { position: 63 })
    );
    assert!(
        ContentAddress::parse(&format!("../{}", "a".repeat(61))).is_err(),
        "a traversal sequence is not hex, so it can never reach a path"
    );
}

#[test]
fn a_file_name_round_trips_through_its_address() {
    let address = address("0f1e2d3c");
    assert_eq!(
        ContentAddress::from_file_name(&address.file_name()),
        Some(address)
    );
    assert_eq!(ContentAddress::from_file_name("readme.txt"), None);
    assert_eq!(ContentAddress::from_file_name(&"a".repeat(64)), None);
    assert_eq!(
        ContentAddress::from_file_name(&format!(".{}.tmp", "a".repeat(64))),
        None,
        "a crashed temp file is debris, not a blob"
    );
}

#[test]
fn an_address_knows_the_shard_it_belongs_to() {
    let address = address("abcdef12");
    assert!(address.is_filed_under(["ab", "cd"]));
    assert!(!address.is_filed_under(["ab", "ce"]));
}

#[test]
fn a_shard_segment_is_two_lowercase_hex_characters() {
    assert!(is_shard_segment("ab"));
    assert!(is_shard_segment("09"));
    assert!(!is_shard_segment("AB"));
    assert!(!is_shard_segment("a"));
    assert!(!is_shard_segment("abc"));
    assert!(!is_shard_segment(".."));
}

/// The layout is valid for a 64-character lowercase-hex address and for nothing else.
///
/// If a future change re-pins the ciphertext digest, this fails and says what the change
/// actually costs — a `.server/version` bump and a data move — rather than letting every
/// stored path silently mean something different.
#[test]
fn digest_change_is_a_layout_change() {
    assert_eq!(
        CONTENT_ADDRESS_LEN, 64,
        "the shard is SHA-256 hex; changing the digest is a `.server/version` data move"
    );
    assert_eq!(SHARD_DEPTH, 2);
    assert_eq!(SHARD_SEGMENT_LEN, 2);
}
