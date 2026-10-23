use super::*;

#[test]
fn test_serde_round_trip() {
    let variants = [
        MemberRole::Primary,
        MemberRole::Raw,
        MemberRole::Video,
        MemberRole::Audio,
        MemberRole::DepthMap,
        MemberRole::Processed,
        MemberRole::Source,
        MemberRole::Alternate,
        MemberRole::Sidecar,
        MemberRole::Proxy,
        MemberRole::Master,
    ];
    for variant in variants {
        let json = serde_json::to_string(&variant).unwrap();
        let roundtrip: MemberRole = serde_json::from_str(&json).unwrap();
        assert_eq!(variant, roundtrip);
    }
}

#[test]
fn test_serde_values() {
    assert_eq!(
        serde_json::to_string(&MemberRole::Primary).unwrap(),
        "\"primary\""
    );
    assert_eq!(
        serde_json::to_string(&MemberRole::DepthMap).unwrap(),
        "\"depth_map\""
    );
}
