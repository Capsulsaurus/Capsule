use super::util::get_file_stem;
use super::{checksum, checksums, sources};

/// A migration's name is its file's stem, which is the version `seaql_migrations` records.
#[test]
fn a_migration_is_named_by_its_file_stem() {
    assert_eq!(
        get_file_stem("capsule-server/migration/src/m20260902_000001_asset_index.rs"),
        "m20260902_000001_asset_index"
    );
    let (first, _) = &sources()[0];
    assert_eq!(first.name(), "m20260902_000001_asset_index");
}

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
