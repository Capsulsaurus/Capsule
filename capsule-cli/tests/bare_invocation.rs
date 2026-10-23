//! Bare `capsule` off a terminal (issue #681): the full-screen TUI opens only when stdin and
//! stdout are both terminals. Spawned with piped stdio, as a script or CI runner would, the
//! binary prints its help on stderr and exits 2, writes nothing on stdout, and returns at once
//! rather than waiting for input.
//!
//! ## Test list
//!
//! - `bare_capsule_in_a_pipe_prints_help_and_exits_2` — status 2, empty stdout, the usage and
//!   the command list on stderr.

use std::process::{Command, Stdio};

#[test]
fn bare_capsule_in_a_pipe_prints_help_and_exits_2() {
    let home = std::env::temp_dir().join(format!("capsule-cli-bare-{}", nanoid::nanoid!()));
    std::fs::create_dir_all(&home).expect("create scratch home");
    let out = Command::new(env!("CARGO_BIN_EXE_capsule"))
        .env("NO_COLOR", "1")
        .env("RUST_LOG", "off")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run capsule");
    // No TUI log was opened: the TUI never started.
    let tui_started = home.join("cache").exists();
    let _ = std::fs::remove_dir_all(&home);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(
        out.stdout.is_empty(),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(stderr.contains("Usage: capsule"), "{stderr}");
    assert!(stderr.contains("Commands:"), "{stderr}");
    assert!(
        !tui_started,
        "the TUI opened its log under the cache directory"
    );
}
