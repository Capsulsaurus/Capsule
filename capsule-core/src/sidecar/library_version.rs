use serde::{Deserialize, Serialize};

pub const CURRENT_LIBRARY_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryVersionCbor {
    pub version: u8,
}

impl Default for LibraryVersionCbor {
    fn default() -> Self {
        Self {
            version: CURRENT_LIBRARY_VERSION,
        }
    }
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
    fn test_round_trip() {
        let v = LibraryVersionCbor { version: 1 };
        assert_eq!(v, cbor_roundtrip(&v));
    }

    #[test]
    fn test_default_version() {
        assert_eq!(
            LibraryVersionCbor::default().version,
            CURRENT_LIBRARY_VERSION
        );
    }
}
