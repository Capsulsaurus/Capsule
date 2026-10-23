use super::*;

#[test]
fn direction_tokens_round_trip() {
    for direction in [Direction::ToInitiator, Direction::ToEnrollee] {
        assert_eq!(Direction::parse(direction.as_str()), Some(direction));
    }
    assert_eq!(Direction::parse("c"), None);
}

#[test]
fn a_relay_payload_is_carried_verbatim() {
    let payload = RelayPayload::new("opaque\u{0}bytes");
    assert_eq!(payload.as_str(), "opaque\u{0}bytes");
    assert_eq!(payload.len(), 12);
    assert!(!payload.is_empty());
    assert!(RelayPayload::new("").is_empty());
}
