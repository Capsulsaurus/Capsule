use serde_json::json;

use super::*;

fn macro_report(benchmarks: Value) -> Value {
    json!({
        "context": { "build": { "model": "Pixel 8" } },
        "benchmarks": benchmarks,
    })
}

#[test]
fn formats_parse_by_name() {
    assert_eq!(
        Format::parse("macrobenchmark").expect("known"),
        Format::Macrobenchmark
    );
    assert_eq!(Format::parse("xcresult").expect("known"), Format::Xcresult);
    assert!(Format::parse("instruments").is_err());
}

#[test]
fn measures_map_with_either_first_letter_case_only() {
    assert_eq!(Measure::parse("scroll"), Some(Measure::Scroll));
    assert_eq!(Measure::parse("Scroll"), Some(Measure::Scroll));
    assert_eq!(
        Measure::parse("ColdStartToFirstThumbnail"),
        Some(Measure::ColdStartToFirstThumbnail)
    );
    assert_eq!(Measure::parse("SCROLL"), None);
    assert_eq!(Measure::parse(""), None);
}

#[test]
fn medians_ignore_non_finite_samples() {
    assert_eq!(median(vec![3.0, 1.0, 2.0]), Some(2.0));
    assert_eq!(median(vec![4.0, 1.0, 2.0, 3.0]), Some(2.5));
    assert_eq!(median(vec![f64::NAN, 5.0]), Some(5.0));
    assert_eq!(median(vec![]), None);
}

#[test]
fn macrobenchmark_startup_prefers_full_display_and_falls_back_to_initial() {
    let report = macro_report(json!([
        {
            "name": "capsule_coldStartToFirstThumbnail",
            "metrics": {
                "timeToInitialDisplayMs": { "median": 300.0, "runs": [300.0] },
                "timeToFullDisplayMs": { "median": 640.0, "runs": [640.0] },
            },
        },
        {
            "name": "platformApp_coldStartToFirstThumbnail[mode=cold]",
            "metrics": { "timeToInitialDisplayMs": { "runs": [500.0, 450.0, 470.0] } },
        },
    ]));
    let results = import(Format::Macrobenchmark, &report, "pixel-8", 100_000).expect("imports");
    assert_eq!(results.platform, "android");
    assert_eq!(
        results.get(Client::Capsule, Metric::ColdStartFirstThumbnailMs),
        Some(640.0)
    );
    assert_eq!(
        results.get(Client::PlatformApp, Metric::ColdStartFirstThumbnailMs),
        Some(470.0)
    );
}

#[test]
fn macrobenchmark_capsule_startup_without_a_full_display_report_is_refused() {
    let report = macro_report(json!([{
        "name": "capsule_coldStartToFirstThumbnail",
        "metrics": { "timeToInitialDisplayMs": { "median": 300.0 } },
    }]));
    let error = import(Format::Macrobenchmark, &report, "pixel-8", 100_000)
        .expect_err("Capsule must report fully drawn");
    assert!(error.to_string().contains("timeToFullDisplayMs"), "{error}");
}

#[test]
fn macrobenchmark_scroll_counts_overrun_frames_across_every_iteration() {
    let report = macro_report(json!([{
        "name": "capsule_scroll",
        "sampledMetrics": {
            "frameOverrunMs": {
                "P50": -6.0, "P95": 1.5,
                "runs": [[-6.0, -5.0, 2.0, -1.0], [-7.0, 0.0, 3.0, -2.0]],
            },
        },
    }]));
    let results = import(Format::Macrobenchmark, &report, "pixel-8", 100_000).expect("imports");
    // Two of eight frames overran; a frame landing exactly on its deadline did not drop.
    assert_eq!(
        results.get(Client::Capsule, Metric::ScrollDroppedFramesPct),
        Some(25.0)
    );
    assert_eq!(
        results.get(Client::Capsule, Metric::ScrollFrameOverrunP95Ms),
        Some(1.5)
    );
}

#[test]
fn macrobenchmark_trace_sections_are_found_by_their_label() {
    let report = macro_report(json!([
        {
            "name": "capsule_fullResolutionOpen",
            "metrics": { "fullResolutionOpenFirstMs": { "median": 180.0 } },
        },
        {
            "name": "capsule_inputToFrame",
            "metrics": { "inputToFrameSumMs": { "runs": [9.0, 11.0, 10.0] } },
        },
    ]));
    let results = import(Format::Macrobenchmark, &report, "pixel-8", 100_000).expect("imports");
    assert_eq!(
        results.get(Client::Capsule, Metric::FullResolutionOpenMs),
        Some(180.0)
    );
    assert_eq!(
        results.get(Client::Capsule, Metric::InputToFrameMs),
        Some(10.0)
    );
}

#[test]
fn macrobenchmark_refuses_unmapped_duplicated_or_empty_reports() {
    let unmapped = macro_report(json!([{ "name": "startup", "metrics": {} }]));
    let error =
        import(Format::Macrobenchmark, &unmapped, "pixel-8", 100_000).expect_err("unmapped name");
    assert!(error.to_string().contains("`startup`"), "{error}");

    let entry = json!({
        "name": "capsule_fullResolutionOpen",
        "metrics": { "fullResolutionOpenFirstMs": { "median": 180.0 } },
    });
    let duplicated = macro_report(json!([entry.clone(), entry]));
    assert!(import(Format::Macrobenchmark, &duplicated, "pixel-8", 100_000).is_err());

    let silent = macro_report(json!([{ "name": "capsule_scroll", "sampledMetrics": {} }]));
    assert!(import(Format::Macrobenchmark, &silent, "pixel-8", 100_000).is_err());

    assert!(
        import(
            Format::Macrobenchmark,
            &macro_report(json!([])),
            "pixel-8",
            100_000
        )
        .is_err()
    );
    assert!(import(Format::Macrobenchmark, &json!({}), "pixel-8", 100_000).is_err());
}

fn xc_metric(identifier: &str, unit: &str, measurements: &[f64]) -> Value {
    json!({
        "identifier": identifier,
        "displayName": identifier,
        "unitOfMeasurement": unit,
        "measurements": measurements,
    })
}

#[test]
fn xcresult_clock_metrics_become_milliseconds() {
    let report = json!([
        {
            "testIdentifier": "ParityBenchmarkTests/testCapsuleColdStartToFirstThumbnail()",
            "testRuns": [{ "metrics": [
                xc_metric("com.apple.dt.XCTMetric_Clock.time.monotonic", "s", &[0.6, 0.8, 0.7]),
            ] }],
        },
        {
            "testIdentifier": "ParityBenchmarkTests/testPlatformAppFullResolutionOpen()",
            "testRuns": [
                { "metrics": [xc_metric("com.apple.dt.XCTMetric_Clock.time.monotonic", "ms", &[150.0])] },
                { "metrics": [xc_metric("com.apple.dt.XCTMetric_Clock.time.monotonic", "ms", &[170.0, 160.0])] },
            ],
        },
    ]);
    let results = import(Format::Xcresult, &report, "iphone-17-pro", 100_000).expect("imports");
    assert_eq!(results.platform, "apple");
    let cold = results
        .get(Client::Capsule, Metric::ColdStartFirstThumbnailMs)
        .expect("measured");
    assert!((cold - 700.0).abs() < 1e-9, "{cold}");
    assert_eq!(
        results.get(Client::PlatformApp, Metric::FullResolutionOpenMs),
        Some(160.0)
    );
}

#[test]
fn xcresult_scroll_reads_frame_rate_and_hitches_from_the_signpost_metric() {
    let scroll = "com.apple.dt.XCTMetric_OSSignpost-Scroll_DraggingAndDeceleration";
    let report = json!({ "tests": [{
        "testIdentifier": "ParityBenchmarkTests/testCapsuleScroll()",
        "testRuns": [{ "metrics": [
            xc_metric(&format!("{scroll}.frame_rate"), "fps", &[59.0, 60.0, 58.0]),
            xc_metric(&format!("{scroll}.frame_count"), "frames", &[200.0]),
            xc_metric(&format!("{scroll}.hitch_count"), "hitches", &[3.0]),
            xc_metric(&format!("{scroll}.hitch_time_ratio"), "ms per s", &[4.0]),
        ] }],
    }] });
    let results = import(Format::Xcresult, &report, "iphone-17-pro", 100_000).expect("imports");
    assert_eq!(
        results.get(Client::Capsule, Metric::ScrollFrameRateFps),
        Some(59.0)
    );
    assert_eq!(
        results.get(Client::Capsule, Metric::ScrollDroppedFramesPct),
        Some(1.5)
    );
}

#[test]
fn xcresult_matches_display_names_when_identifiers_differ() {
    let report = json!([{
        "testIdentifier": "ParityBenchmarkTests/testPlatformAppScroll()",
        "testRuns": [{ "metrics": [
            { "identifier": "x.1", "displayName": "Frame Rate", "unitOfMeasurement": "fps", "measurements": [120.0] },
            { "identifier": "x.2", "displayName": "Frame Count", "unitOfMeasurement": "frames", "measurements": [400.0] },
            { "identifier": "x.3", "displayName": "Hitch Count", "unitOfMeasurement": "hitches", "measurements": [0.0] },
        ] }],
    }]);
    let results = import(Format::Xcresult, &report, "ipad", 100_000).expect("imports");
    assert_eq!(
        results.get(Client::PlatformApp, Metric::ScrollFrameRateFps),
        Some(120.0)
    );
    assert_eq!(
        results.get(Client::PlatformApp, Metric::ScrollDroppedFramesPct),
        Some(0.0)
    );
}

#[test]
fn xcresult_refuses_unmapped_tests_and_unknown_units() {
    let unmapped =
        json!([{ "testIdentifier": "ParityBenchmarkTests/testLaunch()", "testRuns": [] }]);
    assert!(import(Format::Xcresult, &unmapped, "ipad", 100_000).is_err());

    let not_a_test = json!([{ "testIdentifier": "ParityBenchmarkTests/capsuleScroll()" }]);
    assert!(import(Format::Xcresult, &not_a_test, "ipad", 100_000).is_err());

    let minutes = json!([{
        "testIdentifier": "ParityBenchmarkTests/testCapsuleFullResolutionOpen()",
        "testRuns": [{ "metrics": [xc_metric("XCTMetric_Clock.time", "min", &[1.0])] }],
    }]);
    let error = import(Format::Xcresult, &minutes, "ipad", 100_000).expect_err("unit");
    assert!(error.to_string().contains("`min`"), "{error}");

    let no_clock = json!([{
        "testIdentifier": "ParityBenchmarkTests/testCapsuleInputToFrame()",
        "testRuns": [{ "metrics": [] }],
    }]);
    assert!(import(Format::Xcresult, &no_clock, "ipad", 100_000).is_err());

    assert!(import(Format::Xcresult, &json!({ "other": 1 }), "ipad", 100_000).is_err());
}
