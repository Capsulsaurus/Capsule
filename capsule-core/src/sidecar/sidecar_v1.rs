//! The CBOR sidecar schema v1 — the canonical, plaintext-local-only, **signed** metadata
//! record for an asset (SSoT: [Metadata — Sidecar Schema v1]).
//!
//! It is self-describing: `sidecar_schema` is **CBOR field 0** (an integer map key, which
//! sorts before every text key in canonical order), so a reader detects a schema it does
//! not implement before parsing the rest. The signature covers every byte including the
//! preserved `_unknown` map, so stripping unknown fields invalidates it.
//!
//! [Metadata — Sidecar Schema v1]: https://docs/design/metadata/#sidecar-schema-v1

use std::collections::BTreeMap;

use ciborium::value::Value;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::cbor;
use crate::crypto::hash::Hash32;
use crate::crypto::keys::{HybridSignature, HybridSigningKey, HybridVerifyingKey};
use crate::domain::StackType;
use crate::metadata::crdt::{Lww, OrSet};

/// The current sidecar schema version.
pub const SIDECAR_SCHEMA_V1: u16 = 1;

/// Pixel dimensions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dimensions {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Low-quality image placeholder (image-derived; lives in the encrypted sidecar).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lqip {
    /// Chromahash bytes.
    #[serde(with = "serde_bytes")]
    pub chromahash: Vec<u8>,
    /// LQIP format version.
    pub format_version: u16,
    /// Dominant color (RGB).
    pub dominant_color: [u8; 3],
}

/// Camera identifier (fingerprinting surface — stripped on export by default).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraId {
    /// Camera model.
    pub model: String,
    /// Per-device serial number.
    pub serial: String,
}

/// The source of a GPS fix (closed enum per protocol version).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GpsSource {
    /// From the file's EXIF.
    Exif,
    /// User-entered.
    Manual,
    /// Derived (e.g. from a nearby asset).
    Derived,
}

/// WGS-84 geolocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gps {
    /// Latitude (WGS-84).
    pub lat: f64,
    /// Longitude (WGS-84).
    pub lon: f64,
    /// Provenance of the fix.
    pub source: GpsSource,
}

/// An AI-suggested tag (kept in a structurally separate OR-set from user tags).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AiTag {
    /// The tag text.
    pub tag: String,
    /// The model that produced it.
    pub model_id: String,
    /// The model version.
    pub model_version: String,
}

/// Role of an asset within a stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StackRole {
    /// The stack's representative ("best photo").
    Primary,
    /// An ordinary member.
    Member,
    /// A proxy/optimized variant.
    Proxy,
}

/// Stack grouping for this asset (metadata-only; never touches asset bytes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackMembership {
    /// The stack id (UUIDv7).
    pub stack_id: Uuid,
    /// Closed stack-type enum.
    pub stack_type: StackType,
    /// This asset's role in the stack.
    pub role: StackRole,
    /// Ordering within the stack (burst sequence, chapter index).
    pub member_index: Option<u32>,
}

/// The signed CBOR sidecar v1.
#[derive(Debug, Clone, PartialEq)]
pub struct SidecarV1 {
    /// Schema version — serialized as integer **field 0** (sorts first canonically).
    pub sidecar_schema: u16,
    /// Matches the asset's manifest.
    pub crypto_suite_id: u16,
    /// Asset id (UUIDv7).
    pub uuid: Uuid,
    /// Canonical plaintext digest.
    pub hash: Hash32,
    /// RFC3339 capture time.
    pub capture_timestamp: String,
    /// RFC3339 import time.
    pub import_timestamp: String,
    /// Closed content-type string per protocol version.
    pub content_type: String,
    /// Pixel dimensions, if known.
    pub dimensions: Option<Dimensions>,
    /// Display placeholder.
    pub lqip: Option<Lqip>,
    /// User tags (OR-set).
    pub tags_user: OrSet<String>,
    /// AI tags (separate OR-set; cannot overwrite user tags).
    pub tags_ai: OrSet<AiTag>,
    /// Caption LWW register (with superseded log).
    pub caption: Lww<String>,
    /// Rating LWW register.
    pub rating: Lww<u8>,
    /// Stack membership, if any.
    pub stack_membership: Option<StackMembership>,
    /// Camera identifier (export-stripped).
    pub camera_id: Option<CameraId>,
    /// Importing device id (UUIDv4; export-stripped).
    pub device_id: Uuid,
    /// Importing session id (UUIDv7; export-stripped).
    pub session_id: Uuid,
    /// Geolocation (export-rounded).
    pub gps: Option<Gps>,
    /// Hash of the latest provenance record for this asset.
    pub provenance_chain_hash: Hash32,
    /// Unknown CBOR keys preserved verbatim (re-sorted canonically; covered by signature).
    pub unknown: BTreeMap<String, Value>,
    /// Hybrid signature over every byte above (the canonical map minus this field).
    pub signature: Option<HybridSignature>,
}

fn to_value<T: Serialize>(v: &T) -> Value {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(v, &mut buf).expect("sidecar field serializes");
    ciborium::de::from_reader(buf.as_slice()).expect("sidecar field re-reads")
}

fn from_value<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T, String> {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&v, &mut buf).map_err(|e| e.to_string())?;
    ciborium::de::from_reader(buf.as_slice()).map_err(|e| e.to_string())
}

impl SidecarV1 {
    /// Build the CBOR map (as ordered entries) with `sidecar_schema` at integer key 0 and
    /// every other field under a text key. If `include_signature`, the `signature` entry is
    /// appended (used for the full record; excluded for signing bytes).
    fn to_entries(&self, include_signature: bool) -> Vec<(Value, Value)> {
        let mut m: Vec<(Value, Value)> = Vec::new();
        // Field 0: schema version (integer key — sorts before all text keys).
        m.push((
            Value::Integer(0u8.into()),
            Value::Integer(self.sidecar_schema.into()),
        ));

        macro_rules! put {
            ($k:literal, $v:expr) => {
                m.push((Value::Text($k.to_string()), to_value(&$v)));
            };
        }
        macro_rules! put_opt {
            ($k:literal, $v:expr) => {
                if let Some(inner) = &$v {
                    m.push((Value::Text($k.to_string()), to_value(inner)));
                }
            };
        }
        put!("crypto_suite_id", self.crypto_suite_id);
        put!("uuid", self.uuid);
        put!("hash", self.hash);
        put!("capture_timestamp", self.capture_timestamp);
        put!("import_timestamp", self.import_timestamp);
        put!("content_type", self.content_type);
        put_opt!("dimensions", self.dimensions);
        put_opt!("lqip", self.lqip);
        put!("tags_user", self.tags_user);
        put!("tags_ai", self.tags_ai);
        put!("caption", self.caption);
        put!("rating", self.rating);
        put_opt!("stack_membership", self.stack_membership);
        put_opt!("camera_id", self.camera_id);
        put!("device_id", self.device_id);
        put!("session_id", self.session_id);
        put_opt!("gps", self.gps);
        put!("provenance_chain_hash", self.provenance_chain_hash);

        // Merge preserved unknown fields (canonical encode re-sorts everything).
        for (k, v) in &self.unknown {
            m.push((Value::Text(k.clone()), v.clone()));
        }
        if include_signature && let Some(sig) = &self.signature {
            m.push((Value::Text("signature".to_string()), to_value(sig)));
        }
        m
    }

    /// The canonical bytes the signature covers (everything except `signature`).
    pub fn signing_bytes(&self) -> Vec<u8> {
        cbor::value_to_canonical_vec(&Value::Map(self.to_entries(false)))
    }

    /// The full canonical CBOR encoding (including the signature, if present).
    pub fn to_canonical_vec(&self) -> Vec<u8> {
        cbor::value_to_canonical_vec(&Value::Map(self.to_entries(true)))
    }

    /// Sign the sidecar with the user IK, setting `signature`.
    pub fn sign(&mut self, ik: &HybridSigningKey) {
        self.signature = Some(ik.sign(&self.signing_bytes()));
    }

    /// Verify the sidecar's signature against the user IK public key.
    pub fn verify(&self, ik_public: &HybridVerifyingKey) -> bool {
        match &self.signature {
            Some(sig) => ik_public.verify(&self.signing_bytes(), sig),
            None => false,
        }
    }

    /// Decode a sidecar from canonical CBOR bytes, refusing a schema newer than `max_known`
    /// ([Schema Versioning Rules]: an old client must not strip-and-resign a newer sidecar).
    ///
    /// [Schema Versioning Rules]: https://docs/design/metadata/#schema-versioning-rules
    pub fn from_canonical_slice(bytes: &[u8], max_known: u16) -> Result<Self, String> {
        let value: Value =
            ciborium::de::from_reader(bytes).map_err(|e| format!("cbor decode: {e}"))?;
        let Value::Map(entries) = value else {
            return Err("sidecar must be a CBOR map".into());
        };

        let mut schema: Option<u16> = None;
        let mut text: BTreeMap<String, Value> = BTreeMap::new();
        for (k, v) in entries {
            match k {
                Value::Integer(i) if i128::from(i) == 0 => {
                    schema = Some(from_value::<u16>(v)?);
                }
                Value::Text(key) => {
                    text.insert(key, v);
                }
                _ => return Err("unexpected sidecar map key".into()),
            }
        }
        let sidecar_schema = schema.ok_or("missing sidecar_schema (field 0)")?;
        if sidecar_schema > max_known {
            return Err(format!(
                "sidecar_schema {sidecar_schema} newer than max known {max_known}; refusing"
            ));
        }

        macro_rules! req {
            ($k:literal, $t:ty) => {
                from_value::<$t>(text.remove($k).ok_or(concat!("missing field: ", $k))?)?
            };
        }
        macro_rules! opt {
            ($k:literal, $t:ty) => {
                match text.remove($k) {
                    None | Some(Value::Null) => None,
                    Some(v) => Some(from_value::<$t>(v)?),
                }
            };
        }

        let crypto_suite_id = req!("crypto_suite_id", u16);
        let uuid = req!("uuid", Uuid);
        let hash = req!("hash", Hash32);
        let capture_timestamp = req!("capture_timestamp", String);
        let import_timestamp = req!("import_timestamp", String);
        let content_type = req!("content_type", String);
        let dimensions = opt!("dimensions", Dimensions);
        let lqip = opt!("lqip", Lqip);
        let tags_user = req!("tags_user", OrSet<String>);
        let tags_ai = req!("tags_ai", OrSet<AiTag>);
        let caption = req!("caption", Lww<String>);
        let rating = req!("rating", Lww<u8>);
        let stack_membership = opt!("stack_membership", StackMembership);
        let camera_id = opt!("camera_id", CameraId);
        let device_id = req!("device_id", Uuid);
        let session_id = req!("session_id", Uuid);
        let gps = opt!("gps", Gps);
        let provenance_chain_hash = req!("provenance_chain_hash", Hash32);
        let signature = opt!("signature", HybridSignature);

        Ok(SidecarV1 {
            sidecar_schema,
            crypto_suite_id,
            uuid,
            hash,
            capture_timestamp,
            import_timestamp,
            content_type,
            dimensions,
            lqip,
            tags_user,
            tags_ai,
            caption,
            rating,
            stack_membership,
            camera_id,
            device_id,
            session_id,
            gps,
            provenance_chain_hash,
            unknown: text, // whatever remains is unknown — preserved verbatim
            signature,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::primitives::CRYPTO_SUITE_ID;

    fn minimal() -> SidecarV1 {
        SidecarV1 {
            sidecar_schema: SIDECAR_SCHEMA_V1,
            crypto_suite_id: CRYPTO_SUITE_ID,
            uuid: Uuid::from_u128(0x7777),
            hash: Hash32([0xAB; 32]),
            capture_timestamp: "2026-05-31T10:00:00Z".into(),
            import_timestamp: "2026-05-31T11:00:00Z".into(),
            content_type: "image/jpeg".into(),
            dimensions: Some(Dimensions {
                width: 4032,
                height: 3024,
            }),
            lqip: None,
            tags_user: OrSet::new(),
            tags_ai: OrSet::new(),
            caption: Lww::new(),
            rating: Lww::new(),
            stack_membership: None,
            camera_id: Some(CameraId {
                model: "iPhone 15 Pro".into(),
                serial: "ABC123".into(),
            }),
            device_id: Uuid::from_u128(0xD1),
            session_id: Uuid::from_u128(0x5E),
            gps: Some(Gps {
                lat: 40.7128,
                lon: -74.0060,
                source: GpsSource::Exif,
            }),
            provenance_chain_hash: Hash32([0xCC; 32]),
            unknown: BTreeMap::new(),
            signature: None,
        }
    }

    #[test]
    fn schema_is_field_zero_canonically_first() {
        let s = minimal();
        let bytes = s.to_canonical_vec();
        // map head (0xAX for small maps) then the first key must be integer 0 (0x00),
        // then its value (schema = 1 → 0x01).
        assert!(
            (0xa0..=0xbb).contains(&bytes[0]),
            "expected a CBOR map head"
        );
        assert_eq!(bytes[1], 0x00, "first map key must be integer 0");
        assert_eq!(bytes[2], 0x01, "schema value 1");
    }

    #[test]
    fn sign_verify_round_trip_through_canonical_cbor() {
        let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let mut s = minimal();
        s.sign(&ik);
        assert!(s.verify(&ik.verifying_key()));

        let bytes = s.to_canonical_vec();
        let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
        assert_eq!(back, s);
        assert!(back.verify(&ik.verifying_key()));
    }

    #[test]
    fn tampering_after_signing_breaks_verification() {
        let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let mut s = minimal();
        s.sign(&ik);
        s.content_type = "image/heic".into(); // change a field without re-signing
        assert!(!s.verify(&ik.verifying_key()));
    }

    #[test]
    fn unknown_fields_are_preserved_and_signed() {
        let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let mut s = minimal();
        s.unknown
            .insert("future_field".into(), Value::Text("future_value".into()));
        s.sign(&ik);

        let bytes = s.to_canonical_vec();
        let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
        assert_eq!(
            back.unknown.get("future_field"),
            Some(&Value::Text("future_value".into()))
        );
        // Signature still verifies (it covered the unknown field)...
        assert!(back.verify(&ik.verifying_key()));
        // ...and stripping the unknown field invalidates the signature.
        let mut stripped = back.clone();
        stripped.unknown.clear();
        assert!(!stripped.verify(&ik.verifying_key()));
    }

    #[test]
    fn refuses_a_schema_newer_than_known() {
        let mut s = minimal();
        s.sidecar_schema = 99;
        let bytes = s.to_canonical_vec();
        // A client whose max known schema is 1 refuses to read schema 99.
        assert!(SidecarV1::from_canonical_slice(&bytes, 1).is_err());
        // A future client that knows schema 99 reads it.
        assert!(SidecarV1::from_canonical_slice(&bytes, 99).is_ok());
    }

    #[test]
    fn canonical_encoding_is_deterministic() {
        let s = minimal();
        assert_eq!(s.to_canonical_vec(), s.to_canonical_vec());
    }
}
