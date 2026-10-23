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
