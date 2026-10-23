//! Client build identification — the `client_version` / `generated_by_client` grammar.
//!
//! Every *real* manifest producer pins a write to the exact client build that authored it, so a
//! defect in one shipped build — of any client, in-repo or not — is traceable across every asset
//! it touched. The normative grammar (SSoT: [Provenance — Client Build Identification]) is:
//!
//! ```text
//! client_version = client_id "/" semver "+" commit [".dirty"]
//! ```
//!
//! - `client_id` — the client product, which also names the platform (`capsule-ios`,
//!   `capsule-android`, `capsule-desktop`, `capsule-cli`, `capsule-web`, or an out-of-repo
//!   client's own stable id).
//! - `commit` — the git commit of the client's own source tree (a `≥ 12`-hex prefix), embedded
//!   at build time by [`build.rs`](../build.rs) as
//!   [`BUILD_COMMIT`](crate::client_build::BUILD_COMMIT).
//! - `.dirty` — appended ([`BUILD_DIRTY_SUFFIX`](crate::client_build::BUILD_DIRTY_SUFFIX)) when
//!   built from a modified tree.
//!
//! The value is **audit-only** and never load-bearing for authorization: `verify_asset` does not
//! gate on the grammar (a nonconforming string still verifies), so this module is producer
//! discipline plus a parser for audit tooling and the grammar round-trip test.
//!
//! In-repo clients inject only their `client_id` + own `semver` (via
//! `Workspace::with_client_id` or the FFI
//! constructor); the build-embedded commit and dirty flag are appended here so every client on
//! this tree reports the same commit.
//!
//! [Provenance — Client Build Identification]: https://docs/design/cryptography/provenance/#client-build-identification

/// Git commit (a `≥ 12`-hex prefix) of the capsule-core source tree, embedded at build time.
///
/// Falls back to [`UNKNOWN_COMMIT`] when the build ran without a usable git probe (missing
/// binary, non-repository, or a shallow clone with no reachable commit) — a documented sentinel,
/// never a build failure. See [`build.rs`](../build.rs).
pub const BUILD_COMMIT: &str = env!("CAPSULE_BUILD_COMMIT");

/// `".dirty"` when capsule-core was built from a modified (tracked) tree, else `""`.
pub const BUILD_DIRTY_SUFFIX: &str = env!("CAPSULE_BUILD_DIRTY");

/// The all-zero sentinel [`BUILD_COMMIT`] carries when no commit could be probed at build time.
/// Kept at 16 hex digits so a fallback build still emits a grammar-conformant `client_version`.
pub const UNKNOWN_COMMIT: &str = "0000000000000000";

/// The default in-repo product id a bare capsule-core producer reports until an app injects its
/// own identity through the SDK/FFI surface.
pub const CORE_CLIENT_ID: &str = "capsule-core";

/// Compose a grammar-conformant `client_version` for an in-repo client. `client_id` names the
/// product (`capsule-cli`, `capsule-ios`, …); `semver` is that client's own crate version. The
/// build-embedded [`BUILD_COMMIT`] and [`BUILD_DIRTY_SUFFIX`] are appended.
#[must_use]
pub fn client_version(client_id: &str, semver: &str) -> String {
    format!("{client_id}/{semver}+{BUILD_COMMIT}{BUILD_DIRTY_SUFFIX}")
}

/// The default `capsule-core/{semver}+{commit}[.dirty]` identity, used until an app injects its
/// own `client_id`. `semver` is capsule-core's own crate version.
#[must_use]
pub fn core_client_version() -> String {
    client_version(CORE_CLIENT_ID, env!("CARGO_PKG_VERSION"))
}

/// A parsed `client_version`, for the grammar round-trip in tests and for audit tooling. The
/// grammar is producer discipline only — a manifest carrying a nonconforming string still
/// verifies, so parsing is never a `verify_asset` gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientVersion {
    /// The client product / platform id (e.g. `capsule-cli`).
    pub client_id: String,
    /// The client's own semantic version (e.g. `0.1.0`).
    pub semver: String,
    /// The git commit prefix (`≥ 12` hex digits).
    pub commit: String,
    /// Whether the producing tree was modified (`.dirty`).
    pub dirty: bool,
}

impl ClientVersion {
    /// Parse a `client_id "/" semver "+" commit [".dirty"]` string, enforcing the normative
    /// grammar: a non-empty `client_id` and `semver`, and a `commit` of `≥ 12` hex digits.
    /// Returns `None` for a nonconforming value (audit-only; never a verify gate).
    ///
    /// `commit` is split at the **last** `+` so a semver build-metadata `+` (e.g.
    /// `1.2.3+meta+abc123def456`) never steals the commit boundary.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let (client_id, rest) = s.split_once('/')?;
        let (semver, commit_full) = rest.rsplit_once('+')?;
        let (commit, dirty) = match commit_full.strip_suffix(".dirty") {
            Some(commit) => (commit, true),
            None => (commit_full, false),
        };
        if client_id.is_empty() || semver.is_empty() {
            return None;
        }
        if commit.len() < 12 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(Self {
            client_id: client_id.to_owned(),
            semver: semver.to_owned(),
            commit: commit.to_owned(),
            dirty,
        })
    }
}

impl std::fmt::Display for ClientVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}+{}", self.client_id, self.semver, self.commit)?;
        if self.dirty {
            f.write_str(".dirty")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
