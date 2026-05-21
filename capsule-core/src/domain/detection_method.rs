use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionMethod {
    FilenameStem,
    ContentIdentifier,
    Timecode,
    Manual,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serde_round_trip() {
        let variants = [
            DetectionMethod::FilenameStem,
            DetectionMethod::ContentIdentifier,
            DetectionMethod::Timecode,
            DetectionMethod::Manual,
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).unwrap();
            let roundtrip: DetectionMethod = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, roundtrip);
        }
    }

    #[test]
    fn test_serde_values() {
        assert_eq!(
            serde_json::to_string(&DetectionMethod::FilenameStem).unwrap(),
            "\"filename_stem\""
        );
        assert_eq!(
            serde_json::to_string(&DetectionMethod::ContentIdentifier).unwrap(),
            "\"content_identifier\""
        );
    }
}
