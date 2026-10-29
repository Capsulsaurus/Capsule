use super::*;

fn results(capsule: &[(Metric, f64)], platform_app: &[(Metric, f64)]) -> Results {
    let mut results = Results::new("android", "pixel-8", 100_000);
    for &(metric, value) in capsule {
        results.set(Client::Capsule, metric, value);
    }
    for &(metric, value) in platform_app {
        results.set(Client::PlatformApp, metric, value);
    }
    results
}

/// Asserts two limits agree to well within any metric's resolution.
fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "expected {expected}, got {actual}"
    );
}

fn baseline(entries: &[(Metric, f64, f64)]) -> Baseline {
    Baseline {
        schema: SCHEMA,
        platform: "android".to_string(),
        device: "pixel-8".to_string(),
        library_size: 100_000,
        metrics: entries
            .iter()
            .map(|&(metric, value, tolerance_pct)| {
                (
                    metric.key().to_string(),
                    BaselineEntry {
                        value,
                        tolerance_pct,
                    },
                )
            })
            .collect(),
    }
}

#[test]
fn every_metric_key_round_trips_and_only_frame_rate_improves_upward() {
    for metric in Metric::ALL {
        assert_eq!(Metric::from_key(metric.key()), Some(metric));
        let expected = if metric == Metric::ScrollFrameRateFps {
            Better::Higher
        } else {
            Better::Lower
        };
        assert_eq!(metric.better(), expected, "{}", metric.key());
    }
    assert_eq!(Metric::from_key("frame_rate"), None);
}

#[test]
fn a_run_inside_tolerance_passes() {
    let baseline = baseline(&[
        (Metric::ColdStartFirstThumbnailMs, 800.0, 10.0),
        (Metric::ScrollFrameRateFps, 60.0, 10.0),
    ]);
    let results = results(
        &[
            (Metric::ColdStartFirstThumbnailMs, 879.0),
            (Metric::ScrollFrameRateFps, 54.5),
        ],
        &[],
    );
    let verdict = compare(&baseline, &results).expect("comparable");
    assert!(verdict.passed(), "{verdict:?}");
}

#[test]
fn a_lower_is_better_metric_regresses_past_its_tolerance() {
    let baseline = baseline(&[(Metric::ColdStartFirstThumbnailMs, 800.0, 10.0)]);
    let results = results(&[(Metric::ColdStartFirstThumbnailMs, 881.0)], &[]);
    let verdict = compare(&baseline, &results).expect("comparable");
    assert!(!verdict.passed());
    assert_eq!(
        verdict.regressions,
        vec![Regression {
            metric: Metric::ColdStartFirstThumbnailMs,
            baseline: 800.0,
            measured: 881.0,
            limit: 880.0,
        }]
    );
}

#[test]
fn a_higher_is_better_metric_regresses_when_it_falls() {
    let baseline = baseline(&[(Metric::ScrollFrameRateFps, 60.0, 5.0)]);
    let verdict = compare(
        &baseline,
        &results(&[(Metric::ScrollFrameRateFps, 56.9)], &[]),
    )
    .expect("comparable");
    assert_eq!(verdict.regressions.len(), 1);
    // Faster than the baseline is never a regression.
    let verdict = compare(
        &baseline,
        &results(&[(Metric::ScrollFrameRateFps, 120.0)], &[]),
    )
    .expect("comparable");
    assert!(verdict.passed());
}

#[test]
fn a_negative_baseline_widens_its_limit_in_the_worse_direction() {
    // Frame overrun is negative when frames finish early; tolerance must still loosen upward.
    let baseline = baseline(&[(Metric::ScrollFrameOverrunP95Ms, -4.0, 50.0)]);
    let pass = compare(
        &baseline,
        &results(&[(Metric::ScrollFrameOverrunP95Ms, -2.5)], &[]),
    )
    .expect("comparable");
    assert!(pass.passed());
    let fail = compare(
        &baseline,
        &results(&[(Metric::ScrollFrameOverrunP95Ms, -1.0)], &[]),
    )
    .expect("comparable");
    assert_eq!(fail.regressions.first().map(|r| r.limit), Some(-2.0));
}

#[test]
fn a_zero_baseline_still_tolerates_the_metrics_absolute_floor() {
    // 10% of nothing is nothing: without a floor one dropped frame would fail the gate.
    let baseline = baseline(&[
        (Metric::ScrollDroppedFramesPct, 0.0, 10.0),
        (Metric::ScrollFrameOverrunP95Ms, 0.0, 10.0),
    ]);
    let one_frame = compare(
        &baseline,
        &results(
            &[
                (Metric::ScrollDroppedFramesPct, 0.8),
                (Metric::ScrollFrameOverrunP95Ms, 0.5),
            ],
            &[],
        ),
    )
    .expect("comparable");
    assert!(one_frame.passed(), "{one_frame:?}");

    let past_floor = compare(
        &baseline,
        &results(
            &[
                (Metric::ScrollDroppedFramesPct, 2.5),
                (Metric::ScrollFrameOverrunP95Ms, 1.5),
            ],
            &[],
        ),
    )
    .expect("comparable");
    let limits: Vec<_> = past_floor
        .regressions
        .iter()
        .map(|r| (r.metric, r.limit))
        .collect();
    assert_eq!(
        limits,
        vec![
            (
                Metric::ScrollDroppedFramesPct,
                Metric::ScrollDroppedFramesPct.min_slack()
            ),
            (
                Metric::ScrollFrameOverrunP95Ms,
                Metric::ScrollFrameOverrunP95Ms.min_slack()
            ),
        ]
    );
}

#[test]
fn the_percentage_wins_once_it_exceeds_the_floor() {
    let entry = BaselineEntry {
        value: 800.0,
        tolerance_pct: 10.0,
    };
    assert_close(limit(Metric::ColdStartFirstThumbnailMs, entry), 880.0);
    let tight = BaselineEntry {
        value: 800.0,
        tolerance_pct: 0.0,
    };
    assert_close(
        limit(Metric::ColdStartFirstThumbnailMs, tight),
        800.0 + Metric::ColdStartFirstThumbnailMs.min_slack(),
    );
    for metric in Metric::ALL {
        assert!(metric.min_slack() > 0.0, "{}", metric.key());
    }
}

#[test]
fn a_baselined_metric_that_disappears_fails_the_gate() {
    let baseline = baseline(&[
        (Metric::ColdStartFirstThumbnailMs, 800.0, 10.0),
        (Metric::FullResolutionOpenMs, 200.0, 10.0),
    ]);
    let verdict = compare(
        &baseline,
        &results(&[(Metric::ColdStartFirstThumbnailMs, 700.0)], &[]),
    )
    .expect("comparable");
    assert!(!verdict.passed());
    assert_eq!(verdict.missing, vec![Metric::FullResolutionOpenMs]);
}

#[test]
fn a_new_metric_is_reported_without_failing() {
    let baseline = baseline(&[(Metric::ColdStartFirstThumbnailMs, 800.0, 10.0)]);
    let verdict = compare(
        &baseline,
        &results(
            &[
                (Metric::ColdStartFirstThumbnailMs, 800.0),
                (Metric::InputToFrameMs, 9.0),
            ],
            &[],
        ),
    )
    .expect("comparable");
    assert!(verdict.passed());
    assert_eq!(verdict.unbaselined, vec![Metric::InputToFrameMs]);
}

#[test]
fn the_platform_app_never_enters_the_regression_gate() {
    let baseline = baseline(&[(Metric::ColdStartFirstThumbnailMs, 800.0, 10.0)]);
    let verdict = compare(
        &baseline,
        &results(
            &[(Metric::ColdStartFirstThumbnailMs, 800.0)],
            &[(Metric::ColdStartFirstThumbnailMs, 5_000.0)],
        ),
    )
    .expect("comparable");
    assert!(verdict.passed());
}

#[test]
fn another_device_platform_or_library_size_is_refused() {
    let base = baseline(&[(Metric::ColdStartFirstThumbnailMs, 800.0, 10.0)]);
    let run = results(&[(Metric::ColdStartFirstThumbnailMs, 800.0)], &[]);

    let mut other = run.clone();
    other.device = "pixel-9".to_string();
    assert!(compare(&base, &other).is_err());

    let mut other = run.clone();
    other.platform = "apple".to_string();
    assert!(compare(&base, &other).is_err());

    let mut other = run;
    other.library_size = 4_000;
    let error = compare(&base, &other).expect_err("library size differs");
    assert!(error.to_string().contains("100000"), "{error}");
}

#[test]
fn unknown_or_non_finite_metrics_are_refused() {
    let base = baseline(&[]);
    let mut run = results(&[], &[]);
    run.clients
        .entry("capsule".to_string())
        .or_default()
        .insert("frames".to_string(), 1.0);
    assert!(compare(&base, &run).is_err());

    let run = results(&[(Metric::InputToFrameMs, f64::NAN)], &[]);
    assert!(compare(&base, &run).is_err());

    let mut stale = base;
    stale.schema = 0;
    assert!(stale.validate().is_err());
}

#[test]
fn recording_takes_capsules_numbers_and_keeps_tuned_tolerances() {
    let run = results(
        &[
            (Metric::ColdStartFirstThumbnailMs, 750.0),
            (Metric::ScrollDroppedFramesPct, 2.0),
        ],
        &[(Metric::ColdStartFirstThumbnailMs, 600.0)],
    );
    let previous = baseline(&[(Metric::ScrollDroppedFramesPct, 3.0, 25.0)]);
    let recorded = record(&run, Some(&previous), 10.0).expect("recordable");
    assert_eq!(recorded.device, "pixel-8");
    assert_eq!(recorded.library_size, 100_000);
    assert_eq!(
        recorded.metrics,
        baseline(&[
            (Metric::ColdStartFirstThumbnailMs, 750.0, 10.0),
            (Metric::ScrollDroppedFramesPct, 2.0, 25.0),
        ])
        .metrics
    );
    // The baseline it records passes against the run it was recorded from.
    assert!(compare(&recorded, &run).expect("comparable").passed());
}

#[test]
fn recording_refuses_a_run_without_capsule_or_with_a_bad_tolerance() {
    let platform_only = results(&[], &[(Metric::ColdStartFirstThumbnailMs, 600.0)]);
    assert!(record(&platform_only, None, 10.0).is_err());
    let run = results(&[(Metric::ColdStartFirstThumbnailMs, 600.0)], &[]);
    assert!(record(&run, None, -1.0).is_err());
    assert!(record(&run, None, f64::INFINITY).is_err());
}

#[test]
fn parity_compares_each_metric_in_its_own_direction() {
    let run = results(
        &[
            (Metric::ColdStartFirstThumbnailMs, 500.0),
            (Metric::ScrollFrameRateFps, 55.0),
            (Metric::InputToFrameMs, 8.0),
        ],
        &[
            (Metric::ColdStartFirstThumbnailMs, 500.0),
            (Metric::ScrollFrameRateFps, 60.0),
            (Metric::FullResolutionOpenMs, 150.0),
        ],
    );
    assert_eq!(
        parity(&run),
        vec![
            (
                Metric::ColdStartFirstThumbnailMs,
                Standing::AtLeastAsFast {
                    capsule: 500.0,
                    platform_app: 500.0
                }
            ),
            (
                Metric::ScrollFrameRateFps,
                Standing::Behind {
                    capsule: 55.0,
                    platform_app: 60.0
                }
            ),
            (
                Metric::FullResolutionOpenMs,
                Standing::NotMeasured {
                    capsule: None,
                    platform_app: Some(150.0)
                }
            ),
            (
                Metric::InputToFrameMs,
                Standing::NotMeasured {
                    capsule: Some(8.0),
                    platform_app: None
                }
            ),
        ]
    );
}

#[test]
fn the_report_names_every_failure_and_says_parity_does_not_gate() {
    let verdict = Verdict {
        regressions: vec![Regression {
            metric: Metric::FullResolutionOpenMs,
            baseline: 200.0,
            measured: 260.0,
            limit: 220.0,
        }],
        missing: vec![Metric::InputToFrameMs],
        unbaselined: vec![Metric::ScrollFrameRateFps],
    };
    let report = render(
        &verdict,
        &[(
            Metric::FullResolutionOpenMs,
            Standing::Behind {
                capsule: 260.0,
                platform_app: 180.0,
            },
        )],
    );
    assert!(report.starts_with("regression gate: FAIL\n"), "{report}");
    assert!(
        report
            .contains("regressed full_resolution_open_ms: 260.00 (baseline 200.00, limit 220.00)")
    );
    assert!(report.contains("no longer measured: input_to_frame_ms"));
    assert!(report.contains("measured without a baseline: scroll_frame_rate_fps"));
    assert!(report.contains("(reported, not gating)"));
    assert!(report.contains("full_resolution_open_ms: behind (260.00 vs 180.00)"));
}

#[test]
fn results_and_baselines_round_trip_through_json() {
    let run = results(&[(Metric::ColdStartFirstThumbnailMs, 750.0)], &[]);
    let text = serde_json::to_string(&run).expect("serializes");
    assert_eq!(serde_json::from_str::<Results>(&text).expect("parses"), run);
    let base = record(&run, None, DEFAULT_TOLERANCE_PCT).expect("recordable");
    let text = serde_json::to_string(&base).expect("serializes");
    assert_eq!(
        serde_json::from_str::<Baseline>(&text).expect("parses"),
        base
    );
}

#[test]
fn record_then_compare_through_the_command_line() {
    let dir = tempfile::tempdir().expect("tempdir");
    let results_path = dir.path().join("results.json");
    let baseline_path = dir.path().join("baselines/pixel-8.json");
    let path = |p: &Path| p.to_str().expect("utf-8 path").to_string();
    let args = |items: &[&str]| items.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();

    let run = results(&[(Metric::ColdStartFirstThumbnailMs, 750.0)], &[]);
    write_json(&results_path, &run).expect("writes results");

    // No baseline yet: the gate refuses rather than passing on nothing.
    let error = super::run(
        args(&[
            "compare",
            "--results",
            &path(&results_path),
            "--baseline",
            &path(&baseline_path),
        ])
        .into_iter(),
    )
    .expect_err("no baseline");
    assert!(
        error.to_string().contains("no baseline recorded"),
        "{error}"
    );

    super::run(
        args(&[
            "record",
            "--results",
            &path(&results_path),
            "--baseline",
            &path(&baseline_path),
        ])
        .into_iter(),
    )
    .expect("records");
    super::run(
        args(&[
            "compare",
            "--results",
            &path(&results_path),
            "--baseline",
            &path(&baseline_path),
        ])
        .into_iter(),
    )
    .expect("passes against itself");

    let slower = results(&[(Metric::ColdStartFirstThumbnailMs, 900.0)], &[]);
    write_json(&results_path, &slower).expect("writes results");
    assert!(
        super::run(
            args(&[
                "compare",
                "--results",
                &path(&results_path),
                "--baseline",
                &path(&baseline_path)
            ])
            .into_iter(),
        )
        .is_err()
    );
}

#[test]
fn the_command_line_refuses_unknown_repeated_or_missing_flags() {
    let args = |items: &[&str]| items.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert!(super::run(args(&["compare", "--result", "x"]).into_iter()).is_err());
    assert!(
        super::run(args(&["compare", "--results", "a", "--results", "b"]).into_iter()).is_err()
    );
    assert!(super::run(args(&["compare", "--results"]).into_iter()).is_err());
    assert!(super::run(args(&["record", "--baseline", "b"]).into_iter()).is_err());
    assert!(super::run(args(&["explode"]).into_iter()).is_err());
    assert!(super::run(std::iter::empty()).is_err());
}
