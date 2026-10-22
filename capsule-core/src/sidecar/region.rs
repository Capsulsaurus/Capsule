//! Labelled image regions — the sidecar's `regions` map (SSoT: [Organization — Labels] and
//! [Metadata — Grouping Convergence]).
//!
//! A region is a circle or rectangle in coordinates normalized to `[0, 1]` over the original's
//! **display orientation** (after EXIF orientation), so it survives every derivative size. Each
//! region has a UUIDv7 and its own labels OR-set. On the wire the map is
//! `Map<UUIDv7, Lww<Option<Region>>>`: a stamped `Some` is a live region, a stamped `None` is a
//! deleted one.
//!
//! # Merge
//!
//! A region register does not merge as a plain [`Lww`]. Two concurrent label edits on one
//! region must both survive ("labels as tags"), and folding label sets into a last-writer-wins
//! value is not associative once a delete sits between them. The register therefore merges as a
//! join-semilattice:
//!
//! - **Delete is final.** A tombstone absorbs every live write, whatever its stamp; between two
//!   tombstones the later stamp wins. A region id is minted fresh per create, so no create ever
//!   races an earlier delete of the same id — the only write a tombstone overrides is an edit
//!   concurrent with or after the delete.
//! - **Live ⊔ live** takes the shape of the later `(ts, device_id)` stamp (ties broken on the
//!   shape's canonical bytes) and the **union** of the two label OR-sets.
//!
//! Each component is a semilattice, so [`merge_regions`] is commutative, associative and
//! idempotent. A merged register keeps no superseded log: every displaced label survives in the
//! union, and a displaced shape is not a value a user authored alone.
//!
//! [Organization — Labels]: https://docs/design/organization/#labels
//! [Metadata — Grouping Convergence]: https://docs/design/metadata/#grouping-convergence-requirement

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::cbor;
use crate::metadata::crdt::{Lww, OrSet, Stamped};

/// The sidecar's region map: region id (UUIDv7) to its register.
pub type Regions = BTreeMap<Uuid, Lww<Option<Region>>>;

/// A region's outline, in coordinates normalized to `[0, 1]` over the display orientation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegionShape {
    /// A circle centred at `(cx, cy)` with radius `r` (fractions of width / height).
    Circle {
        /// Centre, horizontal.
        cx: f32,
        /// Centre, vertical.
        cy: f32,
        /// Radius.
        r: f32,
    },
    /// A rectangle with top-left `(x, y)`, width `w` and height `h`.
    Rect {
        /// Left edge.
        x: f32,
        /// Top edge.
        y: f32,
        /// Width.
        w: f32,
        /// Height.
        h: f32,
    },
}

/// Why a region shape was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RegionError {
    /// A coordinate is not a finite number in `[0, 1]`.
    #[error("region coordinate out of range [0, 1]")]
    OutOfRange,
    /// A radius, width or height is zero.
    #[error("region has no area")]
    Empty,
    /// A rectangle extends past the right or bottom edge.
    #[error("region extends past the image edge")]
    PastEdge,
}

impl RegionShape {
    /// Check the normalized-coordinate contract: every value finite and in `[0, 1]`, a positive
    /// extent, and a rectangle wholly inside the image.
    ///
    /// # Errors
    ///
    /// The [`RegionError`] naming the first violated rule.
    pub fn validate(&self) -> Result<(), RegionError> {
        let unit = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
        match *self {
            Self::Circle { cx, cy, r } => {
                if ![cx, cy, r].into_iter().all(unit) {
                    return Err(RegionError::OutOfRange);
                }
                if r <= 0.0 {
                    return Err(RegionError::Empty);
                }
            }
            Self::Rect { x, y, w, h } => {
                if ![x, y, w, h].into_iter().all(unit) {
                    return Err(RegionError::OutOfRange);
                }
                if w <= 0.0 || h <= 0.0 {
                    return Err(RegionError::Empty);
                }
                if x + w > 1.0 || y + h > 1.0 {
                    return Err(RegionError::PastEdge);
                }
            }
        }
        Ok(())
    }

    fn canonical_bytes(&self) -> Vec<u8> {
        cbor::to_canonical_vec(self).expect("a region shape serializes")
    }
}

/// One labelled region of an image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Region {
    /// The outline.
    pub shape: RegionShape,
    /// The region's labels.
    pub labels: OrSet<String>,
}

impl Region {
    /// A region with no labels yet.
    pub fn new(shape: RegionShape) -> Self {
        Self {
            shape,
            labels: OrSet::new(),
        }
    }
}

/// The total order on stamps: parsed time, then the writing device, then the raw string (so an
/// unparseable time still orders deterministically, below every parseable one).
fn stamp_key<T>(s: &Stamped<T>) -> (Option<jiff::Timestamp>, Uuid, &str) {
    (s.ts.parse().ok(), s.by, s.ts.as_str())
}

fn later<'a, T>(a: &'a Stamped<T>, b: &'a Stamped<T>) -> Ordering {
    stamp_key(a).cmp(&stamp_key(b))
}

/// Merge two writes to one region register into their join (see the module docs).
fn join(a: &Stamped<Option<Region>>, b: &Stamped<Option<Region>>) -> Stamped<Option<Region>> {
    match (&a.value, &b.value) {
        // A tombstone absorbs a live write.
        (None, Some(_)) => a.clone(),
        (Some(_), None) => b.clone(),
        // Two tombstones: the later stamp.
        (None, None) => {
            if later(a, b) == Ordering::Less {
                b.clone()
            } else {
                a.clone()
            }
        }
        (Some(ra), Some(rb)) => {
            let order = later(a, b)
                .then_with(|| ra.shape.canonical_bytes().cmp(&rb.shape.canonical_bytes()));
            let (winner, winner_region) = if order == Ordering::Less {
                (b, rb)
            } else {
                (a, ra)
            };
            let mut labels = ra.labels.clone();
            labels.merge(&rb.labels);
            Stamped {
                value: Some(Region {
                    shape: winner_region.shape,
                    labels,
                }),
                ts: winner.ts.clone(),
                by: winner.by,
            }
        }
    }
}

/// Merge two registers for one region id. A never-written register is the identity.
pub fn merge_region_register(
    a: &Lww<Option<Region>>,
    b: &Lww<Option<Region>>,
) -> Lww<Option<Region>> {
    let current = match (&a.current, &b.current) {
        (None, None) => None,
        (Some(x), None) | (None, Some(x)) => Some(x.clone()),
        (Some(x), Some(y)) => Some(join(x, y)),
    };
    Lww {
        current,
        superseded: Vec::new(),
    }
}

/// Merge `other`'s region map into `local`: the union of region ids, each register merged by
/// [`merge_region_register`]. Commutative, associative and idempotent.
pub fn merge_regions(local: &mut Regions, other: &Regions) {
    for (id, theirs) in other {
        let merged = match local.get(id) {
            Some(ours) => merge_region_register(ours, theirs),
            None => merge_region_register(&Lww::new(), theirs),
        };
        local.insert(*id, merged);
    }
}

/// Write `value` into a region register as a new local write: the register's current value
/// becomes `value` stamped `(ts, by)`, and nothing is logged as superseded — a local edit
/// already folded the prior state into `value`.
pub fn write_region(
    register: &mut Lww<Option<Region>>,
    value: Option<Region>,
    ts: String,
    by: Uuid,
) {
    register.current = Some(Stamped { value, ts, by });
    register.superseded.clear();
}

/// The live region a register holds, if it is neither unwritten nor deleted.
pub fn live_region(register: &Lww<Option<Region>>) -> Option<&Region> {
    register.get().and_then(Option::as_ref)
}

#[cfg(test)]
mod tests;
