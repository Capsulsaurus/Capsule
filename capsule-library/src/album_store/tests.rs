use tempfile::TempDir;
use uuid::Uuid;

use super::*;

fn one_album() -> AlbumStore {
    let album = Uuid::from_u128(0x11);
    let mut store = AlbumStore::new();
    store.upsert(capsule_core::crypto::keys::PersistedAlbum::recovered(
        album,
        "Restored",
        vec![(album, 1, [8; 32])],
        None,
    ));
    store
}

#[test]
fn album_store_load_returns_none_for_a_library_that_never_had_one() {
    let dir = TempDir::new().unwrap();
    assert!(
        AlbumStore::load(dir.path(), &MasterKey::generate())
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_saved_store_loads_back_and_the_file_is_sealed() {
    let dir = TempDir::new().unwrap();
    let master = MasterKey::generate();
    one_album().save(dir.path(), &master).unwrap();

    let on_disk = fs::read(<AlbumStore as AlbumStoreFile>::path(dir.path())).unwrap();
    assert!(
        !on_disk.windows(32).any(|w| w == [8u8; 32]),
        "a raw AMK must never appear in the sealed file"
    );
    assert!(
        !dir.path().join(".library/albums.cbor.tmp").exists(),
        "the temp file is renamed over the store, never left behind"
    );

    let back = AlbumStore::load(dir.path(), &master).unwrap().unwrap();
    assert_eq!(back.albums().len(), 1);
    assert_eq!(back.albums()[0].amks, one_album().albums()[0].amks);
}

#[test]
fn a_store_sealed_under_another_master_key_is_refused_on_load() {
    let dir = TempDir::new().unwrap();
    one_album()
        .save(dir.path(), &MasterKey::generate())
        .unwrap();
    assert!(AlbumStore::load(dir.path(), &MasterKey::generate()).is_err());
}
