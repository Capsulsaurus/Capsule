use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureTzSource {
    OffsetExif,
    GpsLookup,
    Floating,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serde_round_trip() {
        let variants = [
            CaptureTzSource::OffsetExif,
            CaptureTzSource::GpsLookup,
            CaptureTzSource::Floating,
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let roundtrip: CaptureTzSource = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, roundtrip);
        }
    }

    #[test]
    fn test_serde_values() {
        assert_eq!(
            serde_json::to_string(&CaptureTzSource::OffsetExif).unwrap(),
            "\"offset_exif\""
        );
        assert_eq!(
            serde_json::to_string(&CaptureTzSource::GpsLookup).unwrap(),
            "\"gps_lookup\""
        );
        assert_eq!(
            serde_json::to_string(&CaptureTzSource::Floating).unwrap(),
            "\"floating\""
        );
    }
}
