use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::library::error::LibraryError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LockRecord {
    pub pid: u32,
    pub hostname: String,
    pub locked_at: i64,
}

/// Try to acquire the library lock. Creates `.library/lock` atomically.
/// On AlreadyExists: reads the existing lock; if the holding process is no
/// longer alive (same host, dead PID), the stale lock is removed and
/// acquisition retried. Otherwise returns `LibraryError::Locked`.
pub(crate) fn try_acquire(root: &Path) -> Result<(), LibraryError> {
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
                Ok(contents) => {
                    if let Ok(existing) = serde_json::from_str::<LockRecord>(&contents) {
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
                    } else {
                        // Corrupt lock — remove and retry.
                        let _ = fs::remove_file(&lock_path);
                        try_acquire(root)
                    }
                }
                Err(_) => Err(LibraryError::Io(e)),
            }
        }
        Err(e) => Err(LibraryError::Io(e)),
    }
}

/// Release the lock by deleting `.library/lock`.
pub(crate) fn release(root: &Path) -> Result<(), LibraryError> {
    let lock_path = root.join(".library/lock");
    if lock_path.exists() {
        fs::remove_file(&lock_path)?;
    }
    Ok(())
}

fn current_hostname() -> String {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/etc/hostname").map_or_else(
            |_| std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string()),
            |s| s.trim().to_string(),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "unknown".to_string())
    }
}

/// Whether `pid` names a live process. Android has Linux's `/proc` and needs it most: the
/// system kills an app's process without notice, so every cold start after the first finds
/// a lock its dead predecessor left behind.
fn is_pid_alive(pid: u32) -> bool {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = pid;
        // Conservative: assume alive elsewhere (requires manual lock deletion)
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
mod tests;
