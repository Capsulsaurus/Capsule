use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportMode {
    Copy,
    Move,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serde_round_trip() {
        for variant in [ImportMode::Copy, ImportMode::Move] {
            let json = serde_json::to_string(&variant).unwrap();
            let roundtrip: ImportMode = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, roundtrip);
        }
    }

    #[test]
    fn test_serde_values() {
        assert_eq!(
            serde_json::to_string(&ImportMode::Copy).unwrap(),
            "\"copy\""
        );
        assert_eq!(
            serde_json::to_string(&ImportMode::Move).unwrap(),
            "\"move\""
        );
    }
}
