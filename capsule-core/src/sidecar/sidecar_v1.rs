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

use super::region::Regions;
use crate::cbor;
use crate::crypto::hash::Hash32;
use crate::crypto::keys::{HybridSignature, HybridSigningKey, HybridVerifyingKey};
use crate::domain::{GpsDatum, StackType};
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

/// Geolocation, stored **verbatim in the datum the source supplied** (SSoT:
/// [Metadata — Geolocation]). Never converted at rest; datum-tagged by [`datum`](Gps::datum).
///
/// [Metadata — Geolocation]: https://docs/design/metadata/#geolocation
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gps {
    /// Latitude, in [`datum`](Gps::datum).
    pub lat: f64,
    /// Longitude, in [`datum`](Gps::datum).
    pub lon: f64,
    /// Provenance of the fix.
    pub source: GpsSource,
    /// The coordinate datum `lat`/`lon` are expressed in. **Wire-absent when
    /// [`Wgs84`](GpsDatum::Wgs84)** (the default): a `Wgs84` fix omits this key so every
    /// pre-`datum` sidecar and known-answer vector stays byte-identical, and a `gps` value
    /// with no `datum` key decodes back to `Wgs84`. Additive optional key within sidecar
    /// schema v1 — no `sidecar_schema` bump. `datum` travels with `lat`/`lon` in one
    /// atomic `gps` write, so no CRDT merge rule changes.
    #[serde(default, skip_serializing_if = "GpsDatum::is_wgs84")]
    pub datum: GpsDatum,
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

/// Trinary culling flag (owner: Asset Organization — Culling). `Neutral` is the
/// never-flagged state and the wire-absent default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CullFlag {
    /// Marked as a keeper.
    Pick,
    /// Not yet culled either way (default).
    #[default]
    Neutral,
    /// Marked for rejection (filtered out, candidate for batch delete).
    Reject,
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
    /// RFC3339 capture time **as imported**: what the original's own metadata (or the
    /// exporter's record it was folded with) said when the asset entered the library. A
    /// user's correction never overwrites it; it lands in
    /// [`capture_timestamp_user`](Self::capture_timestamp_user), and
    /// [`effective_capture_timestamp`](Self::effective_capture_timestamp) is the instant the
    /// asset is dated by.
    pub capture_timestamp: String,
    /// The user's capture-time correction: an LWW register over an RFC3339 instant, written by
    /// `Workspace::set_capture_timestamp`. Wire-absent when never written, so every sidecar
    /// without a correction encodes byte-identically to before the field existed; an older
    /// reader keeps it verbatim as an unknown key.
    pub capture_timestamp_user: Lww<String>,
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
    /// Stack membership: an LWW register over `Option<StackMembership>` so stack
    /// edits (join, move, leave = `None`) converge order-independently under the
    /// same `(ts, device_id)` rule as caption/rating — grouping is convergent by
    /// construction, not convention. Wire-absent default = the never-written
    /// register (distinct from an explicit removal, which is a stamped `None`).
    pub stack_membership: Lww<Option<StackMembership>>,
    /// Labelled image regions: region id (UUIDv7) to its register, a stamped `None` being a
    /// deleted region (owner: [Organization — Labels]). Merged by
    /// [`merge_regions`](super::merge_regions), not as plain LWW. Wire-absent when empty, so
    /// every sidecar without regions encodes byte-identically to before the field existed; an
    /// older reader keeps it verbatim as an unknown key.
    ///
    /// [Organization — Labels]: https://docs/design/organization/#labels
    pub regions: Regions,
    /// Culling flag: LWW register over the trinary [`CullFlag`]. Wire-absent
    /// default = never flagged (`Neutral`).
    pub cull: Lww<CullFlag>,
    /// Hidden flag: LWW register; hidden assets are excluded from default views
    /// (owner: Asset Organization — Hidden Assets). Wire-absent default = visible.
    pub hidden: Lww<bool>,
    /// Camera identifier (export-stripped).
    pub camera_id: Option<CameraId>,
    /// Importing device id (UUIDv4; export-stripped).
    pub device_id: Uuid,
    /// Importing session id (UUIDv7; export-stripped).
    pub session_id: Uuid,
    /// Geolocation **as imported** (export-rounded). A user's location edit never overwrites
    /// it; it lands in [`gps_user`](Self::gps_user), and
    /// [`effective_gps`](Self::effective_gps) is the location the asset is placed at.
    pub gps: Option<Gps>,
    /// The user's location edit: an LWW register over `Option<Gps>`, written by
    /// `Workspace::set_location` with [`GpsSource::Manual`]. A stamped `None` is an explicit
    /// "no location", which hides an as-imported fix without erasing it. Wire-absent when never
    /// written (export-rounded like `gps`).
    pub gps_user: Lww<Option<Gps>>,
    /// The **prior** provenance head — the record *preceding* the write that seals this sidecar,
    /// always equal to that write's manifest `prior_provenance_hash` (SSoT:
    /// [Metadata — Provenance Binding and Sealing Order]). Referencing the prior head (not the
    /// sealing write's own record) keeps the binding well-founded — the manifest commits to this
    /// sidecar's `metadata_blob_hash`, so a sidecar referencing that manifest would be a cycle.
    /// Wire-absent (`None`) exactly on the initial `create`.
    ///
    /// [Metadata — Provenance Binding and Sealing Order]: https://docs/design/metadata/#provenance-binding-and-sealing-order
    pub provenance_chain_hash: Option<Hash32>,
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
        if self.capture_timestamp_user != Lww::default() {
            put!("capture_timestamp_user", self.capture_timestamp_user);
        }
        put!("import_timestamp", self.import_timestamp);
        put!("content_type", self.content_type);
        put_opt!("dimensions", self.dimensions);
        put_opt!("lqip", self.lqip);
        put!("tags_user", self.tags_user);
        put!("tags_ai", self.tags_ai);
        put!("caption", self.caption);
        put!("rating", self.rating);
        // Wire-absent default: a never-written register is omitted so pre-existing
        // sidecars (and stackless assets) encode byte-identically.
        if self.stack_membership != Lww::default() {
            put!("stack_membership", self.stack_membership);
        }
        if !self.regions.is_empty() {
            put!("regions", self.regions);
        }
        if self.cull != Lww::default() {
            put!("cull", self.cull);
        }
        if self.hidden != Lww::default() {
            put!("hidden", self.hidden);
        }
        put_opt!("camera_id", self.camera_id);
        put!("device_id", self.device_id);
        put!("session_id", self.session_id);
        put_opt!("gps", self.gps);
        if self.gps_user != Lww::default() {
            put!("gps_user", self.gps_user);
        }
        // Wire-absent on the initial create (no prior head); present on every later write.
        put_opt!("provenance_chain_hash", self.provenance_chain_hash);

        // Merge preserved unknown fields (canonical encode re-sorts everything).
        for (k, v) in &self.unknown {
            m.push((Value::Text(k.clone()), v.clone()));
        }
        if include_signature && let Some(sig) = &self.signature {
            m.push((Value::Text("signature".to_string()), to_value(sig)));
        }
        m
    }

    /// The capture instant the asset is dated by: the user's correction when one was ever
    /// written, else the as-imported [`capture_timestamp`](Self::capture_timestamp).
    pub fn effective_capture_timestamp(&self) -> &str {
        self.capture_timestamp_user
            .get()
            .map_or(self.capture_timestamp.as_str(), String::as_str)
    }

    /// The location the asset is placed at: the user's edit when one was ever written (a
    /// stamped `None` meaning "no location"), else the as-imported [`gps`](Self::gps).
    pub fn effective_gps(&self) -> Option<&Gps> {
        match self.gps_user.get() {
            Some(user) => user.as_ref(),
            None => self.gps.as_ref(),
        }
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

    /// The highest `add_id.counter` `device` has written into any `AddId`-bearing OR-set of this
    /// sidecar — user tags, AI tags, and every region's labels, live or deleted — or `None` if it
    /// wrote none. The floor a restarting device reseeds its counter from.
    pub fn max_add_counter_for(&self, device: &Uuid) -> Option<u64> {
        let regions = self.regions.values().flat_map(|register| {
            register
                .current
                .iter()
                .chain(register.superseded.iter())
                .filter_map(|stamped| stamped.value.as_ref())
                .filter_map(|region| region.labels.max_add_counter_for(device))
        });
        [
            self.tags_user.max_add_counter_for(device),
            self.tags_ai.max_add_counter_for(device),
        ]
        .into_iter()
        .flatten()
        .chain(regions)
        .max()
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
        let capture_timestamp_user = match text.remove("capture_timestamp_user") {
            None | Some(Value::Null) => Lww::new(),
            Some(v) => from_value::<Lww<String>>(v)?,
        };
        let import_timestamp = req!("import_timestamp", String);
        let content_type = req!("content_type", String);
        let dimensions = opt!("dimensions", Dimensions);
        let lqip = opt!("lqip", Lqip);
        let tags_user = req!("tags_user", OrSet<String>);
        let tags_ai = req!("tags_ai", OrSet<AiTag>);
        let caption = req!("caption", Lww<String>);
        let rating = req!("rating", Lww<u8>);
        let stack_membership = match text.remove("stack_membership") {
            None | Some(Value::Null) => Lww::new(),
            Some(v) => from_value::<Lww<Option<StackMembership>>>(v)?,
        };
        let regions = match text.remove("regions") {
            None | Some(Value::Null) => Regions::new(),
            Some(v) => from_value::<Regions>(v)?,
        };
        let cull = match text.remove("cull") {
            None | Some(Value::Null) => Lww::new(),
            Some(v) => from_value::<Lww<CullFlag>>(v)?,
        };
        let hidden = match text.remove("hidden") {
            None | Some(Value::Null) => Lww::new(),
            Some(v) => from_value::<Lww<bool>>(v)?,
        };
        let camera_id = opt!("camera_id", CameraId);
        let device_id = req!("device_id", Uuid);
        let session_id = req!("session_id", Uuid);
        let gps = opt!("gps", Gps);
        let gps_user = match text.remove("gps_user") {
            None | Some(Value::Null) => Lww::new(),
            Some(v) => from_value::<Lww<Option<Gps>>>(v)?,
        };
        let provenance_chain_hash = opt!("provenance_chain_hash", Hash32);
        let signature = opt!("signature", HybridSignature);

        Ok(SidecarV1 {
            sidecar_schema,
            crypto_suite_id,
            uuid,
            hash,
            capture_timestamp,
            capture_timestamp_user,
            import_timestamp,
            content_type,
            dimensions,
            lqip,
            tags_user,
            tags_ai,
            caption,
            rating,
            stack_membership,
            regions,
            cull,
            hidden,
            camera_id,
            device_id,
            session_id,
            gps,
            gps_user,
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
            capture_timestamp_user: Lww::new(),
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
            stack_membership: Lww::new(),
            regions: Regions::new(),
            cull: Lww::new(),
            hidden: Lww::new(),
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
                datum: GpsDatum::Wgs84,
            }),
            gps_user: Lww::new(),
            provenance_chain_hash: Some(Hash32([0xCC; 32])),
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

    /// The generated LQIP (chromahash + version + dominant_color) lands in the sidecar,
    /// is covered by the signature, and survives a canonical round-trip byte-identically.
    #[test]
    fn lqip_lands_in_signed_sidecar_and_round_trips() {
        let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let mut s = minimal();
        s.lqip = Some(Lqip {
            chromahash: vec![1, 2, 3, 4, 5, 6, 7, 8],
            format_version: 1,
            dominant_color: [10, 20, 30],
        });
        s.sign(&ik);
        assert!(s.verify(&ik.verifying_key()));

        let bytes = s.to_canonical_vec();
        let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
        assert_eq!(back.lqip, s.lqip);
        assert!(back.verify(&ik.verifying_key()));

        // Dropping the LQIP after signing invalidates the signature (it was covered).
        let mut stripped = back.clone();
        stripped.lqip = None;
        assert!(!stripped.verify(&ik.verifying_key()));
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

    #[test]
    fn create_sidecar_provenance_chain_hash_is_wire_absent() {
        // A create sidecar references no prior head (sealing order step 2, `H = None`), so
        // `provenance_chain_hash` encodes as an absent map key and round-trips back to `None`.
        let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let mut s = minimal();
        s.provenance_chain_hash = None;
        s.sign(&ik);
        let bytes = s.to_canonical_vec();
        let needle = b"provenance_chain_hash";
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "an absent prior head must not appear on the wire"
        );
        let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
        assert_eq!(back.provenance_chain_hash, None);
        assert_eq!(back, s);
        assert!(back.verify(&ik.verifying_key()));
        // A post-create sidecar (minimal carries `Some`) keeps the key present.
        assert!(
            minimal()
                .to_canonical_vec()
                .windows(needle.len())
                .any(|w| w == needle),
            "a present prior head must appear as a byte string on the wire"
        );
    }

    #[test]
    fn absent_stack_membership_is_wire_absent_and_byte_stable() {
        // A never-written register is omitted from the wire, so pre-existing
        // sidecars (and stackless assets) encode byte-identically to before the
        // Lww wrap.
        let s = minimal();
        let bytes = s.to_canonical_vec();
        let needle = b"stack_membership";
        assert!(!bytes.windows(needle.len()).any(|w| w == needle));
        let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
        assert_eq!(back.stack_membership, Lww::new());
    }

    #[test]
    fn stack_membership_round_trips_and_survives_signing() {
        let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let mut s = minimal();
        s.stack_membership.set(
            Some(StackMembership {
                stack_id: Uuid::from_u128(0x51),
                stack_type: StackType::Burst,
                role: StackRole::Primary,
                member_index: Some(0),
            }),
            "2026-05-31T12:00:00Z",
            Uuid::from_u128(0xD1),
        );
        s.sign(&ik);
        let back =
            SidecarV1::from_canonical_slice(&s.to_canonical_vec(), SIDECAR_SCHEMA_V1).unwrap();
        assert_eq!(back, s);
        assert!(back.verify(&ik.verifying_key()));
    }

    #[test]
    fn concurrent_stack_edits_converge_order_independently() {
        // Replica A joins a stack at 10:00; replica B removes membership at
        // 11:00. Merging in either order — or replaying either op again —
        // converges on the removal: grouping is idempotent and
        // order-independent by construction (metadata doc, Convergence table).
        let membership = Some(StackMembership {
            stack_id: Uuid::from_u128(0x51),
            stack_type: StackType::Burst,
            role: StackRole::Member,
            member_index: Some(3),
        });
        let mut a: Lww<Option<StackMembership>> = Lww::new();
        a.set(
            membership.clone(),
            "2026-05-31T10:00:00Z",
            Uuid::from_u128(0xA),
        );
        let mut b: Lww<Option<StackMembership>> = Lww::new();
        b.set(None, "2026-05-31T11:00:00Z", Uuid::from_u128(0xB));

        let mut ab = a.clone();
        ab.merge(&b);
        let mut ba = b.clone();
        ba.merge(&a);
        assert_eq!(ab.get(), ba.get());
        assert_eq!(ab.get(), Some(&None)); // the removal wins

        // Idempotent: replaying the stale join changes nothing.
        let mut replay = ab.clone();
        replay.merge(&a);
        assert_eq!(replay.get(), ab.get());
    }

    fn with_region() -> (SidecarV1, Uuid) {
        use crate::metadata::crdt::AddId;
        use crate::sidecar::region::{Region, RegionShape, write_region};

        let mut s = minimal();
        let region_id = Uuid::from_u128(0x7E61);
        let mut region = Region::new(RegionShape::Circle {
            cx: 0.5,
            cy: 0.25,
            r: 0.125,
        });
        region.labels.add(
            "Alice".into(),
            AddId {
                device: Uuid::from_u128(0xD1),
                counter: 41,
            },
        );
        let mut register = Lww::new();
        write_region(
            &mut register,
            Some(region),
            "2026-05-31T12:00:00Z".into(),
            Uuid::from_u128(0xD1),
        );
        s.regions.insert(region_id, register);
        (s, region_id)
    }

    #[test]
    fn absent_regions_are_wire_absent_and_byte_stable() {
        let bytes = minimal().to_canonical_vec();
        let needle = b"regions";
        assert!(!bytes.windows(needle.len()).any(|w| w == needle));
        let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
        assert!(back.regions.is_empty());
    }

    #[test]
    fn regions_round_trip_and_survive_signing() {
        let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let (mut s, region_id) = with_region();
        s.sign(&ik);
        let back =
            SidecarV1::from_canonical_slice(&s.to_canonical_vec(), SIDECAR_SCHEMA_V1).unwrap();
        assert_eq!(back, s);
        assert!(back.verify(&ik.verifying_key()));
        assert!(back.regions.contains_key(&region_id));
        // Dropping the regions after signing invalidates the signature (they were covered).
        let mut stripped = back;
        stripped.regions.clear();
        assert!(!stripped.verify(&ik.verifying_key()));
    }

    /// A client that predates `regions` decodes it into `_unknown`, re-signs after an edit of
    /// its own, and hands back a sidecar a current client reads the regions from intact.
    #[test]
    fn an_old_client_preserves_regions_as_an_unknown_key_through_a_re_sign() {
        let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let (mut current, _) = with_region();
        current.sign(&ik);
        let bytes = current.to_canonical_vec();

        // What a pre-`regions` decoder holds: the same record with `regions` as an unknown key.
        let mut old = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
        let raw = to_value(&old.regions);
        old.regions.clear();
        old.unknown.insert("regions".into(), raw);
        assert_eq!(
            old.to_canonical_vec(),
            bytes,
            "an unknown-key regions entry encodes byte-identically"
        );
        assert!(old.verify(&ik.verifying_key()));

        // The old client edits a field it knows and re-signs.
        old.caption.set(
            "seen by an old client".into(),
            "2026-06-01T00:00:00Z",
            Uuid::from_u128(0xD2),
        );
        old.sign(&ik);

        let back =
            SidecarV1::from_canonical_slice(&old.to_canonical_vec(), SIDECAR_SCHEMA_V1).unwrap();
        assert!(back.verify(&ik.verifying_key()));
        assert_eq!(back.regions, current.regions);
        assert!(back.unknown.is_empty());
        assert_eq!(
            back.caption.get().map(String::as_str),
            Some("seen by an old client")
        );
    }

    #[test]
    fn max_add_counter_spans_region_labels() {
        let (s, _) = with_region();
        assert_eq!(s.max_add_counter_for(&Uuid::from_u128(0xD1)), Some(41));
        assert_eq!(s.max_add_counter_for(&Uuid::from_u128(0xD9)), None);
    }

    #[test]
    fn absent_cull_and_hidden_are_wire_absent_and_byte_stable() {
        // Never-flagged assets omit both registers, so pre-existing sidecars
        // encode byte-identically to before the fields existed.
        let s = minimal();
        let bytes = s.to_canonical_vec();
        for needle in [b"cull".as_slice(), b"hidden".as_slice()] {
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle),
                "unexpected wire presence"
            );
        }
        let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
        assert_eq!(back.cull, Lww::new());
        assert_eq!(back.hidden, Lww::new());
    }

    #[test]
    fn cull_and_hidden_round_trip_and_survive_signing() {
        let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let mut s = minimal();
        s.cull.set(
            CullFlag::Reject,
            "2026-05-31T12:00:00Z",
            Uuid::from_u128(0xD1),
        );
        s.hidden
            .set(true, "2026-05-31T12:00:01Z", Uuid::from_u128(0xD1));
        s.sign(&ik);
        let back =
            SidecarV1::from_canonical_slice(&s.to_canonical_vec(), SIDECAR_SCHEMA_V1).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.cull.get(), Some(&CullFlag::Reject));
        assert_eq!(back.hidden.get(), Some(&true));
        assert!(back.verify(&ik.verifying_key()));
    }

    #[test]
    fn concurrent_cull_flags_converge() {
        // Two devices flag the same asset concurrently: pick at 10:00, reject at
        // 11:00 — both merge orders converge on the later reject.
        let mut a: Lww<CullFlag> = Lww::new();
        a.set(CullFlag::Pick, "2026-05-31T10:00:00Z", Uuid::from_u128(0xA));
        let mut b: Lww<CullFlag> = Lww::new();
        b.set(
            CullFlag::Reject,
            "2026-05-31T11:00:00Z",
            Uuid::from_u128(0xB),
        );
        let mut ab = a.clone();
        ab.merge(&b);
        let mut ba = b.clone();
        ba.merge(&a);
        assert_eq!(ab.get(), ba.get());
        assert_eq!(ab.get(), Some(&CullFlag::Reject));
    }

    /// A sidecar with no user correction carries neither register on the wire, so every sidecar
    /// written before the registers existed encodes byte-identically, and the effective values
    /// are the as-imported ones.
    #[test]
    fn absent_user_capture_and_location_are_wire_absent_and_byte_stable() {
        let s = minimal();
        let bytes = s.to_canonical_vec();
        for needle in [b"capture_timestamp_user".as_slice(), b"gps_user".as_slice()] {
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle),
                "unexpected wire presence"
            );
        }
        let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
        assert_eq!(back.capture_timestamp_user, Lww::new());
        assert_eq!(back.gps_user, Lww::new());
        assert_eq!(back.effective_capture_timestamp(), "2026-05-31T10:00:00Z");
        assert_eq!(back.effective_gps(), s.gps.as_ref());
    }

    /// Both registers round-trip under signature beside the as-imported values they override,
    /// and a stamped `None` location hides the import's fix without erasing it.
    #[test]
    fn user_capture_and_location_round_trip_beside_the_imported_values() {
        let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let mut s = minimal();
        s.capture_timestamp_user.set(
            "2019-03-04T05:06:07Z".to_string(),
            "2026-06-01T00:00:00Z",
            Uuid::from_u128(0xD1),
        );
        let manual = Gps {
            lat: 51.5,
            lon: -0.12,
            source: GpsSource::Manual,
            datum: GpsDatum::Wgs84,
        };
        s.gps_user.set(
            Some(manual.clone()),
            "2026-06-01T00:00:00Z",
            Uuid::from_u128(0xD1),
        );
        s.sign(&ik);

        let back =
            SidecarV1::from_canonical_slice(&s.to_canonical_vec(), SIDECAR_SCHEMA_V1).unwrap();
        assert_eq!(back, s);
        assert!(back.verify(&ik.verifying_key()));
        assert_eq!(back.capture_timestamp, "2026-05-31T10:00:00Z");
        assert_eq!(back.effective_capture_timestamp(), "2019-03-04T05:06:07Z");
        assert_eq!(back.gps.as_ref().map(|g| g.source), Some(GpsSource::Exif));
        assert_eq!(back.effective_gps(), Some(&manual));

        // Dropping a correction after signing invalidates the signature (it was covered).
        let mut stripped = back.clone();
        stripped.gps_user = Lww::new();
        assert!(!stripped.verify(&ik.verifying_key()));

        // A stamped `None` is "no location" — distinct from never having edited it.
        let mut cleared = back;
        cleared
            .gps_user
            .set(None, "2026-06-02T00:00:00Z", Uuid::from_u128(0xD1));
        assert_eq!(cleared.effective_gps(), None);
        assert!(cleared.gps.is_some(), "the import's fix is kept");
    }

    /// Two devices correct the same asset's capture time and location concurrently: merging the
    /// registers in either order — or replaying either side — converges on the later write, and
    /// the as-imported values are untouched by any of it.
    #[test]
    fn concurrent_capture_and_location_edits_converge_by_lww() {
        let base = minimal();
        let manual = |lat: f64| {
            Some(Gps {
                lat,
                lon: 0.0,
                source: GpsSource::Manual,
                datum: GpsDatum::Wgs84,
            })
        };
        let mut a = base.clone();
        a.capture_timestamp_user.set(
            "2019-01-01T00:00:00Z".to_string(),
            "2026-06-01T10:00:00Z",
            Uuid::from_u128(0xA),
        );
        a.gps_user
            .set(manual(1.0), "2026-06-01T12:00:00Z", Uuid::from_u128(0xA));
        let mut b = base.clone();
        b.capture_timestamp_user.set(
            "2020-01-01T00:00:00Z".to_string(),
            "2026-06-01T11:00:00Z",
            Uuid::from_u128(0xB),
        );
        // Same instant as A's location write: the larger device id breaks the tie.
        b.gps_user
            .set(manual(2.0), "2026-06-01T12:00:00Z", Uuid::from_u128(0xB));

        let merge = |into: &mut SidecarV1, from: &SidecarV1| {
            into.capture_timestamp_user
                .merge(&from.capture_timestamp_user);
            into.gps_user.merge(&from.gps_user);
        };
        let mut ab = a.clone();
        merge(&mut ab, &b);
        let mut ba = b.clone();
        merge(&mut ba, &a);
        for merged in [&ab, &ba] {
            assert_eq!(merged.effective_capture_timestamp(), "2020-01-01T00:00:00Z");
            assert_eq!(merged.effective_gps(), manual(2.0).as_ref());
            assert_eq!(merged.capture_timestamp, base.capture_timestamp);
            assert_eq!(merged.gps, base.gps);
        }
        assert_eq!(ab.to_canonical_vec(), ba.to_canonical_vec());

        // Replaying the losing side does not change the outcome.
        let mut replay = ab.clone();
        merge(&mut replay, &a);
        assert_eq!(
            replay.effective_capture_timestamp(),
            ab.effective_capture_timestamp()
        );
        assert_eq!(replay.effective_gps(), ab.effective_gps());
    }

    /// Datum-verbatim storage (WGS-84 arm): a `Wgs84` fix omits the `datum` key, so
    /// the encoded `gps` value is byte-identical to the pre-`datum` known-answer shape
    /// (`{lat, lon, source}`), and the whole sidecar carries no `datum` byte on the wire.
    #[test]
    fn wgs84_datum_is_wire_absent_and_byte_identical() {
        // The pre-`datum` known-answer wire shape of a fix: exactly `{lat, lon, source}`.
        #[derive(Serialize)]
        struct LegacyGps {
            lat: f64,
            lon: f64,
            source: GpsSource,
        }
        let legacy = LegacyGps {
            lat: 40.7128,
            lon: -74.0060,
            source: GpsSource::Exif,
        };
        let current = Gps {
            lat: 40.7128,
            lon: -74.0060,
            source: GpsSource::Exif,
            datum: GpsDatum::Wgs84,
        };
        let mut legacy_bytes = Vec::new();
        ciborium::ser::into_writer(&legacy, &mut legacy_bytes).unwrap();
        let mut current_bytes = Vec::new();
        ciborium::ser::into_writer(&current, &mut current_bytes).unwrap();
        assert_eq!(
            legacy_bytes, current_bytes,
            "a wgs84 datum must be wire-absent → byte-identical to the pre-datum vector"
        );

        // Whole-sidecar: the `datum` key never appears, and the fix decodes back to wgs84.
        let s = minimal();
        let bytes = s.to_canonical_vec();
        let needle = b"datum";
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "an absent (wgs84) datum must not appear on the wire"
        );
        let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.gps.as_ref().unwrap().datum, GpsDatum::Wgs84);
    }

    /// Datum-verbatim storage (GCJ-02 arm): a populated-`datum` vector. A `gcj02` fix
    /// round-trips **unconverted** (lat/lon byte-for-byte unchanged), carries the `datum`
    /// key on the wire, and survives signing.
    #[test]
    fn gcj02_datum_round_trips_unconverted_and_survives_signing() {
        let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let mut s = minimal();
        // A user-entered coordinate that arrived already in China's GCJ-02 datum.
        s.gps = Some(Gps {
            lat: 39.90869,
            lon: 116.39745,
            source: GpsSource::Manual,
            datum: GpsDatum::Gcj02,
        });
        s.sign(&ik);
        assert!(s.verify(&ik.verifying_key()));

        let bytes = s.to_canonical_vec();
        // Populated datum is present on the wire.
        let needle = b"datum";
        assert!(
            bytes.windows(needle.len()).any(|w| w == needle),
            "a gcj02 datum must appear on the wire"
        );
        let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
        assert_eq!(back, s);
        let gps = back.gps.as_ref().unwrap();
        // Stored verbatim — never converted at rest.
        assert_eq!(gps.datum, GpsDatum::Gcj02);
        assert_eq!(gps.lat.to_bits(), 39.90869_f64.to_bits());
        assert_eq!(gps.lon.to_bits(), 116.39745_f64.to_bits());
        assert!(back.verify(&ik.verifying_key()));
    }

    /// Datum-verbatim storage (BD-09 arm): BD-09 is **never a storable datum**. A
    /// BD-09 input is folded at the edge and enters the sidecar as `datum = gcj02`, within
    /// the documented sub-metre bound and deterministically — the same input yields a
    /// byte-identical sidecar every time.
    ///
    /// The known-answer pair is the GCJ-02 anchor from the arm above pushed through the
    /// closed-form *forward* GCJ-02 → BD-09 transform; folding it must land back on the
    /// anchor.
    #[test]
    fn bd09_input_is_folded_to_gcj02_within_bound_and_deterministically() {
        use crate::domain::{BD09_FOLD_BOUND_METRES, Bd09Coord, fold_bd09_to_gcj02};

        /// The GCJ-02 point the BD-09 input below is the forward image of.
        const GCJ02_TRUTH: (f64, f64) = (39.90869, 116.39745);
        const BD09_INPUT: Bd09Coord = Bd09Coord {
            lat: 39.915_033_233_576_175,
            lon: 116.403_823_024_761_45,
        };
        /// Metres per degree of latitude, for expressing the residual as a distance.
        const METRES_PER_DEGREE: f64 = 111_320.0;

        let (lat, lon) = fold_bd09_to_gcj02(BD09_INPUT).unwrap();

        // Within the documented sub-metre bound of the true GCJ-02 point.
        let d_lat = (lat - GCJ02_TRUTH.0) * METRES_PER_DEGREE;
        let d_lon = (lon - GCJ02_TRUTH.1) * METRES_PER_DEGREE * lat.to_radians().cos();
        let error_metres = d_lat.hypot(d_lon);
        assert!(
            error_metres < BD09_FOLD_BOUND_METRES,
            "BD-09 fold must land within {BD09_FOLD_BOUND_METRES} m, got {error_metres} m"
        );

        // BD-09 never reaches the wire: what is stored is the folded GCJ-02 pair.
        let stored = |lat: f64, lon: f64| {
            let mut s = minimal();
            s.gps = Some(Gps {
                lat,
                lon,
                source: GpsSource::Manual,
                datum: GpsDatum::Gcj02,
            });
            s
        };
        let s = stored(lat, lon);
        let bytes = s.to_canonical_vec();
        let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
        let gps = back.gps.as_ref().unwrap();
        assert_eq!(
            gps.datum,
            GpsDatum::Gcj02,
            "a folded BD-09 input stores as gcj02"
        );
        assert_ne!(
            gps.lat.to_bits(),
            BD09_INPUT.lat.to_bits(),
            "BD-09 must never be stored verbatim"
        );
        assert_ne!(
            gps.lon.to_bits(),
            BD09_INPUT.lon.to_bits(),
            "BD-09 must never be stored verbatim"
        );

        // Deterministic: an independent fold of the same input encodes byte-identically.
        let (again_lat, again_lon) = fold_bd09_to_gcj02(BD09_INPUT).unwrap();
        assert_eq!(
            bytes,
            stored(again_lat, again_lon).to_canonical_vec(),
            "same BD-09 input must yield a byte-identical sidecar"
        );
    }
}
