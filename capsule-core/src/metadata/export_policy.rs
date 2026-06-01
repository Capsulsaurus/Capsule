//! Privacy on export (SSoT: [Metadata — Privacy on Export]).
//!
//! Several sidecar fields are fingerprinting surface if they leave the user's trust
//! boundary unredacted (a camera serial links every photo to one device; precise GPS
//! reveals a home address). When an asset crosses a boundary (share link, external backup
//! handed off, federated peer), Capsule strips these by default and retains them only on
//! explicit, per-export opt-in. The **local** sidecar is never modified.
//!
//! [Metadata — Privacy on Export]: https://docs/design/metadata/#privacy-on-export

use crate::sidecar::sidecar_v1::SidecarV1;

/// Per-export opt-ins. Defaults strip everything (the safe default).
#[derive(Debug, Clone, Copy, Default)]
pub struct ExportOptions {
    /// Retain the camera serial number.
    pub retain_camera_serial: bool,
    /// Retain the importing device id.
    pub retain_device_id: bool,
    /// Retain the importing session id.
    pub retain_session_id: bool,
    /// Retain full-precision GPS (otherwise rounded to ~1 km).
    pub retain_full_gps: bool,
}

/// Round a coordinate to 2 decimal places (~1 km), matching the export default.
fn round_2dp(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Produce an export copy of `sidecar` with fingerprinting fields stripped per `opts`. The
/// returned sidecar is **unsigned** (the caller re-signs for the export context); the input
/// is left untouched.
pub fn strip_for_export(sidecar: &SidecarV1, opts: &ExportOptions) -> SidecarV1 {
    let mut out = sidecar.clone();
    out.signature = None;

    if !opts.retain_camera_serial
        && let Some(cam) = out.camera_id.as_mut()
    {
        // Keep the model (not identifying); drop the per-device serial.
        cam.serial.clear();
    }
    if !opts.retain_device_id {
        out.device_id = uuid::Uuid::nil();
    }
    if !opts.retain_session_id {
        out.session_id = uuid::Uuid::nil();
    }
    if !opts.retain_full_gps
        && let Some(gps) = out.gps.as_mut()
    {
        gps.lat = round_2dp(gps.lat);
        gps.lon = round_2dp(gps.lon);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hash::Hash32;
    use crate::sidecar::sidecar_v1::{CameraId, Gps, GpsSource, SIDECAR_SCHEMA_V1};
    use std::collections::BTreeMap;
    use uuid::Uuid;

    fn sidecar() -> SidecarV1 {
        SidecarV1 {
            sidecar_schema: SIDECAR_SCHEMA_V1,
            crypto_suite_id: crate::crypto::CRYPTO_SUITE_ID,
            uuid: Uuid::from_u128(1),
            hash: Hash32([0; 32]),
            capture_timestamp: "2026-05-31T10:00:00Z".into(),
            import_timestamp: "2026-05-31T11:00:00Z".into(),
            content_type: "image/jpeg".into(),
            dimensions: None,
            lqip: None,
            tags_user: Default::default(),
            tags_ai: Default::default(),
            caption: Default::default(),
            rating: Default::default(),
            stack_membership: None,
            camera_id: Some(CameraId {
                model: "iPhone 15 Pro".into(),
                serial: "SECRET-SERIAL".into(),
            }),
            device_id: Uuid::from_u128(0xD1),
            session_id: Uuid::from_u128(0x5E),
            gps: Some(Gps {
                lat: 40.712812,
                lon: -74.006015,
                source: GpsSource::Exif,
            }),
            provenance_chain_hash: Hash32([0; 32]),
            unknown: BTreeMap::new(),
            signature: None,
        }
    }

    #[test]
    fn default_strips_all_fingerprinting_fields() {
        let s = sidecar();
        let e = strip_for_export(&s, &ExportOptions::default());
        assert_eq!(e.camera_id.as_ref().unwrap().serial, "");
        assert_eq!(e.camera_id.as_ref().unwrap().model, "iPhone 15 Pro"); // model retained
        assert_eq!(e.device_id, Uuid::nil());
        assert_eq!(e.session_id, Uuid::nil());
        let gps = e.gps.unwrap();
        assert_eq!(gps.lat, 40.71); // rounded to 2dp
        assert_eq!(gps.lon, -74.01);

        // Local sidecar is untouched.
        assert_eq!(s.camera_id.as_ref().unwrap().serial, "SECRET-SERIAL");
        assert_eq!(s.device_id, Uuid::from_u128(0xD1));
        assert_eq!(s.gps.as_ref().unwrap().lat, 40.712812);
    }

    #[test]
    fn opt_in_retains_each_field() {
        let s = sidecar();
        let opts = ExportOptions {
            retain_camera_serial: true,
            retain_device_id: true,
            retain_session_id: true,
            retain_full_gps: true,
        };
        let e = strip_for_export(&s, &opts);
        assert_eq!(e.camera_id.as_ref().unwrap().serial, "SECRET-SERIAL");
        assert_eq!(e.device_id, Uuid::from_u128(0xD1));
        assert_eq!(e.session_id, Uuid::from_u128(0x5E));
        assert_eq!(e.gps.as_ref().unwrap().lat, 40.712812);
    }

    #[test]
    fn partial_opt_in() {
        let s = sidecar();
        let opts = ExportOptions {
            retain_full_gps: true,
            ..Default::default()
        };
        let e = strip_for_export(&s, &opts);
        // GPS retained, but device id still stripped.
        assert_eq!(e.gps.as_ref().unwrap().lat, 40.712812);
        assert_eq!(e.device_id, Uuid::nil());
    }
}
