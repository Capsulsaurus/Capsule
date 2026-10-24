//! [Metadata — Privacy on Export]: what leaves the library when a sidecar crosses a trust
//! boundary.
//!
//! [`sidecar_for_export`] applies every row of the privacy table to a copy of the sidecar:
//!
//! - **Camera serial**: the serial of `camera_id` is emptied; the model stays.
//! - **Device identifier**: `device_id` becomes the nil UUID, and so does every other place a
//!   device id travels: the `by` stamp of every last-writer-wins register (superseded entries
//!   included) and the add-ids of every OR-set, which are re-issued under the nil device. An
//!   OR-set keeps only its live elements, so no tombstone carries a device id either.
//! - **Session id**: `session_id` becomes the nil UUID.
//! - **Edit history**: the `superseded` log of every last-writer-wins register (prior
//!   captions, ratings, capture times, cull and hidden flags, stack memberships, locations and
//!   region versions) is emptied; only each register's current value leaves. A caption the
//!   user rewrote to remove a name must not reach the recipient in its old form.
//! - **GPS**: the as-imported `gps` and every value in the user's `gps_user` register,
//!   superseded ones included when the history is kept, is rounded to 2 decimal places (≈1 km).
//! - **Personal contact tags and image regions**: a face is matched to a person only as a
//!   labelled region, so `regions` is removed whole, and that one strip covers both rows.
//!
//! Each row is kept only when the user opts in for that one export through [`ExportPrivacy`],
//! never through a sticky setting. Keys this reader does not know (`unknown`) are always
//! dropped: a field a newer schema added may itself be a boundary-crossing field, and a copy
//! that cannot tell must not carry it.
//!
//! Stripping produces a copy; the library's own sidecar is untouched.
//!
//! [Metadata — Privacy on Export]: https://docs/design/metadata/#privacy-on-export

use uuid::Uuid;

use super::{Gps, SidecarV1};
use crate::metadata::crdt::{AddId, Lww, OrSet};

/// The per-export privacy choices, one per row of the privacy table. [`Default`] is the strict
/// choice: everything stripped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExportPrivacy {
    /// Keep image regions and their labels, the personal contact tags among them.
    pub retain_regions: bool,
    /// Keep GPS coordinates at full precision instead of rounding them to 2 decimal places.
    pub retain_full_gps: bool,
    /// Keep the camera serial number.
    pub retain_camera_serial: bool,
    /// Keep the importing device id and the device of every register stamp and OR-set add.
    pub retain_device_ids: bool,
    /// Keep the import session id.
    pub retain_session_id: bool,
    /// Keep the superseded log of every last-writer-wins register: the values an edit
    /// displaced.
    pub retain_superseded: bool,
}

impl ExportPrivacy {
    /// The opt-in that keeps every row: a copy that differs from the library's only in the
    /// keys this reader does not know.
    pub const RETAIN_ALL: Self = Self {
        retain_regions: true,
        retain_full_gps: true,
        retain_camera_serial: true,
        retain_device_ids: true,
        retain_session_id: true,
        retain_superseded: true,
    };
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

/// Replace the writing device of every stamp in `register` with the nil UUID; `true` when one
/// changed.
fn anonymize_register<T: Clone + PartialEq>(register: &mut Lww<T>) -> bool {
    let mut changed = false;
    for stamped in register.current.iter_mut().chain(&mut register.superseded) {
        if !stamped.by.is_nil() {
            stamped.by = Uuid::nil();
            changed = true;
        }
    }
    changed
}

/// Re-issue the live elements of `set` under the nil device, dropping its tombstones; `true`
/// when the copy differs from `set`.
fn anonymize_or_set<T: Ord + Clone>(set: &mut OrSet<T>) -> bool {
    let mut out = OrSet::new();
    for (counter, (_, element)) in (0u64..).zip(set.entries()) {
        out.add(
            element,
            AddId {
                device: Uuid::nil(),
                counter,
            },
        );
    }
    let changed = out != *set;
    *set = out;
    changed
}

/// Empty the superseded log of `register`; the number of entries removed.
fn clear_history<T: Clone + PartialEq>(register: &mut Lww<T>) -> usize {
    let removed = register.superseded.len();
    register.superseded.clear();
    removed
}

/// Empty the superseded log of every register `out` carries; the number of entries removed.
fn clear_every_history(out: &mut SidecarV1) -> usize {
    let mut removed = clear_history(&mut out.capture_timestamp_user);
    removed += clear_history(&mut out.caption);
    removed += clear_history(&mut out.rating);
    removed += clear_history(&mut out.stack_membership);
    removed += clear_history(&mut out.cull);
    removed += clear_history(&mut out.hidden);
    removed += clear_history(&mut out.gps_user);
    for register in out.regions.values_mut() {
        removed += clear_history(register);
    }
    removed
}

/// Strip every device id `out` carries; `true` when one was present.
fn strip_device_ids(out: &mut SidecarV1) -> bool {
    let mut changed = !out.device_id.is_nil();
    out.device_id = Uuid::nil();
    changed |= anonymize_register(&mut out.capture_timestamp_user);
    changed |= anonymize_register(&mut out.caption);
    changed |= anonymize_register(&mut out.rating);
    changed |= anonymize_register(&mut out.stack_membership);
    changed |= anonymize_register(&mut out.cull);
    changed |= anonymize_register(&mut out.hidden);
    changed |= anonymize_register(&mut out.gps_user);
    changed |= anonymize_or_set(&mut out.tags_user);
    changed |= anonymize_or_set(&mut out.tags_ai);
    for register in out.regions.values_mut() {
        changed |= anonymize_register(register);
        let stamps = register.current.iter_mut().chain(&mut register.superseded);
        for region in stamps.filter_map(|stamped| stamped.value.as_mut()) {
            changed |= anonymize_or_set(&mut region.labels);
        }
    }
    changed
}

/// Round every coordinate `out` carries; the number of fixes that changed.
fn round_every_fix(out: &mut SidecarV1) -> usize {
    let user = out
        .gps_user
        .current
        .iter_mut()
        .chain(&mut out.gps_user.superseded);
    out.gps
        .iter_mut()
        .chain(user.filter_map(|stamped| stamped.value.as_mut()))
        .map(round_fix)
        .filter(|changed| *changed)
        .count()
}

/// The copy of `sidecar` that may cross a trust boundary under `privacy`.
///
/// When anything is stripped the copy is returned **unsigned**: the library signature covered
/// the removed fields, so it no longer verifies. When nothing is stripped the copy keeps its
/// signature. Under the [`Default`] choice the device and session ids are always stripped, so
/// a strict copy is always unsigned.
#[tracing::instrument(level = "debug", skip_all, fields(asset_id = %sidecar.uuid))]
pub fn sidecar_for_export(sidecar: &SidecarV1, privacy: ExportPrivacy) -> SidecarV1 {
    let mut out = sidecar.clone();
    let mut stripped = false;
    if !out.unknown.is_empty() {
        tracing::debug!(
            keys = out.unknown.len(),
            "export: unknown sidecar keys dropped"
        );
        out.unknown.clear();
        stripped = true;
    }
    if !privacy.retain_regions && !out.regions.is_empty() {
        tracing::debug!(
            regions = out.regions.len(),
            "export: image regions stripped"
        );
        out.regions.clear();
        stripped = true;
    }
    if !privacy.retain_superseded {
        let removed = clear_every_history(&mut out);
        if removed > 0 {
            tracing::debug!(removed, "export: superseded register values stripped");
            stripped = true;
        }
    }
    if !privacy.retain_full_gps {
        let rounded = round_every_fix(&mut out);
        if rounded > 0 {
            tracing::debug!(rounded, "export: GPS coordinates rounded");
            stripped = true;
        }
    }
    if !privacy.retain_camera_serial
        && let Some(camera) = out.camera_id.as_mut()
        && !camera.serial.is_empty()
    {
        tracing::debug!("export: camera serial stripped");
        camera.serial.clear();
        stripped = true;
    }
    if !privacy.retain_device_ids && strip_device_ids(&mut out) {
        tracing::debug!("export: device ids stripped");
        stripped = true;
    }
    if !privacy.retain_session_id && !out.session_id.is_nil() {
        tracing::debug!("export: session id stripped");
        out.session_id = Uuid::nil();
        stripped = true;
    }
    if stripped {
        out.signature = None;
    }
    tracing::trace!(stripped, "export: sidecar copy prepared");
    out
}

#[cfg(test)]
mod tests;
