//! Build-time client build identification for `client_version` / `generated_by_client` (S-D15).
//!
//! Embeds the capsule-core source tree's git commit (a ≥12-hex prefix) and a dirty flag as the
//! `CAPSULE_BUILD_COMMIT` / `CAPSULE_BUILD_DIRTY` compile-time env vars, consumed by
//! [`crate::client_build`](../src/client_build.rs). No `vergen`-class dependency: the probe is a
//! handful of plain `git` invocations.
//!
//! ## Robust, never-fail probe
//! The probe is best-effort. A missing `git` binary, a non-repository checkout, or a shallow
//! clone with no reachable commit all fall back to the all-zero [`UNKNOWN_COMMIT`] sentinel with
//! the dirty flag unset — the build never fails on a degraded environment. The sentinel is still
//! 16 hex digits, so the grammar round-trip in `client_build` holds even for a fallback build.
//!
//! ## `cargo:rerun-if` strategy (keep incremental builds cache-stable)
//! We embed **no** timestamp and register **no** unconditional rerun, so an ordinary incremental
//! build reuses the cached artifact instead of recompiling capsule-core every time. We re-run the
//! probe only when the commit or staged state can actually change, by watching git's own `HEAD`,
//! the branch ref `HEAD` points at, and the `index` (each resolved via `git rev-parse --git-path`
//! so linked worktrees and submodules resolve correctly). A new commit or a `git add` re-runs the
//! probe; nothing else does. Purely *unstaged* working-tree edits touch none of those files, so
//! the `.dirty` flag reflects the tree as of the last commit/stage event — the documented,
//! accepted limitation of a dependency-free build-time git probe.

use std::process::Command;

/// All-zero sentinel commit emitted when no git commit can be probed. Still 16 hex digits, so it
/// satisfies the `commit` grammar (`≥ 12` hex) and parses like any real value in `client_build`.
const UNKNOWN_COMMIT: &str = "0000000000000000";

/// Run `git <args>` and return its trimmed stdout, or `None` on any failure (missing binary,
/// non-zero exit, non-UTF-8, or empty output).
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_owned())
    }
}

fn main() {
    // Commit: a 12-hex prefix of HEAD, or the sentinel on any probe failure.
    let commit =
        git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| UNKNOWN_COMMIT.to_owned());

    // Dirty: tracked modifications relative to HEAD. Untracked files are ignored (`-uno`) so
    // stray scratch/editor files never spuriously flag a clean build. Only meaningful once we
    // actually resolved a commit — a degraded probe reports clean.
    let dirty = commit != UNKNOWN_COMMIT
        && git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());

    println!("cargo:rustc-env=CAPSULE_BUILD_COMMIT={commit}");
    println!(
        "cargo:rustc-env=CAPSULE_BUILD_DIRTY={}",
        if dirty { ".dirty" } else { "" }
    );

    // Re-run only when the commit / staged state can change — see the module docs.
    for git_path in ["HEAD", "index"] {
        if let Some(resolved) = git(&["rev-parse", "--git-path", git_path]) {
            println!("cargo:rerun-if-changed={resolved}");
        }
    }
    if let Some(head_ref) = git(&["symbolic-ref", "--quiet", "HEAD"])
        && let Some(resolved) = git(&["rev-parse", "--git-path", &head_ref])
    {
        println!("cargo:rerun-if-changed={resolved}");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
