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
mod tests;
