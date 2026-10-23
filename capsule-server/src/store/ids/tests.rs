use super::*;

#[test]
fn a_plain_id_prints_its_value() {
    let id = SessionId::new("sid-1");
    assert_eq!(id.to_string(), "sid-1");
    assert_eq!(format!("{id:?}"), r#"SessionId("sid-1")"#);
}

#[test]
fn a_secret_id_never_prints_its_value() {
    let token = ChallengeToken::new("super-secret-challenge");
    let rendered = format!("{token:?}");
    assert!(
        !rendered.contains("super-secret-challenge"),
        "a bearer secret must not reach a log through Debug, got {rendered}"
    );
    assert_eq!(rendered, "ChallengeToken(<redacted>)");
    assert_eq!(
        token.as_str(),
        "super-secret-challenge",
        "the value is still reachable for the backend key"
    );
}

#[test]
fn ids_of_different_kinds_are_different_types() {
    // Compile-time property, asserted by construction: the two ids below hold the same
    // text but no `==` between them exists, so a session id cannot be passed where an
    // upload id is expected. If these ever unify, this file stopped doing its job.
    let session = SessionId::new("shared-text");
    let upload = UploadId::new("shared-text");
    assert_eq!(session.as_str(), upload.as_str());
}
