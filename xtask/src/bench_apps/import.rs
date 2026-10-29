//! Normalizes each platform harness's report into [`Results`].
//!
//! Both harnesses name their tests by one convention, so the mapping is mechanical and a test
//! nobody mapped is an error rather than a silent gap:
//!
//! | Harness | Name | Client | Measure |
//! | --- | --- | --- | --- |
//! | Macrobenchmark | `capsule_scroll` | `capsule_` / `platformApp_` | the rest |
//! | XCTest | `testCapsuleScroll()` | `Capsule` / `PlatformApp` | the rest |
//!
//! The measures are `coldStartToFirstThumbnail`, `scroll`, `fullResolutionOpen` and
//! `inputToFrame` (capitalized after the client on Apple). Each value is the median over the
//! harness's iterations.

use eyre::{ContextCompat, Result, bail, ensure};
use serde_json::Value;
use tracing::debug;

use super::{Client, Metric, Results};

/// The report a harness produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    /// Macrobenchmark's `*-benchmarkData.json`.
    Macrobenchmark,
    /// `xcrun xcresulttool get test-results metrics --path <bundle>.xcresult`.
    Xcresult,
}

impl Format {
    pub(crate) fn parse(raw: &str) -> Result<Format> {
        match raw {
            "macrobenchmark" => Ok(Format::Macrobenchmark),
            "xcresult" => Ok(Format::Xcresult),
            other => bail!("unknown report format `{other}`; expected macrobenchmark or xcresult"),
        }
    }
}

/// What a harness test measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Measure {
    ColdStartToFirstThumbnail,
    Scroll,
    FullResolutionOpen,
    InputToFrame,
}

impl Measure {
    fn parse(raw: &str) -> Option<Measure> {
        // Apple capitalizes the measure after the client; compare case-insensitively on the
        // first letter only, so `Scroll` and `scroll` both map and `SCROLL` does not.
        let mut chars = raw.chars();
        let first = chars.next()?.to_ascii_lowercase();
        let normalized: String = std::iter::once(first).chain(chars).collect();
        match normalized.as_str() {
            "coldStartToFirstThumbnail" => Some(Measure::ColdStartToFirstThumbnail),
            "scroll" => Some(Measure::Scroll),
            "fullResolutionOpen" => Some(Measure::FullResolutionOpen),
            "inputToFrame" => Some(Measure::InputToFrame),
            _ => None,
        }
    }
}

/// Split a harness test name into who and what it measured.
fn classify(name: &str, prefixes: [(&str, Client); 2]) -> Result<(Client, Measure)> {
    for (prefix, client) in prefixes {
        if let Some(rest) = name.strip_prefix(prefix)
            && let Some(measure) = Measure::parse(rest)
        {
            return Ok((client, measure));
        }
    }
    bail!(
        "benchmark `{name}` follows no `<client><measure>` name this importer maps; \
         rename it or teach the importer what it measures"
    )
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    values.retain(|value| value.is_finite());
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        f64::midpoint(values[mid - 1], values[mid])
    } else {
        values[mid]
    })
}

fn numbers(value: &Value) -> Vec<f64> {
    value
        .as_array()
        .map(|items| items.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default()
}

/// Record `value` once; a second test claiming the same client and metric is an error, since
/// keeping either would silently discard the other.
fn put(results: &mut Results, client: Client, metric: Metric, value: f64) -> Result<()> {
    ensure!(
        results.get(client, metric).is_none(),
        "two benchmarks report `{}` for `{}`",
        metric.key(),
        client.key()
    );
    debug!(
        client = client.key(),
        metric = metric.key(),
        value,
        "bench-apps imported metric"
    );
    results.set(client, metric, value);
    Ok(())
}

pub(crate) fn import(
    format: Format,
    report: &Value,
    device: &str,
    library_size: u64,
) -> Result<Results> {
    let results = match format {
        Format::Macrobenchmark => macrobenchmark(report, device, library_size)?,
        Format::Xcresult => xcresult(report, device, library_size)?,
    };
    ensure!(
        !results.clients.is_empty(),
        "the report holds no benchmark this importer maps"
    );
    results.validate()?;
    Ok(results)
}

/// A Macrobenchmark metric's median: the `median` it reports, or the median of its `runs`.
fn macro_median(metric: &Value) -> Option<f64> {
    metric["median"]
        .as_f64()
        .or_else(|| median(numbers(&metric["runs"])))
}

/// The metric whose key starts with `label`: a `TraceSectionMetric` appends its mode to the
/// label it was given (`fullResolutionOpenFirstMs`, `inputToFrameSumMs`).
fn labelled<'a>(metrics: &'a Value, label: &str) -> Option<&'a Value> {
    metrics
        .as_object()?
        .iter()
        .find(|(key, _)| key.starts_with(label))
        .map(|(_, value)| value)
}

fn macrobenchmark(report: &Value, device: &str, library_size: u64) -> Result<Results> {
    let mut results = Results::new("android", device, library_size);
    let benchmarks = report["benchmarks"]
        .as_array()
        .context("Macrobenchmark report has no `benchmarks` array")?;
    for benchmark in benchmarks {
        let raw = benchmark["name"]
            .as_str()
            .context("a Macrobenchmark entry has no name")?;
        // Parameterized tests carry their parameters in brackets after the method name.
        let name = raw.split('[').next().unwrap_or(raw);
        let (client, measure) = classify(
            name,
            [
                ("capsule_", Client::Capsule),
                ("platformApp_", Client::PlatformApp),
            ],
        )?;
        let metrics = &benchmark["metrics"];
        let sampled = &benchmark["sampledMetrics"];
        match measure {
            Measure::ColdStartToFirstThumbnail => {
                // Capsule calls `reportFullyDrawn()` once the first thumbnail is on screen; a
                // platform app that never does is held to its initial display instead.
                let value = metrics
                    .get("timeToFullDisplayMs")
                    .and_then(macro_median)
                    .or_else(|| metrics.get("timeToInitialDisplayMs").and_then(macro_median))
                    .with_context(|| format!("`{raw}` reports no startup timing"))?;
                put(
                    &mut results,
                    client,
                    Metric::ColdStartFirstThumbnailMs,
                    value,
                )?;
            }
            Measure::Scroll => {
                let overrun = sampled
                    .get("frameOverrunMs")
                    .with_context(|| format!("`{raw}` reports no frameOverrunMs"))?;
                let frames: Vec<f64> = overrun["runs"]
                    .as_array()
                    .map(|runs| runs.iter().flat_map(numbers).collect())
                    .unwrap_or_default();
                ensure!(!frames.is_empty(), "`{raw}` recorded no frames");
                let dropped = frames.iter().filter(|&&overrun| overrun > 0.0).count();
                put(
                    &mut results,
                    client,
                    Metric::ScrollDroppedFramesPct,
                    100.0 * dropped as f64 / frames.len() as f64,
                )?;
                let p95 = overrun["P95"]
                    .as_f64()
                    .with_context(|| format!("`{raw}` reports no frameOverrunMs P95"))?;
                put(&mut results, client, Metric::ScrollFrameOverrunP95Ms, p95)?;
            }
            Measure::FullResolutionOpen | Measure::InputToFrame => {
                let (label, metric) = if measure == Measure::FullResolutionOpen {
                    ("fullResolutionOpen", Metric::FullResolutionOpenMs)
                } else {
                    ("inputToFrame", Metric::InputToFrameMs)
                };
                let value = labelled(metrics, label)
                    .and_then(macro_median)
                    .with_context(|| format!("`{raw}` reports no `{label}` trace section"))?;
                put(&mut results, client, metric, value)?;
            }
        }
    }
    Ok(results)
}

/// One XCTest metric's identifier and display name, folded so either can be matched by suffix.
fn xc_names(metric: &Value) -> (String, String) {
    let fold = |key: &str| {
        metric[key]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .replace([' ', '-'], "_")
    };
    (fold("identifier"), fold("displayName"))
}

/// The median of every measurement, across every run, of the first metric `matches` accepts.
fn xc_median(runs: &[Value], matches: impl Fn(&str, &str) -> bool) -> Option<(f64, String)> {
    let mut found = None;
    let mut values = Vec::new();
    for run in runs {
        for metric in run["metrics"].as_array().into_iter().flatten() {
            let (identifier, display) = xc_names(metric);
            if matches(&identifier, &display) {
                found.get_or_insert_with(|| {
                    metric["unitOfMeasurement"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                });
                values.extend(numbers(&metric["measurements"]));
            }
        }
    }
    Some((median(values)?, found?))
}

/// Milliseconds from an XCTest duration in `unit`.
fn to_ms(value: f64, unit: &str, test: &str) -> Result<f64> {
    match unit {
        "s" => Ok(value * 1000.0),
        "ms" => Ok(value),
        other => bail!("`{test}` reports a duration in `{other}`, not s or ms"),
    }
}

fn xcresult(report: &Value, device: &str, library_size: u64) -> Result<Results> {
    let mut results = Results::new("apple", device, library_size);
    let tests = report
        .as_array()
        .or_else(|| report["tests"].as_array())
        .context("xcresult metrics report is neither an array nor holds `tests`")?;
    for test in tests {
        let identifier = test["testIdentifier"]
            .as_str()
            .context("an xcresult entry has no testIdentifier")?;
        let method = identifier.rsplit('/').next().unwrap_or(identifier);
        let method = method.trim_end_matches("()");
        let name = method
            .strip_prefix("test")
            .with_context(|| format!("`{identifier}` is not an XCTest test method"))?;
        let (client, measure) = classify(
            name,
            [
                ("Capsule", Client::Capsule),
                ("PlatformApp", Client::PlatformApp),
            ],
        )?;
        let runs = test["testRuns"].as_array().map_or(&[][..], Vec::as_slice);
        let clock = |identifier: &str, display: &str| {
            identifier.contains("clock") || display.contains("clock")
        };
        match measure {
            Measure::ColdStartToFirstThumbnail
            | Measure::FullResolutionOpen
            | Measure::InputToFrame => {
                let (value, unit) = xc_median(runs, clock)
                    .with_context(|| format!("`{identifier}` reports no clock metric"))?;
                let metric = match measure {
                    Measure::ColdStartToFirstThumbnail => Metric::ColdStartFirstThumbnailMs,
                    Measure::FullResolutionOpen => Metric::FullResolutionOpenMs,
                    _ => Metric::InputToFrameMs,
                };
                put(
                    &mut results,
                    client,
                    metric,
                    to_ms(value, &unit, identifier)?,
                )?;
            }
            Measure::Scroll => {
                let suffix = |want: &'static str| {
                    move |identifier: &str, display: &str| {
                        identifier.ends_with(want) || display.ends_with(want)
                    }
                };
                let (fps, _) = xc_median(runs, suffix("frame_rate"))
                    .with_context(|| format!("`{identifier}` reports no frame rate"))?;
                put(&mut results, client, Metric::ScrollFrameRateFps, fps)?;
                let (frames, _) = xc_median(runs, suffix("frame_count"))
                    .with_context(|| format!("`{identifier}` reports no frame count"))?;
                let (hitches, _) = xc_median(runs, suffix("hitch_count"))
                    .with_context(|| format!("`{identifier}` reports no hitch count"))?;
                ensure!(frames > 0.0, "`{identifier}` recorded no frames");
                put(
                    &mut results,
                    client,
                    Metric::ScrollDroppedFramesPct,
                    100.0 * hitches / frames,
                )?;
            }
        }
    }
    Ok(results)
}

#[cfg(test)]
mod tests;
