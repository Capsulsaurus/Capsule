use crate::domain::{CaptureTzSource, ImportMode};
use crate::metadata::AssetType;
use crate::sidecar::StackHint;
use ciborium::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// CBOR sidecar for a media asset. Unknown fields are preserved verbatim
/// for forward compatibility (Postel's Law).
#[derive(Debug, Clone, PartialEq)]
pub struct AssetSidecar {
    // Required fields
    pub version: u8,
    pub uuid: String,
    pub asset_type: AssetType,
    pub original_filename: String,
    pub import_timestamp: i64,
    pub modified_timestamp: i64,
    pub hash_blake3: String,
    pub file_size: u64,
    pub is_deleted: bool,
    pub rating: u8,
    pub tags: Vec<String>,
    pub import_mode: ImportMode,
    pub importer_version: String,
    pub rawshift_version: String,

    // Optional fields
    pub capture_timestamp: Option<i64>,
    pub capture_utc: Option<i64>,
    pub capture_tz: Option<String>,
    pub capture_tz_source: Option<CaptureTzSource>,
    pub tz_db_version: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub stack_hint: Option<StackHint>,
    pub album_id: Option<String>,
    pub deleted_at: Option<i64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,

    /// Unknown fields preserved for forward compatibility.
    pub unknown_fields: BTreeMap<String, Value>,
}

/// Re-encode a `Value` into CBOR bytes and deserialize as `T`.
fn value_to<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T, String> {
    let mut buf = vec![];
    ciborium::ser::into_writer(&v, &mut buf).map_err(|e| e.to_string())?;
    ciborium::de::from_reader(buf.as_slice()).map_err(|e| e.to_string())
}

/// Serialize `T` to CBOR bytes and deserialize as a `Value`.
fn to_value<T: Serialize>(v: &T) -> Result<Value, String> {
    let mut buf = vec![];
    ciborium::ser::into_writer(v, &mut buf).map_err(|e| e.to_string())?;
    ciborium::de::from_reader(buf.as_slice()).map_err(|e| e.to_string())
}

impl Serialize for AssetSidecar {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error;

        let mut map: Vec<(Value, Value)> = Vec::new();

        macro_rules! insert {
            ($key:expr, $val:expr) => {{
                let v = to_value(&$val).map_err(S::Error::custom)?;
                map.push((Value::Text($key.to_string()), v));
            }};
        }
        macro_rules! insert_opt {
            ($key:expr, $val:expr) => {{
                if let Some(ref inner) = $val {
                    let v = to_value(inner).map_err(S::Error::custom)?;
                    map.push((Value::Text($key.to_string()), v));
                }
            }};
        }

        insert!("version", self.version);
        insert!("uuid", self.uuid);
        insert!("asset_type", self.asset_type);
        insert!("original_filename", self.original_filename);
        insert!("import_timestamp", self.import_timestamp);
        insert!("modified_timestamp", self.modified_timestamp);
        insert!("hash_blake3", self.hash_blake3);
        insert!("file_size", self.file_size);
        insert!("is_deleted", self.is_deleted);
        insert!("rating", self.rating);
        insert!("tags", self.tags);
        insert!("import_mode", self.import_mode);
        insert!("importer_version", self.importer_version);
        insert!("rawshift_version", self.rawshift_version);
        insert_opt!("capture_timestamp", self.capture_timestamp);
        insert_opt!("capture_utc", self.capture_utc);
        insert_opt!("capture_tz", self.capture_tz);
        insert_opt!("capture_tz_source", self.capture_tz_source);
        insert_opt!("tz_db_version", self.tz_db_version);
        insert_opt!("width", self.width);
        insert_opt!("height", self.height);
        insert_opt!("duration_ms", self.duration_ms);
        insert_opt!("stack_hint", self.stack_hint);
        insert_opt!("album_id", self.album_id);
        insert_opt!("deleted_at", self.deleted_at);
        insert_opt!("camera_make", self.camera_make);
        insert_opt!("camera_model", self.camera_model);
        insert_opt!("gps_lat", self.gps_lat);
        insert_opt!("gps_lon", self.gps_lon);

        // Merge unknown fields last so they are preserved verbatim.
        for (k, v) in &self.unknown_fields {
            map.push((Value::Text(k.clone()), v.clone()));
        }

        Value::Map(map).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AssetSidecar {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;

        let value = Value::deserialize(deserializer)?;
        let raw_map = match value {
            Value::Map(m) => m,
            _ => return Err(D::Error::custom("expected CBOR map for AssetSidecar")),
        };

        // Collect into a BTreeMap keyed by string for easy lookup.
        let mut fields: BTreeMap<String, Value> = BTreeMap::new();
        for (k, v) in raw_map {
            if let Value::Text(key) = k {
                fields.insert(key, v);
            }
            // Non-text keys are silently dropped (not expected in our format).
        }

        macro_rules! req {
            ($key:expr, $t:ty) => {{
                let val = fields
                    .remove($key)
                    .ok_or_else(|| D::Error::custom(format!("missing required field: {}", $key)))?;
                value_to::<$t>(val).map_err(D::Error::custom)?
            }};
        }
        macro_rules! opt {
            ($key:expr, $t:ty) => {{
                match fields.remove($key) {
                    None | Some(Value::Null) => None,
                    Some(v) => Some(value_to::<$t>(v).map_err(D::Error::custom)?),
                }
            }};
        }

        let version = req!("version", u8);
        let uuid = req!("uuid", String);
        let asset_type = req!("asset_type", AssetType);
        let original_filename = req!("original_filename", String);
        let import_timestamp = req!("import_timestamp", i64);
        let modified_timestamp = req!("modified_timestamp", i64);
        let hash_blake3 = req!("hash_blake3", String);
        let file_size = req!("file_size", u64);
        let is_deleted = req!("is_deleted", bool);
        let rating = req!("rating", u8);
        let tags = req!("tags", Vec<String>);
        let import_mode = req!("import_mode", ImportMode);
        let importer_version = req!("importer_version", String);
        let rawshift_version = req!("rawshift_version", String);

        let capture_timestamp = opt!("capture_timestamp", i64);
        let capture_utc = opt!("capture_utc", i64);
        let capture_tz = opt!("capture_tz", String);
        let capture_tz_source = opt!("capture_tz_source", CaptureTzSource);
        let tz_db_version = opt!("tz_db_version", String);
        let width = opt!("width", u32);
        let height = opt!("height", u32);
        let duration_ms = opt!("duration_ms", u64);
        let stack_hint = opt!("stack_hint", StackHint);
        let album_id = opt!("album_id", String);
        let deleted_at = opt!("deleted_at", i64);
        let camera_make = opt!("camera_make", String);
        let camera_model = opt!("camera_model", String);
        let gps_lat = opt!("gps_lat", f64);
        let gps_lon = opt!("gps_lon", f64);

        // Any remaining fields are unknown — preserve them.
        let unknown_fields = fields;

        Ok(AssetSidecar {
            version,
            uuid,
            asset_type,
            original_filename,
            import_timestamp,
            modified_timestamp,
            hash_blake3,
            file_size,
            is_deleted,
            rating,
            tags,
            import_mode,
            importer_version,
            rawshift_version,
            capture_timestamp,
            capture_utc,
            capture_tz,
            capture_tz_source,
            tz_db_version,
            width,
            height,
            duration_ms,
            stack_hint,
            album_id,
            deleted_at,
            camera_make,
            camera_model,
            gps_lat,
            gps_lon,
            unknown_fields,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CaptureTzSource, DetectionMethod, ImportMode, MemberRole, StackType};
    use crate::metadata::AssetType;
    use crate::sidecar::StackHint;
    use std::collections::BTreeMap;

    fn minimal_sidecar() -> AssetSidecar {
        AssetSidecar {
            version: 1,
            uuid: "01956ef3-0000-7000-8000-000000000001".to_string(),
            asset_type: AssetType::Photo,
            original_filename: "IMG_1234.jpg".to_string(),
            import_timestamp: 1720000000,
            modified_timestamp: 1720000000,
            hash_blake3: "a".repeat(64),
            file_size: 1024 * 1024,
            is_deleted: false,
            rating: 0,
            tags: vec![],
            import_mode: ImportMode::Copy,
            importer_version: "0.1.0".to_string(),
            rawshift_version: "0.1.0".to_string(),
            capture_timestamp: None,
            capture_utc: None,
            capture_tz: None,
            capture_tz_source: None,
            tz_db_version: None,
            width: None,
            height: None,
            duration_ms: None,
            stack_hint: None,
            album_id: None,
            deleted_at: None,
            camera_make: None,
            camera_model: None,
            gps_lat: None,
            gps_lon: None,
            unknown_fields: BTreeMap::new(),
        }
    }

    fn cbor_roundtrip(s: &AssetSidecar) -> AssetSidecar {
        let mut buf = vec![];
        ciborium::ser::into_writer(s, &mut buf).unwrap();
        ciborium::de::from_reader(buf.as_slice()).unwrap()
    }

    #[test]
    fn test_minimal_roundtrip() {
        let s = minimal_sidecar();
        assert_eq!(s, cbor_roundtrip(&s));
    }

    #[test]
    fn test_full_roundtrip() {
        let mut s = minimal_sidecar();
        s.capture_timestamp = Some(1719990000);
        s.capture_utc = Some(1719986400);
        s.capture_tz = Some("America/New_York".to_string());
        s.capture_tz_source = Some(CaptureTzSource::GpsLookup);
        s.tz_db_version = Some("2024b".to_string());
        s.width = Some(4032);
        s.height = Some(3024);
        s.camera_make = Some("Apple".to_string());
        s.camera_model = Some("iPhone 15 Pro".to_string());
        s.gps_lat = Some(40.7128);
        s.gps_lon = Some(-74.0060);
        s.tags = vec!["vacation".to_string(), "2024".to_string()];
        s.rating = 4;
        s.stack_hint = Some(StackHint {
            detection_key: "img_1234".to_string(),
            detection_method: DetectionMethod::FilenameStem,
            member_role: MemberRole::Primary,
            stack_type: StackType::RawJpeg,
        });
        assert_eq!(s, cbor_roundtrip(&s));
    }

    #[test]
    fn test_unknown_field_preservation() {
        let s = minimal_sidecar();
        let mut buf = vec![];
        ciborium::ser::into_writer(&s, &mut buf).unwrap();

        // Deserialize to a raw Value, inject an unknown field, re-serialize.
        let mut val: Value = ciborium::de::from_reader(buf.as_slice()).unwrap();
        if let Value::Map(ref mut entries) = val {
            entries.push((
                Value::Text("future_field".to_string()),
                Value::Text("future_value".to_string()),
            ));
        }
        let mut buf2 = vec![];
        ciborium::ser::into_writer(&val, &mut buf2).unwrap();

        // Deserialize as AssetSidecar — unknown field must be preserved.
        let decoded: AssetSidecar = ciborium::de::from_reader(buf2.as_slice()).unwrap();
        assert_eq!(
            decoded.unknown_fields.get("future_field"),
            Some(&Value::Text("future_value".to_string()))
        );

        // Re-serialize and re-deserialize — unknown field must survive a second round-trip.
        let mut buf3 = vec![];
        ciborium::ser::into_writer(&decoded, &mut buf3).unwrap();
        let decoded2: AssetSidecar = ciborium::de::from_reader(buf3.as_slice()).unwrap();
        assert_eq!(
            decoded2.unknown_fields.get("future_field"),
            Some(&Value::Text("future_value".to_string()))
        );
    }

    #[test]
    fn test_large_file_size() {
        // Verify u64 round-trips correctly for large values (> i64::MAX would fail, but
        // realistic file sizes well within u64 range should work).
        let mut s = minimal_sidecar();
        s.file_size = 10 * 1024 * 1024 * 1024; // 10 GiB
        assert_eq!(s, cbor_roundtrip(&s));
    }
}
