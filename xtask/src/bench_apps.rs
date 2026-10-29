//! The apps' responsiveness parity benchmark: baseline records and the regression gate.
//!
//! The device harnesses (Macrobenchmark on Android, XCTest metrics on Apple) each emit their own
//! report. [`import`] normalizes either into one [`Results`] document — a fixed set of
//! [`Metric`]s per [`Client`], measured on one device against one library size — and this module
//! judges it twice:
//!
//! - **Regression** ([`compare`]): Capsule's numbers against the [`Baseline`] recorded for the
//!   same device. A metric worse than its baseline by more than its tolerance, or a baselined
//!   metric that is no longer measured, fails the gate.
//! - **Parity** ([`parity`]): Capsule against the platform photo app measured in the same run.
//!   It is reported, never gating: the contract that fails CI is the recorded baseline.
//!
//! Commands (driven by `mise run bench-apps`):
//!
//! - `bench-apps import --format macrobenchmark|xcresult --input F --device D --library-size N --out F`
//! - `bench-apps compare --results F --baseline F`
//! - `bench-apps record --results F --baseline F [--tolerance-pct P]`

mod import;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::{fmt, fs};

use eyre::{Context, ContextCompat, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// The only document schema this module reads or writes.
pub(crate) const SCHEMA: u32 = 1;

/// Tolerance a freshly recorded baseline grants every metric unless told otherwise.
pub(crate) const DEFAULT_TOLERANCE_PCT: f64 = 10.0;

const USAGE: &str = "usage: xtask bench-apps <import --format macrobenchmark|xcresult --input F \
                     --device D --library-size N --out F | compare --results F --baseline F | \
                     record --results F --baseline F [--tolerance-pct P]>";

/// Which way a metric improves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Better {
    Lower,
    Higher,
}

/// A responsiveness measurement the parity contract names.
///
/// The key is the serialized name in [`Results`] and [`Baseline`]; renaming one orphans every
/// recorded baseline, so keys are append-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Metric {
    /// Process start to the first library thumbnail drawn.
    ColdStartFirstThumbnailMs,
    /// Frames presented per second while flinging the library grid.
    ScrollFrameRateFps,
    /// Share of the frames rendered during the fling that missed their deadline.
    ScrollDroppedFramesPct,
    /// 95th-percentile frame overrun during the fling (negative is headroom).
    ScrollFrameOverrunP95Ms,
    /// Tap on a thumbnail to the full-resolution image on screen.
    FullResolutionOpenMs,
    /// Input event to the frame that answers it.
    InputToFrameMs,
}

impl Metric {
    pub(crate) const ALL: [Metric; 6] = [
        Metric::ColdStartFirstThumbnailMs,
        Metric::ScrollFrameRateFps,
        Metric::ScrollDroppedFramesPct,
        Metric::ScrollFrameOverrunP95Ms,
        Metric::FullResolutionOpenMs,
        Metric::InputToFrameMs,
    ];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Metric::ColdStartFirstThumbnailMs => "cold_start_first_thumbnail_ms",
            Metric::ScrollFrameRateFps => "scroll_frame_rate_fps",
            Metric::ScrollDroppedFramesPct => "scroll_dropped_frames_pct",
            Metric::ScrollFrameOverrunP95Ms => "scroll_frame_overrun_p95_ms",
            Metric::FullResolutionOpenMs => "full_resolution_open_ms",
            Metric::InputToFrameMs => "input_to_frame_ms",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<Metric> {
        Metric::ALL.into_iter().find(|metric| metric.key() == key)
    }

    pub(crate) fn better(self) -> Better {
        match self {
            Metric::ScrollFrameRateFps => Better::Higher,
            _ => Better::Lower,
        }
    }
}

/// Who was measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Client {
    Capsule,
    /// Photos on Apple; Google Photos or the OEM gallery on Android.
    PlatformApp,
}

impl Client {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Client::Capsule => "capsule",
            Client::PlatformApp => "platform_app",
        }
    }
}

/// One run's normalized measurements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Results {
    pub(crate) schema: u32,
    /// `android` or `apple`.
    pub(crate) platform: String,
    /// Stable device key; the baseline file is named after it.
    pub(crate) device: String,
    /// Assets in the library both clients were measured against.
    pub(crate) library_size: u64,
    /// Client key to metric key to value.
    pub(crate) clients: BTreeMap<String, BTreeMap<String, f64>>,
}

impl Results {
    pub(crate) fn new(platform: &str, device: &str, library_size: u64) -> Results {
        Results {
            schema: SCHEMA,
            platform: platform.to_string(),
            device: device.to_string(),
            library_size,
            clients: BTreeMap::new(),
        }
    }

    pub(crate) fn set(&mut self, client: Client, metric: Metric, value: f64) {
        self.clients
            .entry(client.key().to_string())
            .or_default()
            .insert(metric.key().to_string(), value);
    }

    pub(crate) fn get(&self, client: Client, metric: Metric) -> Option<f64> {
        self.clients
            .get(client.key())
            .and_then(|metrics| metrics.get(metric.key()))
            .copied()
    }

    /// Reject a document this module cannot judge: another schema, an empty device, a metric
    /// key nobody defined, or a value that is not a finite number.
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == SCHEMA,
            "results schema {} is not {SCHEMA}",
            self.schema
        );
        ensure!(!self.device.is_empty(), "results name no device");
        ensure!(self.library_size > 0, "results name no library size");
        for (client, metrics) in &self.clients {
            for (key, value) in metrics {
                ensure!(
                    Metric::from_key(key).is_some(),
                    "results for `{client}` carry unknown metric `{key}`"
                );
                ensure!(
                    value.is_finite(),
                    "results for `{client}` carry a non-finite `{key}`"
                );
            }
        }
        Ok(())
    }
}

/// Capsule's recorded numbers on one device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Baseline {
    pub(crate) schema: u32,
    pub(crate) platform: String,
    pub(crate) device: String,
    pub(crate) library_size: u64,
    pub(crate) metrics: BTreeMap<String, BaselineEntry>,
}

/// One baselined metric and how far it may drift before the gate fails.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct BaselineEntry {
    pub(crate) value: f64,
    /// Allowed worsening, as a percentage of `value`.
    pub(crate) tolerance_pct: f64,
}

impl Baseline {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == SCHEMA,
            "baseline schema {} is not {SCHEMA}",
            self.schema
        );
        for (key, entry) in &self.metrics {
            ensure!(
                Metric::from_key(key).is_some(),
                "baseline carries unknown metric `{key}`"
            );
            ensure!(
                entry.value.is_finite() && entry.tolerance_pct.is_finite(),
                "baseline `{key}` is not a finite number"
            );
            ensure!(
                entry.tolerance_pct >= 0.0,
                "baseline `{key}` has a negative tolerance"
            );
        }
        Ok(())
    }
}

/// Record Capsule's numbers in `results` as a baseline granting each metric `tolerance_pct`.
///
/// A metric that `previous` already baselines keeps its own tolerance: re-recording moves the
/// value, never a threshold someone tuned by hand.
pub(crate) fn record(
    results: &Results,
    previous: Option<&Baseline>,
    tolerance_pct: f64,
) -> Result<Baseline> {
    results.validate()?;
    ensure!(
        tolerance_pct.is_finite() && tolerance_pct >= 0.0,
        "tolerance must be a non-negative percentage"
    );
    let capsule = results
        .clients
        .get(Client::Capsule.key())
        .filter(|metrics| !metrics.is_empty())
        .context("results hold no Capsule measurements to record")?;
    let metrics = capsule
        .iter()
        .map(|(key, &value)| {
            let tolerance_pct = previous
                .and_then(|baseline| baseline.metrics.get(key))
                .map_or(tolerance_pct, |entry| entry.tolerance_pct);
            (
                key.clone(),
                BaselineEntry {
                    value,
                    tolerance_pct,
                },
            )
        })
        .collect();
    Ok(Baseline {
        schema: SCHEMA,
        platform: results.platform.clone(),
        device: results.device.clone(),
        library_size: results.library_size,
        metrics,
    })
}

/// A baselined metric that got worse than its tolerance allows.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Regression {
    pub(crate) metric: Metric,
    pub(crate) baseline: f64,
    pub(crate) measured: f64,
    /// The worst value still inside tolerance.
    pub(crate) limit: f64,
}

/// The regression verdict for one run.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Verdict {
    pub(crate) regressions: Vec<Regression>,
    /// Baselined metrics the run no longer measured.
    pub(crate) missing: Vec<Metric>,
    /// Metrics measured with no baseline yet; reported, not judged.
    pub(crate) unbaselined: Vec<Metric>,
}

impl Verdict {
    pub(crate) fn passed(&self) -> bool {
        self.regressions.is_empty() && self.missing.is_empty()
    }
}

/// The worst value `entry` tolerates for `metric`.
fn limit(metric: Metric, entry: BaselineEntry) -> f64 {
    let slack = entry.value.abs() * entry.tolerance_pct / 100.0;
    match metric.better() {
        Better::Lower => entry.value + slack,
        Better::Higher => entry.value - slack,
    }
}

/// Judge Capsule's numbers in `results` against `baseline`.
///
/// A baseline for another device, platform or library size is refused rather than compared:
/// numbers from two different set-ups say nothing about a regression.
pub(crate) fn compare(baseline: &Baseline, results: &Results) -> Result<Verdict> {
    baseline.validate()?;
    results.validate()?;
    ensure!(
        baseline.platform == results.platform,
        "baseline is for platform `{}`, results are for `{}`",
        baseline.platform,
        results.platform
    );
    ensure!(
        baseline.device == results.device,
        "baseline is for device `{}`, results are for `{}`",
        baseline.device,
        results.device
    );
    ensure!(
        baseline.library_size == results.library_size,
        "baseline was recorded against {} assets, results against {}",
        baseline.library_size,
        results.library_size
    );
    let mut verdict = Verdict::default();
    for metric in Metric::ALL {
        let entry = baseline.metrics.get(metric.key()).copied();
        let measured = results.get(Client::Capsule, metric);
        match (entry, measured) {
            (Some(entry), Some(measured)) => {
                let limit = limit(metric, entry);
                let worse = match metric.better() {
                    Better::Lower => measured > limit,
                    Better::Higher => measured < limit,
                };
                debug!(
                    metric = metric.key(),
                    baseline = entry.value,
                    measured,
                    limit,
                    worse,
                    "bench-apps compared metric"
                );
                if worse {
                    verdict.regressions.push(Regression {
                        metric,
                        baseline: entry.value,
                        measured,
                        limit,
                    });
                }
            }
            (Some(_), None) => verdict.missing.push(metric),
            (None, Some(_)) => verdict.unbaselined.push(metric),
            (None, None) => {}
        }
    }
    Ok(verdict)
}

/// How Capsule stood against the platform app on one metric.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Standing {
    /// Capsule matched or beat the platform app.
    AtLeastAsFast { capsule: f64, platform_app: f64 },
    /// The platform app was faster.
    Behind { capsule: f64, platform_app: f64 },
    /// One side was not measured, so there is nothing to compare.
    NotMeasured {
        capsule: Option<f64>,
        platform_app: Option<f64>,
    },
}

/// Capsule against the platform app, metric by metric, for every metric either side measured.
pub(crate) fn parity(results: &Results) -> Vec<(Metric, Standing)> {
    Metric::ALL
        .into_iter()
        .filter_map(|metric| {
            let capsule = results.get(Client::Capsule, metric);
            let platform_app = results.get(Client::PlatformApp, metric);
            let standing = match (capsule, platform_app) {
                (None, None) => return None,
                (Some(capsule), Some(platform_app)) => {
                    let ahead = match metric.better() {
                        Better::Lower => capsule <= platform_app,
                        Better::Higher => capsule >= platform_app,
                    };
                    if ahead {
                        Standing::AtLeastAsFast {
                            capsule,
                            platform_app,
                        }
                    } else {
                        Standing::Behind {
                            capsule,
                            platform_app,
                        }
                    }
                }
                (capsule, platform_app) => Standing::NotMeasured {
                    capsule,
                    platform_app,
                },
            };
            Some((metric, standing))
        })
        .collect()
}

impl fmt::Display for Standing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let show =
            |value: Option<f64>| value.map_or_else(|| "-".to_string(), |v| format!("{v:.2}"));
        match self {
            Standing::AtLeastAsFast {
                capsule,
                platform_app,
            } => write!(f, "at least as fast ({capsule:.2} vs {platform_app:.2})"),
            Standing::Behind {
                capsule,
                platform_app,
            } => write!(f, "behind ({capsule:.2} vs {platform_app:.2})"),
            Standing::NotMeasured {
                capsule,
                platform_app,
            } => write!(
                f,
                "not compared (capsule {}, platform app {})",
                show(*capsule),
                show(*platform_app)
            ),
        }
    }
}

/// Render the verdict and the parity table as the gate's human-readable report.
pub(crate) fn render(verdict: &Verdict, parity: &[(Metric, Standing)]) -> String {
    let mut lines = vec![if verdict.passed() {
        "regression gate: pass".to_string()
    } else {
        "regression gate: FAIL".to_string()
    }];
    lines.extend(verdict.regressions.iter().map(|regression| {
        format!(
            "  regressed {}: {:.2} (baseline {:.2}, limit {:.2})",
            regression.metric.key(),
            regression.measured,
            regression.baseline,
            regression.limit
        )
    }));
    lines.extend(
        verdict
            .missing
            .iter()
            .map(|metric| format!("  no longer measured: {}", metric.key())),
    );
    lines.extend(
        verdict
            .unbaselined
            .iter()
            .map(|metric| format!("  measured without a baseline: {}", metric.key())),
    );
    lines.push("parity with the platform photo app (reported, not gating):".to_string());
    lines.extend(
        parity
            .iter()
            .map(|(metric, standing)| format!("  {}: {standing}", metric.key())),
    );
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut text = serde_json::to_string_pretty(value).context("serializing")?;
    text.push('\n');
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

/// Parsed `--flag value` pairs; a repeated, unknown or valueless flag is an error.
fn flags(args: impl Iterator<Item = String>, allowed: &[&str]) -> Result<BTreeMap<String, String>> {
    let mut parsed = BTreeMap::new();
    let mut args = args;
    while let Some(flag) = args.next() {
        if !allowed.contains(&flag.as_str()) {
            bail!("unknown argument `{flag}`; {USAGE}");
        }
        let value = args
            .next()
            .with_context(|| format!("{flag} needs a value"))?;
        if parsed.insert(flag.clone(), value).is_some() {
            bail!("{flag} given twice");
        }
    }
    Ok(parsed)
}

fn required<'a>(flags: &'a BTreeMap<String, String>, flag: &str) -> Result<&'a str> {
    flags
        .get(flag)
        .map(String::as_str)
        .with_context(|| format!("{flag} is required; {USAGE}"))
}

/// `xtask bench-apps ...`.
pub(crate) fn run(mut args: impl Iterator<Item = String>) -> Result<()> {
    match args.next().as_deref() {
        Some("import") => {
            let flags = flags(
                args,
                &["--format", "--input", "--device", "--library-size", "--out"],
            )?;
            let format = import::Format::parse(required(&flags, "--format")?)?;
            let input = PathBuf::from(required(&flags, "--input")?);
            let device = required(&flags, "--device")?;
            let raw_size = required(&flags, "--library-size")?;
            let library_size = raw_size
                .parse::<u64>()
                .with_context(|| format!("--library-size `{raw_size}` is not a count"))?;
            let out = PathBuf::from(required(&flags, "--out")?);
            let report: serde_json::Value = read_json(&input)?;
            let results = import::import(format, &report, device, library_size)?;
            write_json(&out, &results)?;
            info!(
                format = ?format,
                device,
                library_size,
                out = %out.display(),
                "bench-apps imported results"
            );
            Ok(())
        }
        Some("compare") => {
            let flags = flags(args, &["--results", "--baseline"])?;
            let results: Results = read_json(Path::new(required(&flags, "--results")?))?;
            let baseline_path = PathBuf::from(required(&flags, "--baseline")?);
            if !baseline_path.exists() {
                bail!(
                    "no baseline recorded for device `{}` at {}; record one from a trusted run \
                     with `xtask bench-apps record --results <file> --baseline {}`",
                    results.device,
                    baseline_path.display(),
                    baseline_path.display()
                );
            }
            let baseline: Baseline = read_json(&baseline_path)?;
            let verdict = compare(&baseline, &results)?;
            print!("{}", render(&verdict, &parity(&results)));
            info!(
                device = %results.device,
                passed = verdict.passed(),
                regressions = verdict.regressions.len(),
                missing = verdict.missing.len(),
                "bench-apps compared results"
            );
            if verdict.passed() {
                Ok(())
            } else {
                bail!("responsiveness regressed past the recorded baseline")
            }
        }
        Some("record") => {
            let flags = flags(args, &["--results", "--baseline", "--tolerance-pct"])?;
            let results: Results = read_json(Path::new(required(&flags, "--results")?))?;
            let baseline_path = PathBuf::from(required(&flags, "--baseline")?);
            let tolerance_pct = match flags.get("--tolerance-pct") {
                Some(raw) => raw
                    .parse::<f64>()
                    .with_context(|| format!("--tolerance-pct `{raw}` is not a number"))?,
                None => DEFAULT_TOLERANCE_PCT,
            };
            let previous = if baseline_path.exists() {
                Some(read_json::<Baseline>(&baseline_path)?)
            } else {
                None
            };
            let baseline = record(&results, previous.as_ref(), tolerance_pct)?;
            write_json(&baseline_path, &baseline)?;
            info!(
                device = %baseline.device,
                metrics = baseline.metrics.len(),
                baseline = %baseline_path.display(),
                "bench-apps recorded baseline"
            );
            Ok(())
        }
        Some(other) => bail!("unknown bench-apps command `{other}`; {USAGE}"),
        None => bail!("{USAGE}"),
    }
}

#[cfg(test)]
mod tests;
