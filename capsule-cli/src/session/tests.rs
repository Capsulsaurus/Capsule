use super::*;

fn temp_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "capsule-cli-session-{tag}-{}.json",
        nanoid::nanoid!()
    ))
}

/// A saved session round-trips through the file store byte-for-byte.
#[test]
fn save_then_load_round_trips_the_token_pair() {
    let path = temp_path("roundtrip");
    let store = SessionStore::new(&path);
    assert!(store.load().unwrap().is_none(), "empty store loads None");

    let session = PersistedSession {
        access_token: "access-xyz".into(),
        refresh_token: "refresh-abc".into(),
        access_expires_at_unix: 1_900_000_000,
    };
    store.save(&session).unwrap();

    let loaded = store.load().unwrap().expect("session present after save");
    assert_eq!(loaded.access_token.expose_secret(), "access-xyz");
    assert_eq!(loaded.refresh_token.expose_secret(), "refresh-abc");
    assert_eq!(loaded.access_expires_at_unix, 1_900_000_000);

    store.clear().unwrap();
    assert!(store.load().unwrap().is_none(), "cleared store loads None");
    // Clearing again is idempotent.
    store.clear().unwrap();
}

/// The session file is written owner-only (`0600`) so the tokens are not
/// readable by other users on a shared host.
#[cfg(unix)]
#[test]
fn session_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let path = temp_path("perms");
    let store = SessionStore::new(&path);
    store
        .save(&PersistedSession {
            access_token: "a".into(),
            refresh_token: "r".into(),
            access_expires_at_unix: 1,
        })
        .unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "session file must be owner-only");
    store.clear().unwrap();
}

/// A corrupt session file surfaces a typed decode error, not a silent empty.
#[test]
fn malformed_file_is_a_decode_error() {
    let path = temp_path("malformed");
    std::fs::write(&path, b"not json").unwrap();
    let store = SessionStore::new(&path);
    assert!(matches!(store.load(), Err(SessionError::Decode { .. })));
    let _ = std::fs::remove_file(&path);
}
