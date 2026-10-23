use tempfile::TempDir;

use super::*;

fn make_library_dir(tmp: &TempDir) {
    fs::create_dir_all(tmp.path().join(".library")).unwrap();
}

#[test]
fn test_acquire_and_release() {
    let tmp = TempDir::new().unwrap();
    make_library_dir(&tmp);
    let root = tmp.path();

    try_acquire(root).expect("first acquire should succeed");
    assert!(root.join(".library/lock").exists());

    release(root).expect("release should succeed");
    assert!(!root.join(".library/lock").exists());
}

#[test]
fn test_double_acquire_fails() {
    let tmp = TempDir::new().unwrap();
    make_library_dir(&tmp);
    let root = tmp.path();

    try_acquire(root).unwrap();
    let result = try_acquire(root);
    assert!(
        matches!(result, Err(LibraryError::Locked { .. })),
        "second acquire should fail with Locked, got: {result:?}"
    );

    release(root).unwrap();
}

#[test]
fn test_stale_lock_recovery() {
    let tmp = TempDir::new().unwrap();
    make_library_dir(&tmp);
    let root = tmp.path();

    // Write a fake lock with PID 0 (never a real process on Linux)
    let stale = LockRecord {
        pid: 0,
        hostname: current_hostname(),
        locked_at: 0,
    };
    let lock_path = root.join(".library/lock");
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&lock_path)
        .unwrap();
    f.write_all(serde_json::to_string(&stale).unwrap().as_bytes())
        .unwrap();

    // On Linux, PID 0 is not alive, so acquisition should succeed after cleanup.
    // On non-Linux the conservative assumption keeps the lock — skip that case.
    #[cfg(target_os = "linux")]
    {
        try_acquire(root).expect("should recover from stale lock");
        release(root).unwrap();
    }
}

#[test]
fn test_release_nonexistent_is_ok() {
    let tmp = TempDir::new().unwrap();
    make_library_dir(&tmp);
    release(tmp.path()).expect("releasing a non-existent lock should be a no-op");
}
