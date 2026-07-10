//! The coordinate datum a GPS fix is stored in, plus the input-edge BD-09 fold seam
//! (SSoT: [Metadata — Geolocation] and [Metadata — Closed Enum Value Sets]).
//!
//! GPS is stored **verbatim in the datum the source supplied** — never converted at rest
//! (GCJ-02 → WGS-84 has no exact inverse, so converting on input would destroy the user's
//! ground truth). The value set is closed: adding a value requires a new, later-dated
//! `protocol_version`.
//!
//! **BD-09 is never a storable datum.** Baidu's BD-09 (a second obfuscation layer over
//! GCJ-02) exists only at the input edge: it is folded — exactly, closed-form — to GCJ-02
//! on entry and stored as `datum = gcj02`. That fold is the sole gated piece of slice
//! `S-A7`; see [`fold_bd09_to_gcj02`].
//!
//! [Metadata — Geolocation]: https://docs/design/metadata/#geolocation
//! [Metadata — Closed Enum Value Sets]: https://docs/design/metadata/#closed-enum-value-sets

use serde::{Deserialize, Serialize};

/// The coordinate datum a stored GPS fix is expressed in (closed enum per
/// `protocol_version`; the code mirror of the [Closed Enum Value Sets] catalog).
///
/// [`Wgs84`](GpsDatum::Wgs84) is the near-universal camera format and the **wire-absent
/// default** — a `gps` value with no `datum` key decodes as `Wgs84`, and a `Wgs84` fix
/// omits the key on encode, so every pre-`datum` sidecar and known-answer vector stays
/// byte-identical.
///
/// [Closed Enum Value Sets]: https://docs/design/metadata/#closed-enum-value-sets
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GpsDatum {
    /// The near-universal camera datum, and the wire-absent default.
    #[default]
    #[serde(rename = "wgs84")]
    Wgs84,
    /// China's legally mandated obfuscated datum — user-entered coordinates from Chinese
    /// maps arrive in it. Also the target datum a BD-09 input is folded to on entry.
    #[serde(rename = "gcj02")]
    Gcj02,
}

impl GpsDatum {
    /// Whether this is the wire-absent default ([`Wgs84`](GpsDatum::Wgs84)). Used as the
    /// sidecar `skip_serializing_if` predicate so a `Wgs84` fix omits the `datum` key.
    #[must_use]
    pub fn is_wgs84(&self) -> bool {
        matches!(self, GpsDatum::Wgs84)
    }
}

/// A raw BD-09 coordinate presented at the **input edge**. BD-09 is never a storable
/// [`GpsDatum`] (metadata doc, Closed Enum Value Sets): it must be folded to GCJ-02 by
/// [`fold_bd09_to_gcj02`] before it can enter a sidecar, and is stored as
/// [`GpsDatum::Gcj02`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bd09Coord {
    /// Latitude in the BD-09 datum.
    pub lat: f64,
    /// Longitude in the BD-09 datum.
    pub lon: f64,
}

/// Error folding a BD-09 input coordinate to GCJ-02 at the input edge.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DatumFoldError {
    /// The exact BD-09 → GCJ-02 transform is gated on the unpublished in-house
    /// `geocoordinates-rs` library (repo-root `SLICES.md`). Until it lands the fold is
    /// **refused** rather than approximated: BD-09 is not a storable datum, so a BD-09
    /// input can neither be stored verbatim nor converted, and must be rejected at the
    /// edge.
    #[error(
        "BD-09 → GCJ-02 fold is gated on the unpublished `geocoordinates-rs` library \
         (repo-root SLICES.md); BD-09 input cannot enter a sidecar until it lands"
    )]
    FoldGated,
}

/// Fold a BD-09 input coordinate to its exact GCJ-02 equivalent at the input edge,
/// returning the `(lat, lon)` to store under [`GpsDatum::Gcj02`].
///
/// **GATED SEAM.** The BD-09 → GCJ-02 transform is closed-form and exact, but its
/// implementation lives in the in-house `geocoordinates-rs` library, which is **not yet
/// published** (repo-root `SLICES.md`, "In-House and External Library Gates"). Until it
/// lands this returns [`DatumFoldError::FoldGated`].
///
/// This seam **MUST NEVER** apply an approximate conversion, and BD-09 **MUST NOT** be
/// stored verbatim — it is not a storable [`GpsDatum`]. When `geocoordinates-rs` lands,
/// this body becomes the exact fold and callers store the result as `datum = gcj02`; the
/// seam's signature and the callers do not change.
pub fn fold_bd09_to_gcj02(_input: Bd09Coord) -> Result<(f64, f64), DatumFoldError> {
    // Gated: no `geocoordinates-rs` yet ⇒ refuse. Never approximate, never store BD-09.
    Err(DatumFoldError::FoldGated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_values_are_the_catalog_strings() {
        assert_eq!(
            serde_json::to_string(&GpsDatum::Wgs84).unwrap(),
            "\"wgs84\""
        );
        assert_eq!(
            serde_json::to_string(&GpsDatum::Gcj02).unwrap(),
            "\"gcj02\""
        );
    }

    #[test]
    fn serde_round_trip() {
        for v in [GpsDatum::Wgs84, GpsDatum::Gcj02] {
            let json = serde_json::to_string(&v).unwrap();
            let back: GpsDatum = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn default_is_wgs84() {
        assert_eq!(GpsDatum::default(), GpsDatum::Wgs84);
        assert!(GpsDatum::default().is_wgs84());
        assert!(!GpsDatum::Gcj02.is_wgs84());
    }

    /// The gated seam: with `geocoordinates-rs` unpublished, a BD-09 input is **refused**
    /// (not approximated, not stored verbatim) — the pre-gate behaviour the metadata doc
    /// mandates ("BD-09 is never a storable datum ... folded ... exact"). When the library
    /// lands this test's expectation flips to the exact GCJ-02 fold.
    #[test]
    fn bd09_fold_is_gated_and_refused() {
        let input = Bd09Coord {
            lat: 39.915,
            lon: 116.404,
        };
        assert_eq!(fold_bd09_to_gcj02(input), Err(DatumFoldError::FoldGated));
    }
}
