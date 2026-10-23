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
