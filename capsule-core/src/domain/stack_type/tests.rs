use super::*;

#[test]
fn test_serde_round_trip() {
    let variants = [
        StackType::RawJpeg,
        StackType::Burst,
        StackType::LivePhoto,
        StackType::Portrait,
        StackType::SmartSelection,
        StackType::HdrBracket,
        StackType::FocusStack,
        StackType::PixelShift,
        StackType::Panorama,
        StackType::Proxy,
        StackType::Chaptered,
        StackType::DualAudio,
        StackType::Custom,
    ];
    for variant in variants {
        let json = serde_json::to_string(&variant).unwrap();
        let roundtrip: StackType = serde_json::from_str(&json).unwrap();
        assert_eq!(variant, roundtrip);
    }
}

#[test]
fn test_serde_values() {
    assert_eq!(
        serde_json::to_string(&StackType::RawJpeg).unwrap(),
        "\"raw_jpeg\""
    );
    assert_eq!(
        serde_json::to_string(&StackType::LivePhoto).unwrap(),
        "\"live_photo\""
    );
    assert_eq!(
        serde_json::to_string(&StackType::HdrBracket).unwrap(),
        "\"hdr_bracket\""
    );
}
