//! The coordinate datum a GPS fix is stored in, plus the input-edge BD-09 fold
//! (SSoT: [Metadata — Geolocation] and [Metadata — Closed Enum Value Sets]).
//!
//! GPS is stored **verbatim in the datum the source supplied** — never converted at rest
//! (GCJ-02 → WGS-84 has no exact inverse, so converting on input would destroy the user's
//! ground truth). The value set is closed: adding a value requires a new, later-dated
//! `protocol_version`.
//!
//! **BD-09 is never a storable datum.** Baidu's BD-09 (a second obfuscation layer over
//! GCJ-02) exists only at the input edge: it is folded to GCJ-02 on entry and stored as
//! `datum = gcj02`. Only the **forward** GCJ-02 → BD-09 direction is closed-form, so the
//! fold is the **error-bounded iterative inverse** of that forward transform, held to a
//! sub-metre bound (decision 2026-07-12, amending the earlier "closed-form and exact"
//! claim; implemented in-house per the 2026-08-21 gates decision — no crate is adopted).
//! See [`fold_bd09_to_gcj02`].
//!
//! [Metadata — Geolocation]: https://docs/design/metadata/#geolocation
//! [Metadata — Closed Enum Value Sets]: https://docs/design/metadata/#closed-enum-value-sets

use serde::{Deserialize, Serialize};
use tracing::{trace, warn};

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

/// The documented accuracy bound on the BD-09 → GCJ-02 fold, in metres: a successful
/// [`fold_bd09_to_gcj02`] is **sub-metre**, far below consumer-GPS noise, which is why the
/// bounded inverse is accepted rather than refusing BD-09 input at the edge.
///
/// This is the *contract* ceiling. The tolerance actually enforced
/// ([`BD09_FOLD_TOLERANCE_DEGREES`]) is roughly four orders of magnitude tighter, so the
/// bound holds with enormous margin.
pub const BD09_FOLD_BOUND_METRES: f64 = 1.0;

/// The residual the refinement must reach, in degrees, before a fold is accepted.
///
/// `1e-9°` is ≈ 0.11 mm of latitude — five orders of magnitude above the `f64` resolution
/// floor at geographic magnitudes, so it is always reachable, and four orders below
/// [`BD09_FOLD_BOUND_METRES`].
pub const BD09_FOLD_TOLERANCE_DEGREES: f64 = 1e-9;

/// Iteration cap for the refinement. The iteration contracts by ≈ 0.02 per step at
/// geographic magnitudes (≈ 1.7 decimal digits gained per step, from a ≈ 2e-5° seed error),
/// so it reaches [`BD09_FOLD_TOLERANCE_DEGREES`] in single-digit steps; the cap exists only
/// so a pathological input terminates instead of spinning.
const BD09_FOLD_MAX_REFINEMENTS: usize = 24;

/// Metres per degree of latitude on the WGS-84 mean meridian — used only to express the
/// fold's residual as a distance for the [`BD09_FOLD_BOUND_METRES`] contract.
const METRES_PER_DEGREE_LATITUDE: f64 = 111_320.0;

/// Baidu's magic constant: π scaled by 3000/180.
const BD09_X_PI: f64 = std::f64::consts::PI * 3000.0 / 180.0;
/// The BD-09 latitude offset applied by the forward transform.
const BD09_LAT_OFFSET: f64 = 0.006;
/// The BD-09 longitude offset applied by the forward transform.
const BD09_LON_OFFSET: f64 = 0.0065;
/// Amplitude of the forward transform's radial wobble.
const BD09_RADIAL_AMPLITUDE: f64 = 0.000_02;
/// Amplitude of the forward transform's angular wobble.
const BD09_ANGULAR_AMPLITUDE: f64 = 0.000_003;

/// Error folding a BD-09 input coordinate to GCJ-02 at the input edge.
///
/// BD-09 is not a storable [`GpsDatum`], so a fold that cannot be certified is a hard
/// refusal: the coordinate can neither be stored verbatim nor approximated past the bound.
#[derive(Debug, Clone, Copy, thiserror::Error, PartialEq)]
pub enum DatumFoldError {
    /// The input coordinate was not a finite number, so no fold is defined for it.
    #[error("BD-09 input coordinate is not finite (lat={lat}, lon={lon})")]
    NonFinite {
        /// The offending latitude.
        lat: f64,
        /// The offending longitude.
        lon: f64,
    },
    /// The refinement did not reach [`BD09_FOLD_TOLERANCE_DEGREES`] within the iteration
    /// cap, so the result cannot be certified inside [`BD09_FOLD_BOUND_METRES`]. The fold
    /// refuses rather than returning an uncertified approximation.
    #[error(
        "BD-09 → GCJ-02 refinement did not converge: residual {residual_degrees}° exceeds \
         the fold tolerance after {BD09_FOLD_MAX_REFINEMENTS} steps"
    )]
    DidNotConverge {
        /// The residual left when the iteration cap was hit, in degrees.
        residual_degrees: f64,
    },
}

/// The **closed-form** GCJ-02 → BD-09 transform: Baidu's published forward direction, and
/// the only direction of this pair that has a closed form.
///
/// [`fold_bd09_to_gcj02`] inverts *this* function numerically.
fn gcj02_to_bd09(lat: f64, lon: f64) -> (f64, f64) {
    let z = lon.hypot(lat) + BD09_RADIAL_AMPLITUDE * (lat * BD09_X_PI).sin();
    let theta = lat.atan2(lon) + BD09_ANGULAR_AMPLITUDE * (lon * BD09_X_PI).cos();
    (
        z * theta.sin() + BD09_LAT_OFFSET,
        z * theta.cos() + BD09_LON_OFFSET,
    )
}

/// Fold a BD-09 input coordinate to GCJ-02 at the input edge, returning the `(lat, lon)`
/// to store under [`GpsDatum::Gcj02`].
///
/// Only `gcj02_to_bd09` — the *forward* direction — is closed-form, so this is the
/// **error-bounded iterative inverse** of it: seed with the offset-removed coordinate, then
/// repeatedly push the estimate forward, measure the residual against the input, and
/// correct by it. The forward map is the identity plus a small perturbation, so the
/// iteration contracts sharply and reaches [`BD09_FOLD_TOLERANCE_DEGREES`] in a handful of
/// steps.
///
/// The result is **certified before it is returned**: the accepted estimate is pushed
/// forward one last time and must land on the input within tolerance, so a successful fold
/// is always inside [`BD09_FOLD_BOUND_METRES`]. It is **deterministic** — pure `f64`
/// arithmetic on a fixed schedule, so the same input yields bit-identical output on every
/// run.
///
/// BD-09 **MUST NOT** be stored verbatim — it is not a storable [`GpsDatum`]. Callers store
/// the result as `datum = gcj02`.
pub fn fold_bd09_to_gcj02(input: Bd09Coord) -> Result<(f64, f64), DatumFoldError> {
    if !input.lat.is_finite() || !input.lon.is_finite() {
        warn!(
            lat = input.lat,
            lon = input.lon,
            "refusing BD-09 fold: input coordinate is not finite"
        );
        return Err(DatumFoldError::NonFinite {
            lat: input.lat,
            lon: input.lon,
        });
    }

    // Seed: strip the forward transform's constant offsets. That leaves only its sinusoidal
    // wobble as error (~2e-5°), which the refinement below drives down to the tolerance.
    let mut lat = input.lat - BD09_LAT_OFFSET;
    let mut lon = input.lon - BD09_LON_OFFSET;

    for step in 0..BD09_FOLD_MAX_REFINEMENTS {
        let (probe_lat, probe_lon) = gcj02_to_bd09(lat, lon);
        let delta_lat = input.lat - probe_lat;
        let delta_lon = input.lon - probe_lon;
        lat += delta_lat;
        lon += delta_lon;
        let residual = delta_lat.abs().max(delta_lon.abs());
        trace!(
            step,
            residual_degrees = residual,
            "BD-09 fold refinement step"
        );
        if residual <= BD09_FOLD_TOLERANCE_DEGREES {
            break;
        }
    }

    // Certify the value we are about to return, not the one before the last correction.
    let (probe_lat, probe_lon) = gcj02_to_bd09(lat, lon);
    let residual_degrees = (input.lat - probe_lat)
        .abs()
        .max((input.lon - probe_lon).abs());
    if residual_degrees.is_nan() || residual_degrees > BD09_FOLD_TOLERANCE_DEGREES {
        warn!(
            lat = input.lat,
            lon = input.lon,
            residual_degrees,
            "refusing BD-09 fold: refinement did not reach the documented bound"
        );
        return Err(DatumFoldError::DidNotConverge { residual_degrees });
    }

    trace!(
        bd09_lat = input.lat,
        bd09_lon = input.lon,
        gcj02_lat = lat,
        gcj02_lon = lon,
        residual_metres = residual_degrees * METRES_PER_DEGREE_LATITUDE,
        "folded BD-09 input to GCJ-02"
    );
    Ok((lat, lon))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Separation in metres, adequate at the scale of the fold's residual (millimetres):
    /// latitude degrees are constant-length, longitude degrees shrink by `cos(lat)`.
    fn separation_metres(a: (f64, f64), b: (f64, f64)) -> f64 {
        let d_lat = (a.0 - b.0) * METRES_PER_DEGREE_LATITUDE;
        let d_lon = (a.1 - b.1) * METRES_PER_DEGREE_LATITUDE * a.0.to_radians().cos();
        d_lat.hypot(d_lon)
    }

    /// A deterministic sweep of GCJ-02 anchors spanning China's bounding box — the only
    /// region BD-09 is defined over — at 2° spacing.
    fn china_grid() -> Vec<(f64, f64)> {
        let mut points = Vec::new();
        let mut lat_step = 0;
        while lat_step <= 36 {
            let mut lon_step = 0;
            while lon_step <= 62 {
                points.push((18.0 + f64::from(lat_step), 73.0 + f64::from(lon_step)));
                lon_step += 2;
            }
            lat_step += 2;
        }
        points
    }

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

    /// The metadata doc's amended datum-verbatim-storage bullet, BD-09 arm: a BD-09 input
    /// folds to GCJ-02 **within the documented sub-metre bound**. Measured end to end —
    /// take a known GCJ-02 point, push it through the closed-form forward transform to get
    /// a genuine BD-09 coordinate, fold that back, and compare against the point we started
    /// from.
    #[test]
    fn bd09_fold_lands_within_the_documented_bound() {
        let mut worst_metres: f64 = 0.0;
        for truth in china_grid() {
            let (bd_lat, bd_lon) = gcj02_to_bd09(truth.0, truth.1);
            let folded = fold_bd09_to_gcj02(Bd09Coord {
                lat: bd_lat,
                lon: bd_lon,
            })
            .unwrap();
            worst_metres = worst_metres.max(separation_metres(folded, truth));
        }
        assert!(
            worst_metres < BD09_FOLD_BOUND_METRES,
            "worst-case fold error {worst_metres} m must stay under the documented \
             {BD09_FOLD_BOUND_METRES} m bound"
        );
        // The bound is a contract ceiling; the refinement is in fact sub-millimetric.
        assert!(
            worst_metres < 0.001,
            "refined inverse should be sub-millimetric, got {worst_metres} m"
        );
    }

    /// Same input, same output, every run — bit-for-bit, not merely within tolerance. The
    /// iteration has a fixed schedule and no state outside its arguments.
    #[test]
    fn bd09_fold_is_deterministic() {
        for truth in china_grid() {
            let (bd_lat, bd_lon) = gcj02_to_bd09(truth.0, truth.1);
            let input = Bd09Coord {
                lat: bd_lat,
                lon: bd_lon,
            };
            let first = fold_bd09_to_gcj02(input).unwrap();
            for _ in 0..8 {
                let again = fold_bd09_to_gcj02(input).unwrap();
                assert_eq!(
                    (first.0.to_bits(), first.1.to_bits()),
                    (again.0.to_bits(), again.1.to_bits()),
                    "fold must be bit-identical across repeated calls"
                );
            }
        }
    }

    /// The refinement is load-bearing: the naive one-shot inverse (evaluating the wobble at
    /// the BD-09 coordinate instead of the GCJ-02 one) is metres off, and the iteration is
    /// what buys the sub-metre bound. Guards against someone "simplifying" the loop away.
    #[test]
    fn refinement_beats_the_naive_one_shot_inverse() {
        let truth = (39.915, 116.404); // Tiananmen, GCJ-02.
        let (bd_lat, bd_lon) = gcj02_to_bd09(truth.0, truth.1);

        let x = bd_lon - BD09_LON_OFFSET;
        let y = bd_lat - BD09_LAT_OFFSET;
        let z = x.hypot(y) - BD09_RADIAL_AMPLITUDE * (y * BD09_X_PI).sin();
        let theta = y.atan2(x) - BD09_ANGULAR_AMPLITUDE * (x * BD09_X_PI).cos();
        let naive = (z * theta.sin(), z * theta.cos());

        let refined = fold_bd09_to_gcj02(Bd09Coord {
            lat: bd_lat,
            lon: bd_lon,
        })
        .unwrap();

        let naive_error = separation_metres(naive, truth);
        let refined_error = separation_metres(refined, truth);
        assert!(
            refined_error < naive_error,
            "refined inverse ({refined_error} m) must beat the naive one ({naive_error} m)"
        );
        assert!(
            refined_error < BD09_FOLD_BOUND_METRES,
            "refined inverse must be sub-metre, got {refined_error} m"
        );
    }

    /// A non-finite coordinate has no fold; it is refused at the edge rather than stored.
    #[test]
    fn non_finite_input_is_refused() {
        for input in [
            Bd09Coord {
                lat: f64::NAN,
                lon: 116.404,
            },
            Bd09Coord {
                lat: 39.915,
                lon: f64::INFINITY,
            },
            Bd09Coord {
                lat: f64::NEG_INFINITY,
                lon: f64::NAN,
            },
        ] {
            assert!(
                matches!(
                    fold_bd09_to_gcj02(input),
                    Err(DatumFoldError::NonFinite { .. })
                ),
                "non-finite input must be refused, not folded"
            );
        }
    }

    /// The iteration only contracts while the forward transform's perturbation stays small.
    /// At an absurd magnitude it does not, and the fold refuses rather than returning an
    /// uncertified approximation — the postcondition is "`Ok` implies inside the bound".
    #[test]
    fn non_convergent_input_is_refused_rather_than_approximated() {
        let input = Bd09Coord {
            lat: 1.0e30,
            lon: 1.0e30,
        };
        assert!(
            matches!(
                fold_bd09_to_gcj02(input),
                Err(DatumFoldError::DidNotConverge { .. })
            ),
            "a non-convergent fold must refuse, never return an uncertified estimate"
        );
    }

    /// Every `Ok` is certified: pushing the folded GCJ-02 point back through the closed-form
    /// forward transform reproduces the BD-09 input inside the enforced tolerance.
    #[test]
    fn accepted_folds_reproduce_their_input_under_the_forward_transform() {
        for truth in china_grid() {
            let (bd_lat, bd_lon) = gcj02_to_bd09(truth.0, truth.1);
            let (lat, lon) = fold_bd09_to_gcj02(Bd09Coord {
                lat: bd_lat,
                lon: bd_lon,
            })
            .unwrap();
            let (probe_lat, probe_lon) = gcj02_to_bd09(lat, lon);
            assert!(
                (probe_lat - bd_lat).abs() <= BD09_FOLD_TOLERANCE_DEGREES
                    && (probe_lon - bd_lon).abs() <= BD09_FOLD_TOLERANCE_DEGREES,
                "folded point must map forward onto its BD-09 input within tolerance"
            );
        }
    }
}
