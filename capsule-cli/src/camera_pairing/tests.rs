//! The pairing state: created once and stable after, renamed without losing the GUID, and never
//! silently replaced when the file cannot be read.

use super::*;

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("capsule-camera-pairing-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn store(&self) -> PairingStore {
        PairingStore::new(self.0.join("config").join(PAIRING_FILE))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_client_that_never_paired_has_no_identity() {
    let scratch = Scratch::new();
    assert_eq!(scratch.store().load().unwrap(), None);
}

#[test]
fn the_identity_is_created_once_and_reused_across_runs() {
    let scratch = Scratch::new();
    let (first, created) = scratch
        .store()
        .load_or_create(|| "Capsule (studio)".into())
        .unwrap();
    assert!(created);
    assert_eq!(first.name, "Capsule (studio)");
    assert_eq!(first.guid.get_version_num(), 4, "random, not time-ordered");

    // A later run, through a fresh store over the same file, presents the same identity.
    let (second, created) = scratch
        .store()
        .load_or_create(|| panic!("an existing identity is never re-minted"))
        .unwrap();
    assert!(!created);
    assert_eq!(second, first);
    assert_eq!(second.initiator().guid, *first.guid.as_bytes());
    assert_eq!(second.initiator().name, "Capsule (studio)");
    assert!(
        !scratch.store().path().with_extension("json.tmp").exists(),
        "the staging file was renamed into place"
    );
}

#[test]
fn renaming_keeps_the_guid_a_camera_paired_with() {
    let scratch = Scratch::new();
    let (paired, _) = scratch.store().load_or_create(|| "Old".into()).unwrap();
    let (renamed, created) = scratch.store().rename("  Darkroom  ").unwrap();
    assert!(!created, "the identity already existed");
    assert_eq!(renamed.guid, paired.guid);
    assert_eq!(renamed.name, "Darkroom", "trimmed");
    assert_eq!(scratch.store().load().unwrap(), Some(renamed));
}

#[test]
fn renaming_before_any_pairing_creates_the_identity() {
    let scratch = Scratch::new();
    let (named, created) = scratch.store().rename("Laptop").unwrap();
    assert!(created, "this run created the identity");
    assert!(!named.guid.is_nil());
    assert_eq!(scratch.store().load().unwrap(), Some(named));
}

#[test]
fn a_corrupt_file_is_an_error_never_a_fresh_identity() {
    let scratch = Scratch::new();
    let store = scratch.store();
    std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    std::fs::write(store.path(), b"{ not json").unwrap();
    assert!(matches!(store.load(), Err(PairingError::Corrupt { .. })));
    assert!(matches!(
        store.load_or_create(|| "Capsule".into()),
        Err(PairingError::Corrupt { .. })
    ));
    assert_eq!(
        std::fs::read(store.path()).unwrap(),
        b"{ not json",
        "the unreadable file is left for the person to inspect"
    );

    let nil = serde_json::json!({ "guid": Uuid::nil(), "name": "Capsule" });
    std::fs::write(store.path(), nil.to_string()).unwrap();
    assert!(matches!(store.load(), Err(PairingError::Corrupt { .. })));

    let blank = serde_json::json!({ "guid": Uuid::new_v4(), "name": " " });
    std::fs::write(store.path(), blank.to_string()).unwrap();
    assert!(matches!(store.load(), Err(PairingError::Corrupt { .. })));
}

#[test]
fn a_name_must_be_one_a_camera_can_show() {
    assert!(matches!(
        validate_name("   "),
        Err(PairingError::InvalidName(_))
    ));
    assert!(matches!(
        validate_name("bad\u{7}name"),
        Err(PairingError::InvalidName(_))
    ));
    assert!(validate_name(&"x".repeat(MAX_NAME_CHARS)).is_ok());
    assert!(matches!(
        validate_name(&"x".repeat(MAX_NAME_CHARS + 1)),
        Err(PairingError::InvalidName(_))
    ));
    let scratch = Scratch::new();
    assert!(scratch.store().rename("").is_err());
    assert_eq!(scratch.store().load().unwrap(), None, "nothing was written");
}

#[test]
fn the_default_name_carries_the_host_within_the_length_bound() {
    assert_eq!(default_name(Some("studio")), "Capsule (studio)");
    assert_eq!(default_name(None), "Capsule");
    assert_eq!(default_name(Some(" \n ")), "Capsule");
    let long = default_name(Some(&"h".repeat(200)));
    assert_eq!(long.chars().count(), MAX_NAME_CHARS);
    assert!(validate_name(&long).is_ok());
}
