use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::library::error::LibraryError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockRecord {
    pub pid: u32,
    pub hostname: String,
    pub locked_at: i64,
}

/// Try to acquire the library lock. Creates `.library/lock` atomically.
/// On AlreadyExists: reads the existing lock; if the holding process is no
/// longer alive (same host, dead PID), the stale lock is removed and
/// acquisition retried. Otherwise returns `LibraryError::Locked`.
pub fn try_acquire(root: &Path) -> Result<(), LibraryError> {
    let lock_path = root.join(".library/lock");

    let record = LockRecord {
        pid: std::process::id(),
        hostname: current_hostname(),
        locked_at: now_secs(),
    };
    let json = serde_json::to_string(&record)
        .map_err(|e| LibraryError::Io(std::io::Error::other(e.to_string())))?;

    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
    {
        Ok(mut file) => {
            file.write_all(json.as_bytes())?;
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Read the existing lock record.
            match fs::read_to_string(&lock_path) {
                Ok(contents) => match serde_json::from_str::<LockRecord>(&contents) {
                    Ok(existing) => {
                        let same_host = existing.hostname == current_hostname();
                        if same_host && !is_pid_alive(existing.pid) {
                            // Stale lock from a dead process — remove and retry once.
                            fs::remove_file(&lock_path)?;
                            return try_acquire(root);
                        }
                        Err(LibraryError::Locked {
                            pid: existing.pid,
                            hostname: existing.hostname,
                            locked_at: existing.locked_at,
                        })
                    }
                    Err(_) => {
                        // Corrupt lock — remove and retry.
                        let _ = fs::remove_file(&lock_path);
                        try_acquire(root)
                    }
                },
                Err(_) => Err(LibraryError::Io(e)),
            }
        }
        Err(e) => Err(LibraryError::Io(e)),
    }
}

/// Release the lock by deleting `.library/lock`.
pub fn release(root: &Path) -> Result<(), LibraryError> {
    let lock_path = root.join(".library/lock");
    if lock_path.exists() {
        fs::remove_file(&lock_path)?;
    }
    Ok(())
}

fn current_hostname() -> String {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/etc/hostname")
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "unknown".to_string())
    }
}

fn is_pid_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        // Conservative: assume alive on non-Linux (requires manual lock deletion)
        true
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
}
