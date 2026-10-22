//! The region and GPS rows of [Metadata — Privacy on Export]: what leaves the library when a
//! sidecar crosses a trust boundary.
//!
//! Region labels can name people, so `regions` is **stripped by default** and kept only when
//! the user opts in for that one export. Every GPS coordinate the sidecar carries — the
//! as-imported `gps` and every value in the user's `gps_user` register, superseded ones
//! included — is **rounded to 2 decimal places** (≈1 km) unless the user opts in to full
//! precision. Each opt-in is a per-export value, never a sticky setting. Stripping produces a
//! copy; the library's own sidecar is untouched. The remaining rows of the privacy table
//! (device and session ids, camera serial) are owned by the share-link strip, which builds on
//! this options value.
//!
//! [Metadata — Privacy on Export]: https://docs/design/metadata/#privacy-on-export

use super::{Gps, SidecarV1};

/// The per-export privacy choices. [`Default`] is the strict choice: everything stripped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExportPrivacy {
    /// Keep image regions and their labels in the exported copy.
    pub retain_regions: bool,
    /// Keep GPS coordinates at full precision instead of rounding them to 2 decimal places.
    pub retain_full_gps: bool,
}

/// Decimal places an exported coordinate keeps by default (≈1 km).
const EXPORT_GPS_DECIMALS: i32 = 2;

/// Round one coordinate to [`EXPORT_GPS_DECIMALS`] places.
fn round_coordinate(value: f64) -> f64 {
    let scale = 10f64.powi(EXPORT_GPS_DECIMALS);
    (value * scale).round() / scale
}

/// Round `gps` in place; `true` when either coordinate changed.
fn round_fix(gps: &mut Gps) -> bool {
    let (lat, lon) = (round_coordinate(gps.lat), round_coordinate(gps.lon));
    let changed = lat.to_bits() != gps.lat.to_bits() || lon.to_bits() != gps.lon.to_bits();
    gps.lat = lat;
    gps.lon = lon;
    changed
}

/// The copy of `sidecar` that may cross a trust boundary under `privacy`.
///
/// When anything is stripped the copy is returned **unsigned**: the library signature covered
/// the removed fields, so the exporting client re-signs the copy it hands out. When nothing is
/// stripped the copy keeps its signature.
pub fn sidecar_for_export(sidecar: &SidecarV1, privacy: ExportPrivacy) -> SidecarV1 {
    let mut out = sidecar.clone();
    let mut stripped = false;
    if !privacy.retain_regions && !out.regions.is_empty() {
        tracing::debug!(
            asset_id = %out.uuid,
            regions = out.regions.len(),
            "export: image regions stripped"
        );
        out.regions.clear();
        stripped = true;
    }
    if !privacy.retain_full_gps {
        let user = out
            .gps_user
            .current
            .iter_mut()
            .chain(&mut out.gps_user.superseded);
        let mut rounded = 0usize;
        for fix in out
            .gps
            .iter_mut()
            .chain(user.filter_map(|stamped| stamped.value.as_mut()))
        {
            if round_fix(fix) {
                rounded += 1;
            }
        }
        if rounded > 0 {
            tracing::debug!(asset_id = %out.uuid, rounded, "export: GPS coordinates rounded");
            stripped = true;
        }
    }
    if stripped {
        out.signature = None;
    }
    out
}

#[cfg(test)]
mod tests;
