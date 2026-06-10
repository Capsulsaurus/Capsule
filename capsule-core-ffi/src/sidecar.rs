//! CBOR sidecar (de)serialisation exposed across the FFI boundary.
//!
//! The canonical sidecar format lives in `capsule-core`. Swift never re-encodes
//! it: it builds an [`AssetSidecarRecord`], calls [`serialize_sidecar`] to get
//! the bytes to write to disk, and calls [`deserialize_sidecar`] to read them
//! back. Sidecar fields this build does not recognise are carried through the
//! `unknown_fields_cbor` blob verbatim, so forward compatibility is preserved
//! without Swift needing a CBOR implementation of its own.

use std::collections::BTreeMap;

use capsule_core::sidecar::{AssetSidecar, StackHint};
use ciborium::value::Value;

use crate::error::CatalogError;

/// The CBOR sidecar paired with every managed media file.
///
/// Enum-typed fields (`asset_type`, `import_mode`, `capture_tz_source`) carry
/// their canonical snake_case string values.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct AssetSidecarRecord {
    pub version: u8,
    pub uuid: String,
    pub asset_type: String,
    pub original_filename: String,
    pub import_timestamp: i64,
    pub modified_timestamp: i64,
    pub hash_sha256: String,
    pub file_size: u64,
    pub is_deleted: bool,
    pub rating: u8,
    pub tags: Vec<String>,
    pub import_mode: String,
    pub importer_version: String,
    pub rawshift_version: String,
    pub capture_timestamp: Option<i64>,
    pub capture_utc: Option<i64>,
    pub capture_tz: Option<String>,
    pub capture_tz_source: Option<String>,
    pub tz_db_version: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub stack_hint: Option<StackHintRecord>,
    pub album_id: Option<String>,
    pub deleted_at: Option<i64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    /// Opaque CBOR encoding of sidecar fields this build does not recognise.
    /// Round-tripped verbatim; empty when there are none. Never inspected by Swift.
    pub unknown_fields_cbor: Vec<u8>,
}

/// Stack-membership hint stored in a sidecar. Enum fields carry snake_case values.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct StackHintRecord {
    pub detection_key: String,
    pub detection_method: String,
    pub member_role: String,
    pub stack_type: String,
}

/// Serialise an [`AssetSidecarRecord`] to canonical CBOR bytes.
#[uniffi::export]
pub fn serialize_sidecar(record: AssetSidecarRecord) -> Result<Vec<u8>, CatalogError> {
    let sidecar = record_to_sidecar(record)?;
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&sidecar, &mut buf).map_err(|e| CatalogError::Sidecar {
        message: format!("failed to encode sidecar: {e}"),
    })?;
    Ok(buf)
}

/// Decode canonical CBOR bytes into an [`AssetSidecarRecord`].
#[uniffi::export]
pub fn deserialize_sidecar(bytes: Vec<u8>) -> Result<AssetSidecarRecord, CatalogError> {
    let sidecar: AssetSidecar =
        ciborium::de::from_reader(bytes.as_slice()).map_err(|e| CatalogError::Sidecar {
            message: format!("failed to decode sidecar: {e}"),
        })?;
    sidecar_to_record(&sidecar)
}

// ── Conversion ───────────────────────────────────────────────────────────────

fn record_to_sidecar(r: AssetSidecarRecord) -> Result<AssetSidecar, CatalogError> {
    Ok(AssetSidecar {
        version: r.version,
        uuid: r.uuid,
        asset_type: enum_from_string(&r.asset_type, "asset_type")?,
        original_filename: r.original_filename,
        import_timestamp: r.import_timestamp,
        modified_timestamp: r.modified_timestamp,
        hash_sha256: r.hash_sha256,
        file_size: r.file_size,
        is_deleted: r.is_deleted,
        rating: r.rating,
        tags: r.tags,
        import_mode: enum_from_string(&r.import_mode, "import_mode")?,
        importer_version: r.importer_version,
        rawshift_version: r.rawshift_version,
        capture_timestamp: r.capture_timestamp,
        capture_utc: r.capture_utc,
        capture_tz: r.capture_tz,
        capture_tz_source: r
            .capture_tz_source
            .as_deref()
            .map(|s| enum_from_string(s, "capture_tz_source"))
            .transpose()?,
        tz_db_version: r.tz_db_version,
        width: r.width,
        height: r.height,
        duration_ms: r.duration_ms,
        stack_hint: r.stack_hint.map(record_to_stack_hint).transpose()?,
        album_id: r.album_id,
        deleted_at: r.deleted_at,
        camera_make: r.camera_make,
        camera_model: r.camera_model,
        gps_lat: r.gps_lat,
        gps_lon: r.gps_lon,
        unknown_fields: decode_unknown_fields(&r.unknown_fields_cbor)?,
    })
}

fn sidecar_to_record(s: &AssetSidecar) -> Result<AssetSidecarRecord, CatalogError> {
    Ok(AssetSidecarRecord {
        version: s.version,
        uuid: s.uuid.clone(),
        asset_type: enum_to_string(&s.asset_type, "asset_type")?,
        original_filename: s.original_filename.clone(),
        import_timestamp: s.import_timestamp,
        modified_timestamp: s.modified_timestamp,
        hash_sha256: s.hash_sha256.clone(),
        file_size: s.file_size,
        is_deleted: s.is_deleted,
        rating: s.rating,
        tags: s.tags.clone(),
        import_mode: enum_to_string(&s.import_mode, "import_mode")?,
        importer_version: s.importer_version.clone(),
        rawshift_version: s.rawshift_version.clone(),
        capture_timestamp: s.capture_timestamp,
        capture_utc: s.capture_utc,
        capture_tz: s.capture_tz.clone(),
        capture_tz_source: s
            .capture_tz_source
            .as_ref()
            .map(|v| enum_to_string(v, "capture_tz_source"))
            .transpose()?,
        tz_db_version: s.tz_db_version.clone(),
        width: s.width,
        height: s.height,
        duration_ms: s.duration_ms,
        stack_hint: s
            .stack_hint
            .as_ref()
            .map(stack_hint_to_record)
            .transpose()?,
        album_id: s.album_id.clone(),
        deleted_at: s.deleted_at,
        camera_make: s.camera_make.clone(),
        camera_model: s.camera_model.clone(),
        gps_lat: s.gps_lat,
        gps_lon: s.gps_lon,
        unknown_fields_cbor: encode_unknown_fields(&s.unknown_fields)?,
    })
}

fn record_to_stack_hint(r: StackHintRecord) -> Result<StackHint, CatalogError> {
    Ok(StackHint {
        detection_key: r.detection_key,
        detection_method: enum_from_string(&r.detection_method, "detection_method")?,
        member_role: enum_from_string(&r.member_role, "member_role")?,
        stack_type: enum_from_string(&r.stack_type, "stack_type")?,
    })
}

fn stack_hint_to_record(h: &StackHint) -> Result<StackHintRecord, CatalogError> {
    Ok(StackHintRecord {
        detection_key: h.detection_key.clone(),
        detection_method: enum_to_string(&h.detection_method, "detection_method")?,
        member_role: enum_to_string(&h.member_role, "member_role")?,
        stack_type: enum_to_string(&h.stack_type, "stack_type")?,
    })
}

// ── Enum <-> canonical string ────────────────────────────────────────────────
//
// `capsule-core`'s domain enums all derive serde with `rename_all = "snake_case"`,
// so a JSON round-trip yields exactly the canonical string the catalog and
// sidecar use — no hand-written mapping tables to drift out of sync.

fn enum_to_string<T: serde::Serialize>(value: &T, field: &str) -> Result<String, CatalogError> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => Ok(s),
        Ok(other) => Err(CatalogError::Sidecar {
            message: format!("field '{field}' did not serialise to a string: {other}"),
        }),
        Err(e) => Err(CatalogError::Sidecar {
            message: format!("field '{field}': {e}"),
        }),
    }
}

fn enum_from_string<T: serde::de::DeserializeOwned>(
    s: &str,
    field: &str,
) -> Result<T, CatalogError> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).map_err(|e| {
        CatalogError::Sidecar {
            message: format!("field '{field}' has invalid value '{s}': {e}"),
        }
    })
}

// ── Unknown-field CBOR blob ──────────────────────────────────────────────────

fn decode_unknown_fields(bytes: &[u8]) -> Result<BTreeMap<String, Value>, CatalogError> {
    if bytes.is_empty() {
        return Ok(BTreeMap::new());
    }
    let value: Value = ciborium::de::from_reader(bytes).map_err(|e| CatalogError::Sidecar {
        message: format!("unknown_fields_cbor is not valid CBOR: {e}"),
    })?;
    match value {
        Value::Map(entries) => {
            let mut map = BTreeMap::new();
            for (k, v) in entries {
                match k {
                    Value::Text(key) => {
                        map.insert(key, v);
                    }
                    _ => {
                        return Err(CatalogError::Sidecar {
                            message: "unknown_fields_cbor contains a non-text key".to_string(),
                        });
                    }
                }
            }
            Ok(map)
        }
        _ => Err(CatalogError::Sidecar {
            message: "unknown_fields_cbor is not a CBOR map".to_string(),
        }),
    }
}

fn encode_unknown_fields(map: &BTreeMap<String, Value>) -> Result<Vec<u8>, CatalogError> {
    if map.is_empty() {
        return Ok(Vec::new());
    }
    let value = Value::Map(
        map.iter()
            .map(|(k, v)| (Value::Text(k.clone()), v.clone()))
            .collect(),
    );
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&value, &mut buf).map_err(|e| CatalogError::Sidecar {
        message: format!("failed to encode unknown_fields: {e}"),
    })?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_record() -> AssetSidecarRecord {
        AssetSidecarRecord {
            version: 1,
            uuid: "01956ef3-0000-7000-8000-000000000001".to_string(),
            asset_type: "photo".to_string(),
            original_filename: "IMG_1234.jpg".to_string(),
            import_timestamp: 1_720_000_000,
            modified_timestamp: 1_720_000_000,
            hash_sha256: "a".repeat(64),
            file_size: 2048,
            is_deleted: false,
            rating: 0,
            tags: vec![],
            import_mode: "copy".to_string(),
            importer_version: "0.1.0".to_string(),
            rawshift_version: "0.0.0".to_string(),
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
            unknown_fields_cbor: Vec::new(),
        }
    }

    #[test]
    fn test_sidecar_minimal_roundtrip() {
        let record = minimal_record();
        let bytes = serialize_sidecar(record.clone()).unwrap();
        let decoded = deserialize_sidecar(bytes).unwrap();
        assert_eq!(decoded, record);
    }

    #[test]
    fn test_sidecar_full_roundtrip_with_stack_hint() {
        let mut record = minimal_record();
        record.capture_tz_source = Some("gps_lookup".to_string());
        record.width = Some(4032);
        record.height = Some(3024);
        record.tags = vec!["trip".to_string(), "2024".to_string()];
        record.gps_lat = Some(40.7128);
        record.gps_lon = Some(-74.0060);
        record.stack_hint = Some(StackHintRecord {
            detection_key: "apple-content-id".to_string(),
            detection_method: "content_identifier".to_string(),
            member_role: "primary".to_string(),
            stack_type: "live_photo".to_string(),
        });
        let bytes = serialize_sidecar(record.clone()).unwrap();
        let decoded = deserialize_sidecar(bytes).unwrap();
        assert_eq!(decoded, record);
    }

    #[test]
    fn test_invalid_enum_value_is_rejected() {
        let mut record = minimal_record();
        record.asset_type = "not_a_real_type".to_string();
        assert!(matches!(
            serialize_sidecar(record),
            Err(CatalogError::Sidecar { .. })
        ));
    }

    #[test]
    fn test_unknown_fields_preserved() {
        // A sidecar field written by a future build must survive a full
        // decode → re-encode → decode cycle.
        let mut unknown = BTreeMap::new();
        unknown.insert(
            "future_field".to_string(),
            Value::Text("future_value".to_string()),
        );
        let mut record = minimal_record();
        record.unknown_fields_cbor = encode_unknown_fields(&unknown).unwrap();

        let bytes = serialize_sidecar(record).unwrap();
        let decoded = deserialize_sidecar(bytes).unwrap();

        let decoded_unknown = decode_unknown_fields(&decoded.unknown_fields_cbor).unwrap();
        assert_eq!(
            decoded_unknown.get("future_field"),
            Some(&Value::Text("future_value".to_string()))
        );
    }
}
