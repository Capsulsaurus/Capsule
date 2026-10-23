//! Label, region, capture-time and location edits on a local library: one [`Edit`] value, one
//! [`apply`] that writes it, and one [`EditReport`] saying what the library holds afterwards.
//!
//! `capsule label`, `capsule region` and `capsule edit` build an [`Edit`] from their arguments;
//! the TUI's label editor and metadata repair screens build the same value from their forms and
//! hand it to the same [`apply`] through their port. So a TUI edit and its scriptable twin are
//! one code path, and the report the CLI prints as `--json` is the one the TUI summarises.
//!
//! Every edit goes through the workspace's own signed writers, and none of them can lose a
//! value:
//!
//! - An asset label or region edit is one signed `metadata-update` on the asset's chain. A
//!   stack or album label edit re-signs that stack's or album's label record.
//! - A capture-time or location edit writes the **user** register beside the as-imported
//!   value, which is never overwritten ([`Workspace::set_capture_timestamp`],
//!   [`Workspace::set_location`]): what the original said stays readable after any correction.
//! - Removing a label removes every add of it this device has observed; an add another device
//!   made concurrently survives the merge (OR-set add-wins).
//! - Deleting a region is final once devices merge (a tombstone absorbs every live write).
//!
//! The parsers ([`parse_circle`], [`parse_rect`], [`parse_coordinate`]) refuse a non-finite
//! number, so every [`Edit`] they build compares equal to itself.

use capsule_core::domain::GpsDatum;
use capsule_core::sidecar::RegionShape;
use capsule_library::lifecycle::{LifecycleError, Workspace};
use jiff::Timestamp;
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::show::{Fix, gps_datum_name, gps_source_name};

/// What a label edit applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelTarget {
    /// An asset's user labels (its `tags_user`).
    Asset(Uuid),
    /// A region of an asset.
    Region {
        /// The asset the region is drawn on.
        asset_id: Uuid,
        /// The region.
        region_id: Uuid,
    },
    /// A stack.
    Stack(Uuid),
    /// An album.
    Album(Uuid),
}

/// One edit to a local library.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// Add a label to the target.
    AddLabel {
        /// What is labelled.
        target: LabelTarget,
        /// The label, as typed; the workspace applies the label rule.
        label: String,
    },
    /// Remove a label from the target.
    RemoveLabel {
        /// What is unlabelled.
        target: LabelTarget,
        /// The label, as typed.
        label: String,
    },
    /// Mark a region on an asset, with its first labels.
    AddRegion {
        /// The asset.
        asset_id: Uuid,
        /// The outline, normalized to `[0, 1]` over the display orientation.
        shape: RegionShape,
        /// Labels to give it at once; may be empty.
        labels: Vec<String>,
    },
    /// Delete a region. Final.
    DeleteRegion {
        /// The asset.
        asset_id: Uuid,
        /// The region.
        region_id: Uuid,
    },
    /// Record the user's capture time.
    SetCaptureTime {
        /// The asset.
        asset_id: Uuid,
        /// The corrected instant.
        capture: Timestamp,
    },
    /// Record the user's location: a manual fix in `datum`, stored verbatim.
    SetLocation {
        /// The asset.
        asset_id: Uuid,
        /// Latitude, in `datum`.
        lat: f64,
        /// Longitude, in `datum`.
        lon: f64,
        /// The datum the coordinates are in.
        datum: GpsDatum,
    },
    /// Record that the asset has no location, as the user's edit.
    ClearLocation {
        /// The asset.
        asset_id: Uuid,
    },
}

// Every float an `Edit` carries comes through a parser that refuses NaN (`parse_coordinate`,
// `parse_circle`, `parse_rect`), so equality is reflexive on every value built here.
impl Eq for Edit {}

impl Edit {
    /// The edit's name as the report spells it.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::AddLabel { .. } => "label-add",
            Self::RemoveLabel { .. } => "label-remove",
            Self::AddRegion { .. } => "region-add",
            Self::DeleteRegion { .. } => "region-delete",
            Self::SetCaptureTime { .. } => "capture-time",
            Self::SetLocation { .. } => "location",
            Self::ClearLocation { .. } => "location-clear",
        }
    }
}

/// Why an edit was not written.
#[derive(Debug, Error)]
pub enum EditError {
    /// The asset selector did not resolve to exactly one asset.
    #[error(transparent)]
    Selector(#[from] crate::show::ShowError),
    /// The workspace refused the edit (an unknown id, a label the label rule refuses, a shape
    /// or coordinate out of range, a read-only album), and wrote nothing.
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),
}

/// A human-readable reason for an [`EditError`], for the failure line and the TUI status.
#[must_use]
pub fn describe_error(error: &EditError) -> String {
    match error {
        EditError::Selector(error) => crate::show::describe_error(error),
        EditError::Lifecycle(LifecycleError::InvalidLabel(reason)) => {
            format!("That label is refused ({reason}): a label is 1 to 256 bytes once trimmed.")
        }
        EditError::Lifecycle(LifecycleError::InvalidRegion(reason)) => format!(
            "That region is refused ({reason}): every coordinate is a fraction of the image in \
             [0, 1], and a rectangle stays inside the image."
        ),
        EditError::Lifecycle(LifecycleError::InvalidLocation { lat, lon }) => format!(
            "That location is refused: latitude {lat} must be in [-90, 90] and longitude {lon} \
             in [-180, 180]."
        ),
        EditError::Lifecycle(LifecycleError::NotFound(what)) => {
            format!("This library has no {what}.")
        }
        EditError::Lifecycle(error) => error.to_string(),
    }
}

/// A location fix as the report prints it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FixReport {
    /// Latitude, in `datum`.
    pub lat: f64,
    /// Longitude, in `datum`.
    pub lon: f64,
    /// `WGS-84` or `GCJ-02`.
    pub datum: &'static str,
    /// Where the fix came from: `EXIF`, `manual` or `derived`.
    pub source: &'static str,
}

impl From<Fix> for FixReport {
    fn from(fix: Fix) -> Self {
        Self {
            lat: fix.lat,
            lon: fix.lon,
            datum: gps_datum_name(fix.datum),
            source: gps_source_name(fix.source),
        }
    }
}

/// What one edit did, and what its target holds afterwards. `--json` prints it as one
/// document; a field that does not apply to the edit is left out.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EditReport {
    /// The edit ([`Edit::name`]).
    pub edit: &'static str,
    /// The asset edited, for an asset, region, capture-time or location edit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<Uuid>,
    /// The region edited or created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region_id: Option<Uuid>,
    /// The stack edited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_id: Option<Uuid>,
    /// The album edited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_id: Option<Uuid>,
    /// Whether anything was written: false only for a removal of a label the target does not
    /// carry.
    pub changed: bool,
    /// The target's labels afterwards, sorted, for a label or region edit (empty once a region
    /// is deleted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    /// The effective capture instant afterwards, for a capture-time edit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_timestamp: Option<String>,
    /// The as-imported capture instant, which a correction never overwrites.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_timestamp_imported: Option<String>,
    /// The effective location afterwards for a location edit: `null` when the user recorded
    /// none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Option<FixReport>>,
    /// The as-imported location for a location edit, which an edit never overwrites: `null`
    /// when the original carried none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location_imported: Option<Option<FixReport>>,
}

impl EditReport {
    fn new(edit: &Edit) -> Self {
        Self {
            edit: edit.name(),
            asset_id: None,
            region_id: None,
            stack_id: None,
            album_id: None,
            changed: true,
            labels: None,
            capture_timestamp: None,
            capture_timestamp_imported: None,
            location: None,
            location_imported: None,
        }
    }

    fn target(mut self, target: LabelTarget) -> Self {
        match target {
            LabelTarget::Asset(asset_id) => self.asset_id = Some(asset_id),
            LabelTarget::Region {
                asset_id,
                region_id,
            } => {
                self.asset_id = Some(asset_id);
                self.region_id = Some(region_id);
            }
            LabelTarget::Stack(stack_id) => self.stack_id = Some(stack_id),
            LabelTarget::Album(album_id) => self.album_id = Some(album_id),
        }
        self
    }
}

/// The target's current labels, sorted.
///
/// # Errors
///
/// The workspace has no such target, or its label record does not verify.
pub fn labels(ws: &Workspace, target: LabelTarget) -> Result<Vec<String>, EditError> {
    let not_found = |what: String| EditError::Lifecycle(LifecycleError::NotFound(what));
    Ok(match target {
        LabelTarget::Asset(asset_id) => ws
            .asset(&asset_id)
            .ok_or_else(|| not_found(format!("asset {asset_id}")))?
            .sidecar
            .tags_user
            .value()
            .into_iter()
            .collect(),
        LabelTarget::Region {
            asset_id,
            region_id,
        } => ws
            .regions(&asset_id)
            .into_iter()
            .find(|(id, _)| *id == region_id)
            .ok_or_else(|| not_found(format!("region {region_id} of {asset_id}")))?
            .1
            .labels
            .value()
            .into_iter()
            .collect(),
        LabelTarget::Stack(stack_id) => ws.stack_labels(stack_id)?.into_iter().collect(),
        LabelTarget::Album(album_id) => ws.album_labels(&album_id)?.into_iter().collect(),
    })
}

/// Write `edit` to `ws` and report what its target holds afterwards.
///
/// # Errors
///
/// The workspace refused the edit; nothing was written.
#[tracing::instrument(level = "debug", skip(ws), fields(edit = edit.name()))]
pub fn apply(ws: &mut Workspace, edit: &Edit) -> Result<EditReport, EditError> {
    let report = EditReport::new(edit);
    let report = match edit {
        Edit::AddLabel { target, label } => {
            match *target {
                LabelTarget::Asset(asset_id) => ws.tag_add(&asset_id, label)?,
                LabelTarget::Region {
                    asset_id,
                    region_id,
                } => ws.add_region_label(&asset_id, &region_id, label)?,
                LabelTarget::Stack(stack_id) => ws.add_stack_label(stack_id, label)?,
                LabelTarget::Album(album_id) => ws.add_album_label(album_id, label)?,
            }
            EditReport {
                labels: Some(labels(ws, *target)?),
                ..report.target(*target)
            }
        }
        Edit::RemoveLabel { target, label } => {
            let removed = match *target {
                LabelTarget::Asset(asset_id) => ws.tag_remove(&asset_id, label)?,
                LabelTarget::Region {
                    asset_id,
                    region_id,
                } => ws.remove_region_label(&asset_id, &region_id, label)?,
                LabelTarget::Stack(stack_id) => ws.remove_stack_label(stack_id, label)?,
                LabelTarget::Album(album_id) => ws.remove_album_label(album_id, label)?,
            };
            EditReport {
                changed: removed > 0,
                labels: Some(labels(ws, *target)?),
                ..report.target(*target)
            }
        }
        Edit::AddRegion {
            asset_id,
            shape,
            labels: initial,
        } => {
            // Check every label before the region is written, so a refused label leaves no
            // unlabelled region behind.
            for label in initial {
                capsule_core::sidecar::normalize_label(label).map_err(LifecycleError::from)?;
            }
            let region_id = ws.add_region(asset_id, *shape)?;
            for label in initial {
                ws.add_region_label(asset_id, &region_id, label)?;
            }
            let target = LabelTarget::Region {
                asset_id: *asset_id,
                region_id,
            };
            EditReport {
                labels: Some(labels(ws, target)?),
                ..report.target(target)
            }
        }
        Edit::DeleteRegion {
            asset_id,
            region_id,
        } => {
            ws.delete_region(asset_id, region_id)?;
            EditReport {
                labels: Some(Vec::new()),
                ..report.target(LabelTarget::Region {
                    asset_id: *asset_id,
                    region_id: *region_id,
                })
            }
        }
        Edit::SetCaptureTime { asset_id, capture } => {
            ws.set_capture_timestamp(asset_id, *capture)?;
            capture_report(ws, *asset_id, report)
        }
        Edit::SetLocation {
            asset_id,
            lat,
            lon,
            datum,
        } => {
            ws.set_location(asset_id, *lat, *lon, *datum)?;
            location_report(ws, *asset_id, report)
        }
        Edit::ClearLocation { asset_id } => {
            ws.clear_location(asset_id)?;
            location_report(ws, *asset_id, report)
        }
    };
    tracing::info!(
        edit = report.edit,
        changed = report.changed,
        asset_id = ?report.asset_id,
        region_id = ?report.region_id,
        stack_id = ?report.stack_id,
        album_id = ?report.album_id,
        "library edit applied"
    );
    Ok(report)
}

fn capture_report(ws: &Workspace, asset_id: Uuid, report: EditReport) -> EditReport {
    let view = crate::show::collect(ws, &asset_id);
    EditReport {
        asset_id: Some(asset_id),
        capture_timestamp: view.as_ref().map(|v| v.capture_timestamp.clone()),
        capture_timestamp_imported: view.and_then(|v| v.capture_timestamp_imported),
        ..report
    }
}

fn location_report(ws: &Workspace, asset_id: Uuid, report: EditReport) -> EditReport {
    let view = crate::show::collect(ws, &asset_id);
    EditReport {
        asset_id: Some(asset_id),
        location: Some(view.as_ref().and_then(|v| v.gps).map(FixReport::from)),
        location_imported: Some(view.and_then(|v| v.gps_imported).map(FixReport::from)),
        ..report
    }
}

/// The report as one line a person reads: what changed and what the target holds now.
#[must_use]
pub fn summary(report: &EditReport) -> String {
    let list = |labels: &[String]| {
        if labels.is_empty() {
            "(none)".to_owned()
        } else {
            labels.join(", ")
        }
    };
    let labels = report.labels.as_deref().map(list).unwrap_or_default();
    let fix = |fix: &Option<FixReport>| {
        fix.as_ref().map_or_else(
            || "no location".to_owned(),
            |f| format!("{:.6}, {:.6} ({}, {})", f.lat, f.lon, f.datum, f.source),
        )
    };
    match report.edit {
        "label-add" => format!("Label added. Labels now: {labels}."),
        "label-remove" if report.changed => format!("Label removed. Labels now: {labels}."),
        "label-remove" => format!("It did not carry that label. Labels: {labels}."),
        "region-add" => format!(
            "Region {} added. Its labels: {labels}.",
            report.region_id.map(short).unwrap_or_default()
        ),
        "region-delete" => format!(
            "Region {} deleted.",
            report.region_id.map(short).unwrap_or_default()
        ),
        "capture-time" => format!(
            "Capture time now {}; the original's {} is kept.",
            report.capture_timestamp.as_deref().unwrap_or("(unset)"),
            report
                .capture_timestamp_imported
                .as_deref()
                .unwrap_or("(unset)")
        ),
        _ => format!(
            "Location now {}; the original's {} is kept.",
            report.location.as_ref().map(fix).unwrap_or_default(),
            report
                .location_imported
                .as_ref()
                .map(fix)
                .unwrap_or_default()
        ),
    }
}

/// Short form of an id: its last eight hex digits (a UUIDv7 leads with its creation time).
fn short(id: Uuid) -> String {
    let simple = id.simple().to_string();
    simple[simple.len() - 8..].to_owned()
}

/// Parse a finite number.
///
/// # Errors
///
/// The text is not a number, or is infinite or NaN.
pub fn parse_coordinate(text: &str) -> Result<f64, String> {
    let value: f64 = text
        .trim()
        .parse()
        .map_err(|_| format!("{:?} is not a number", text.trim()))?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(format!("{:?} is not a finite number", text.trim()))
    }
}

/// `N` comma-separated finite numbers, spelled as `form` says.
fn fractions<const N: usize>(text: &str, form: &str) -> Result<[f32; N], String> {
    let parts: Vec<&str> = text.split(',').map(str::trim).collect();
    if parts.len() != N {
        return Err(format!("expected {form}, {N} numbers separated by commas"));
    }
    let mut out = [0.0_f32; N];
    for (slot, part) in out.iter_mut().zip(parts) {
        let value: f32 = part
            .parse()
            .map_err(|_| format!("{part:?} is not a number; expected {form}"))?;
        if !value.is_finite() {
            return Err(format!("{part:?} is not a finite number"));
        }
        *slot = value;
    }
    Ok(out)
}

/// Parse `CX,CY,R` into a circle, each a fraction of the image. The range itself is the
/// workspace's to check.
///
/// # Errors
///
/// Not three finite numbers.
pub fn parse_circle(text: &str) -> Result<RegionShape, String> {
    let [cx, cy, r] = fractions::<3>(text, "CX,CY,R")?;
    Ok(RegionShape::Circle { cx, cy, r })
}

/// Parse `X,Y,W,H` into a rectangle, each a fraction of the image.
///
/// # Errors
///
/// Not four finite numbers.
pub fn parse_rect(text: &str) -> Result<RegionShape, String> {
    let [x, y, w, h] = fractions::<4>(text, "X,Y,W,H")?;
    Ok(RegionShape::Rect { x, y, w, h })
}

/// Parse a datum name: `wgs84`/`WGS-84` or `gcj02`/`GCJ-02`, in any case.
///
/// # Errors
///
/// Neither datum.
pub fn parse_datum(text: &str) -> Result<GpsDatum, String> {
    let key: String = text
        .trim()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    match key.as_str() {
        "wgs84" => Ok(GpsDatum::Wgs84),
        "gcj02" => Ok(GpsDatum::Gcj02),
        _ => Err(format!(
            "{:?} is not a datum; use WGS-84 or GCJ-02",
            text.trim()
        )),
    }
}

#[cfg(test)]
mod tests;
