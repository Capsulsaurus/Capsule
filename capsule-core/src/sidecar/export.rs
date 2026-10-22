//! The region half of [Metadata — Privacy on Export]: what leaves the library when a sidecar
//! crosses a trust boundary.
//!
//! Region labels can name people, so `regions` is **stripped by default** and kept only when
//! the user opts in for that one export. The opt-in is a per-export value, never a sticky
//! setting. Stripping produces a copy; the library's own sidecar is untouched. The remaining
//! rows of the privacy table (device and session ids, camera serial, GPS rounding) are owned by
//! the share-link strip, which builds on this options value.
//!
//! [Metadata — Privacy on Export]: https://docs/design/metadata/#privacy-on-export

use super::SidecarV1;

/// The per-export privacy choices. [`Default`] is the strict choice: everything stripped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExportPrivacy {
    /// Keep image regions and their labels in the exported copy.
    pub retain_regions: bool,
}

/// The copy of `sidecar` that may cross a trust boundary under `privacy`.
///
/// When anything is stripped the copy is returned **unsigned**: the library signature covered
/// the removed fields, so the exporting client re-signs the copy it hands out. When nothing is
/// stripped the copy keeps its signature.
pub fn sidecar_for_export(sidecar: &SidecarV1, privacy: ExportPrivacy) -> SidecarV1 {
    let mut out = sidecar.clone();
    if !privacy.retain_regions && !out.regions.is_empty() {
        tracing::debug!(
            asset_id = %out.uuid,
            regions = out.regions.len(),
            "export: image regions stripped"
        );
        out.regions.clear();
        out.signature = None;
    }
    out
}

#[cfg(test)]
mod tests;
