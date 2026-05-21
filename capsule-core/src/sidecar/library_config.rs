use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryConfigCbor {
    pub schema_version: u8,
    pub library_name: String,
    pub last_opened_at: i64,
    pub last_scrubbed_at: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cbor_roundtrip<
        T: serde::Serialize + for<'de> serde::Deserialize<'de> + PartialEq + std::fmt::Debug,
    >(
        val: &T,
    ) -> T {
        let mut buf = vec![];
        ciborium::ser::into_writer(val, &mut buf).unwrap();
        ciborium::de::from_reader(buf.as_slice()).unwrap()
    }

    #[test]
    fn test_round_trip_with_scrubbed() {
        let cfg = LibraryConfigCbor {
            schema_version: 1,
            library_name: "My Photos".to_string(),
            last_opened_at: 1720000000,
            last_scrubbed_at: Some(1719990000),
        };
        assert_eq!(cfg, cbor_roundtrip(&cfg));
    }

    #[test]
    fn test_round_trip_no_scrubbed() {
        let cfg = LibraryConfigCbor {
            schema_version: 1,
            library_name: "Library".to_string(),
            last_opened_at: 1720000000,
            last_scrubbed_at: None,
        };
        assert_eq!(cfg, cbor_roundtrip(&cfg));
    }
}
