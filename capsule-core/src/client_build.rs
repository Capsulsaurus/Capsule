//! Client build identification — the `client_version` / `generated_by_client` grammar (S-D15).
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
//!   at build time by [`build.rs`](../build.rs) as [`BUILD_COMMIT`].
//! - `.dirty` — appended ([`BUILD_DIRTY_SUFFIX`]) when built from a modified tree.
//!
//! The value is **audit-only** and never load-bearing for authorization: `verify_asset` does not
//! gate on the grammar (a nonconforming string still verifies), so this module is producer
//! discipline plus a parser for audit tooling and the grammar round-trip test.
//!
//! In-repo clients inject only their `client_id` + own `semver` (via
//! [`Workspace::with_client_id`](crate::lifecycle::Workspace::with_client_id) or the FFI
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
mod tests {
    use super::*;

    /// The build-embedded commit is always grammar-legal: a `≥ 12`-hex prefix, or the all-zero
    /// fallback sentinel — never something that would break a producer's `client_version`.
    #[test]
    fn build_commit_is_grammar_legal() {
        assert!(
            BUILD_COMMIT.len() >= 12 && BUILD_COMMIT.bytes().all(|b| b.is_ascii_hexdigit()),
            "BUILD_COMMIT {BUILD_COMMIT:?} must be >=12 hex digits (real commit or sentinel)"
        );
        assert!(BUILD_DIRTY_SUFFIX.is_empty() || BUILD_DIRTY_SUFFIX == ".dirty");
    }

    /// A produced `client_version` has the normative grammar shape and round-trips through the
    /// parser — the Validation bullet's "grammar round-trip parses the emitted value".
    #[test]
    fn produced_client_version_has_grammar_shape() {
        let produced = client_version("capsule-cli", "1.4.2");
        let parsed = ClientVersion::parse(&produced)
            .unwrap_or_else(|| panic!("produced {produced:?} must parse"));
        assert_eq!(parsed.client_id, "capsule-cli");
        assert_eq!(parsed.semver, "1.4.2");
        assert!(parsed.commit.len() >= 12);
        // Round-trip: re-rendering the parse reproduces the produced string exactly.
        assert_eq!(parsed.to_string(), produced);
    }

    /// The bare-core default names `capsule-core` and carries a real embedded commit.
    #[test]
    fn core_client_version_names_core() {
        let produced = core_client_version();
        let parsed = ClientVersion::parse(&produced).expect("core client_version must parse");
        assert_eq!(parsed.client_id, CORE_CLIENT_ID);
        assert_eq!(parsed.semver, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn parses_dirty_suffix() {
        let parsed = ClientVersion::parse("capsule-ios/2.0.0+9f3a1c7d2b4e.dirty").unwrap();
        assert!(parsed.dirty);
        assert_eq!(parsed.commit, "9f3a1c7d2b4e");
        assert_eq!(parsed.to_string(), "capsule-ios/2.0.0+9f3a1c7d2b4e.dirty");
    }

    #[test]
    fn splits_commit_at_last_plus() {
        // A semver with build metadata keeps its `+meta`; only the final `+commit` is the commit.
        let parsed = ClientVersion::parse("capsule-web/1.0.0+build.7+abc123def4567").unwrap();
        assert_eq!(parsed.semver, "1.0.0+build.7");
        assert_eq!(parsed.commit, "abc123def4567");
    }

    #[test]
    fn rejects_nonconforming_values() {
        // The example fixtures scattered through the codebase are deliberately nonconforming.
        assert!(ClientVersion::parse("capsule-cli/0.1.0").is_none()); // no commit
        assert!(ClientVersion::parse("capsule-core/0.1.0+short").is_none()); // <12 hex
        assert!(ClientVersion::parse("capsule-core/0.1.0+zzzzzzzzzzzz").is_none()); // non-hex
        assert!(ClientVersion::parse("/0.1.0+abc123def456").is_none()); // empty client_id
        assert!(ClientVersion::parse("capsule-cli/+abc123def456").is_none()); // empty semver
        assert!(ClientVersion::parse("t").is_none()); // arbitrary test string
    }

    /// The sentinel fallback still parses, so a degraded (no-git) build emits a grammar-legal
    /// value rather than a broken one.
    #[test]
    fn sentinel_fallback_is_grammar_legal() {
        let produced = format!("capsule-cli/0.1.0+{UNKNOWN_COMMIT}");
        assert!(ClientVersion::parse(&produced).is_some());
    }
}
