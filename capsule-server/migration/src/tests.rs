use super::{checksum, checksums, sources};

#[test]
fn a_checksum_is_the_hex_sha256_of_the_source() {
    assert_eq!(
        checksum("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn a_checksum_does_not_depend_on_line_endings() {
    assert_eq!(checksum("a\r\nb\r\n"), checksum("a\nb\n"));
}

#[test]
fn an_edit_changes_the_checksum() {
    assert_ne!(checksum("CREATE TABLE a"), checksum("CREATE TABLE a "));
}

/// Each migration hashes its own file, and no two files are the same text.
#[test]
fn every_migration_has_a_distinct_checksum() {
    let all = checksums();
    assert_eq!(all.len(), sources().len());
    for (index, (name, sum)) in all.iter().enumerate() {
        assert!(
            all.iter().skip(index + 1).all(|(_, other)| other != sum),
            "{name} shares its checksum with a later migration"
        );
    }
}
