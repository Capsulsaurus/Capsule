//! `capsule repair capture-time` — recover capture timestamps stamped with import time
//! (slice `S-B17`).
//!
//! Before `S-B16`, `extract_exif` could never parse a well-formed `DateTimeOriginal`, so every
//! import wrote its own clock as the asset's capture time — **inside the signed sidecar**, and
//! as the `media/{YYYY}/{YYYY-MM}` shard. Fixing the parser did not fix the data: the wrong
//! value is under signature, a rebuild reconstructs from those same sidecars and would
//! faithfully preserve it, and nothing else ever goes back to the original to ask. This pass
//! does.
//!
//! ## The rule
//!
//! For every managed asset, re-read the original's EXIF and resolve it exactly as the
//! importer does ([`resolve_timezone`] over [`extract_exif`]):
//!
//! - **No resolvable instant** — no EXIF, no `DateTimeOriginal`, or a floating one with
//!   neither an `OffsetTimeOriginal` nor a fix to anchor it — is **skipped**. Nothing is
//!   recoverable, and nothing is known to be broken: the importer would write the same value
//!   today. (A floating time is not treated as UTC here for the same reason the importer does
//!   not: that would be a guess signed as a fact.)
//! - **An instant equal to the recorded one** is skipped: the asset is correct.
//! - **An instant that differs** is *affected*. The comparison is against the sidecar's
//!   capture timestamp only — never against import time — so an asset genuinely imported the
//!   second it was taken is not reported as broken.
//! - **An original that cannot be read** is reported as unreadable, never as "no EXIF".
//! - **An asset in trash** is skipped and counted as its own category before its original is
//!   read: appending an irreversible signed record to an asset the user has decided to
//!   discard is not a repair, and a restored asset is picked up by the next run.
//!
//! By construction the pass is a no-op on a library imported after `S-B16`: that importer
//! already wrote the resolved instant wherever one existed, and where none existed this pass
//! skips. Assets whose capture time came from a Takeout record (`CaptureSource::Folded`) have
//! no EXIF instant of their own and are left alone.
//!
//! ## The write
//!
//! Dry run is the default; `--apply` issues one signed `metadata-update` per affected asset
//! through [`Workspace::set_capture_timestamp`], each an independent write — an interrupted
//! run leaves every completed asset correct and a re-run skips them, because their EXIF now
//! agrees. The media bundle is **not** relocated: the sidecar is authoritative for the date
//! and the month directory is only the shard fixed at import, which the design treats as
//! expected drift after a capture correction rather than as a fault.
//!
//! ## Not covered
//!
//! An asset whose capture time a user deliberately set to something other than its EXIF.
//! No such edit surface exists today — `set_capture_timestamp` is the first — so the
//! question does not arise; once one exists, the pass must skip assets whose chain carries a
//! capture correction that was not itself this repair. Recorded in `SLICES.md` under
//! `S-B17` rather than implemented against a surface that is not there.

use std::path::Path;

use capsule_core::exif::{extract_exif, resolve_timezone};
use capsule_core::lifecycle::Workspace;
use capsule_i18n::Bundle;
use colored::Colorize as _;
use jiff::Timestamp;
use thiserror::Error;
use uuid::Uuid;

use crate::i18n::{Value, keys};

/// One `capsule repair capture-time` invocation, independent of the argument parser.
#[derive(Debug, Clone, Copy, Default)]
pub struct RepairRequest {
    /// Write the corrections. Without it the pass only reports.
    pub apply: bool,
    /// Correct at most this many affected assets in one `--apply` run. Detection still covers
    /// the whole library; the dry-run report is unaffected.
    pub limit: Option<usize>,
}

/// What re-reading one original's EXIF says about its recorded capture time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The recorded value disagrees with the instant recovered from EXIF.
    Affected {
        /// The sidecar's capture timestamp, parsed; `None` when it does not parse.
        recorded: Option<Timestamp>,
        /// The instant the original's EXIF resolves to.
        recovered: Timestamp,
    },
    /// The recorded value equals the recovered instant.
    Agrees,
    /// The original carries no instant the importer could have resolved: nothing to recover.
    NoInstant,
    /// The original could not be read at all.
    Unreadable(String),
}

/// An affected asset: what the sidecar says, and what its original says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Affected {
    /// The asset.
    pub asset_id: Uuid,
    /// The sidecar's capture timestamp as written.
    pub recorded_text: String,
    /// The sidecar's capture timestamp, parsed; `None` when it does not parse.
    pub recorded: Option<Timestamp>,
    /// The instant the original's EXIF resolves to.
    pub recovered: Timestamp,
}

impl Affected {
    /// Seconds from the recorded instant to the recovered one, when the recorded one parses.
    #[must_use]
    pub fn delta_seconds(&self) -> Option<i64> {
        self.recorded
            .map(|recorded| self.recovered.as_second() - recorded.as_second())
    }
}

/// The verdicts over a whole library, affected assets in ascending asset-id order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detection {
    /// How many assets were examined.
    pub scanned: usize,
    /// Assets whose recorded capture time disagrees with their EXIF.
    pub affected: Vec<Affected>,
    /// Assets whose recorded capture time equals their EXIF instant.
    pub agrees: usize,
    /// Assets with no recoverable instant.
    pub no_instant: usize,
    /// Assets whose original could not be read, with the reason.
    pub unreadable: Vec<(Uuid, String)>,
    /// Assets in trash, skipped without reading their originals.
    pub trashed: Vec<Uuid>,
}

/// What one invocation did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepairSummary {
    /// The detection the run acted on.
    pub detection: Detection,
    /// Whether `--apply` was given.
    pub applied: bool,
    /// The assets corrected, in the order they were written. Empty on a dry run.
    pub corrected: Vec<Uuid>,
    /// Whether `--limit` stopped the run before every affected asset was corrected.
    pub limit_reached: bool,
}

/// Why a repair could not complete.
#[derive(Debug, Error)]
pub enum RepairError {
    /// The lifecycle refused a correction. The run stops at the first refusal rather than
    /// continuing over a library whose sealing may be broken; everything corrected before it
    /// stays corrected.
    #[error("correcting {asset_id}: {detail}")]
    Lifecycle {
        /// The asset the write was for.
        asset_id: Uuid,
        /// The lifecycle's own description.
        detail: String,
    },
}

/// Compare one asset's recorded capture timestamp against what its original's EXIF resolves
/// to, applying the importer's own resolution.
#[tracing::instrument(skip_all, fields(original = %original.display()))]
pub fn detect_one(recorded: &str, original: &Path) -> Verdict {
    // Readability is checked before EXIF is asked for, because `extract_exif` reports "no
    // EXIF container" as an empty extract and an unreadable file must not be confused with it.
    if let Err(error) = std::fs::File::open(original) {
        tracing::warn!(%error, "repair: original unreadable");
        return Verdict::Unreadable(error.to_string());
    }
    let exif = match extract_exif(original) {
        Ok(exif) => exif,
        Err(error) => {
            tracing::warn!(%error, "repair: original unreadable while extracting EXIF");
            return Verdict::Unreadable(error.to_string());
        }
    };
    let Some(secs) = resolve_timezone(&exif).capture_utc else {
        tracing::debug!("repair: no resolvable EXIF instant; nothing to recover");
        return Verdict::NoInstant;
    };
    let Ok(recovered) = Timestamp::from_second(secs) else {
        tracing::warn!(
            secs,
            "repair: EXIF instant out of range; treated as unrecoverable"
        );
        return Verdict::NoInstant;
    };
    let parsed = recorded.parse::<Timestamp>().ok();
    if parsed.is_some_and(|recorded| recorded.as_second() == secs) {
        return Verdict::Agrees;
    }
    tracing::debug!(recorded, %recovered, "repair: recorded capture time disagrees with EXIF");
    Verdict::Affected {
        recorded: parsed,
        recovered,
    }
}

/// Examine every managed asset of `ws`.
#[tracing::instrument(skip_all, fields(assets = ws.asset_ids().len()))]
pub fn detect(ws: &Workspace) -> Detection {
    let mut ids = ws.asset_ids();
    ids.sort_unstable();
    let mut detection = Detection::default();
    for id in ids {
        let Some(asset) = ws.asset(&id) else {
            continue;
        };
        let Some(original) = ws.original_path(&id) else {
            continue;
        };
        detection.scanned += 1;
        if ws.is_trashed(&id) {
            tracing::debug!(asset_id = %id, "repair: asset in trash; skipped");
            detection.trashed.push(id);
            continue;
        }
        let recorded_text = asset.sidecar.capture_timestamp.clone();
        match detect_one(&recorded_text, &original) {
            Verdict::Affected {
                recorded,
                recovered,
            } => detection.affected.push(Affected {
                asset_id: id,
                recorded_text,
                recorded,
                recovered,
            }),
            Verdict::Agrees => detection.agrees += 1,
            Verdict::NoInstant => detection.no_instant += 1,
            Verdict::Unreadable(reason) => detection.unreadable.push((id, reason)),
        }
    }
    tracing::info!(
        scanned = detection.scanned,
        affected = detection.affected.len(),
        agrees = detection.agrees,
        no_instant = detection.no_instant,
        unreadable = detection.unreadable.len(),
        trashed = detection.trashed.len(),
        "repair: capture-time detection complete"
    );
    detection
}

/// Correct the affected assets, at most `limit` of them, each as its own signed write.
#[tracing::instrument(skip_all, fields(affected = affected.len(), limit))]
pub fn apply(
    ws: &mut Workspace,
    affected: &[Affected],
    limit: Option<usize>,
) -> Result<Vec<Uuid>, RepairError> {
    let budget = limit.unwrap_or(affected.len());
    let mut corrected = Vec::new();
    for item in affected.iter().take(budget) {
        ws.set_capture_timestamp(&item.asset_id, item.recovered)
            .map_err(|error| RepairError::Lifecycle {
                asset_id: item.asset_id,
                detail: error.to_string(),
            })?;
        corrected.push(item.asset_id);
    }
    Ok(corrected)
}

/// Detect, then (with `--apply`) correct.
pub fn run(ws: &mut Workspace, request: RepairRequest) -> Result<RepairSummary, RepairError> {
    let detection = detect(ws);
    let mut summary = RepairSummary {
        applied: request.apply,
        ..Default::default()
    };
    if request.apply {
        summary.corrected = apply(ws, &detection.affected, request.limit)?;
        summary.limit_reached = summary.corrected.len() < detection.affected.len();
    }
    summary.detection = detection;
    Ok(summary)
}

/// Localize a [`RepairError`] for the failure line.
pub fn describe_error(bundle: &Bundle, error: &RepairError) -> String {
    match error {
        RepairError::Lifecycle { asset_id, detail } => bundle.format(
            keys::REPAIR_CAPTURE_TIME_FAILED_ASSET,
            &[
                ("asset_id", Value::Str(&asset_id.to_string())),
                ("reason", Value::Str(detail)),
            ],
        ),
    }
}

/// Render the run as the lines `capsule repair capture-time` prints.
#[must_use]
pub fn render(bundle: &Bundle, request: RepairRequest, summary: &RepairSummary) -> String {
    let detection = &summary.detection;
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };

    line(
        bundle
            .format(
                keys::REPAIR_CAPTURE_TIME_CHECKED,
                &[("count", Value::Int(detection.scanned as i64))],
            )
            .cyan()
            .to_string(),
    );
    if !request.apply {
        line(
            bundle
                .format(keys::REPAIR_CAPTURE_TIME_DRY_RUN_NOTICE, &[])
                .yellow()
                .to_string(),
        );
    }

    for item in &detection.affected {
        let recovered = item.recovered.to_string();
        let asset_id = item.asset_id.to_string();
        line(match item.delta_seconds() {
            Some(delta) => bundle.format(
                keys::REPAIR_CAPTURE_TIME_ROW,
                &[
                    ("asset_id", Value::Str(&asset_id)),
                    ("recorded", Value::Str(&item.recorded_text)),
                    ("recovered", Value::Str(&recovered)),
                    ("delta", Value::Int(delta)),
                ],
            ),
            None => bundle.format(
                keys::REPAIR_CAPTURE_TIME_ROW_UNPARSEABLE,
                &[
                    ("asset_id", Value::Str(&asset_id)),
                    ("recorded", Value::Str(&item.recorded_text)),
                    ("recovered", Value::Str(&recovered)),
                ],
            ),
        });
    }
    for (asset_id, reason) in &detection.unreadable {
        line(
            bundle
                .format(
                    keys::REPAIR_CAPTURE_TIME_UNREADABLE,
                    &[
                        ("asset_id", Value::Str(&asset_id.to_string())),
                        ("reason", Value::Str(reason)),
                    ],
                )
                .red()
                .to_string(),
        );
    }
    for asset_id in &summary.corrected {
        line(
            bundle
                .format(
                    keys::REPAIR_CAPTURE_TIME_CORRECTED,
                    &[("asset_id", Value::Str(&asset_id.to_string()))],
                )
                .green()
                .to_string(),
        );
    }
    if summary.limit_reached {
        line(
            bundle
                .format(
                    keys::REPAIR_CAPTURE_TIME_LIMIT_NOTICE,
                    &[("corrected", Value::Int(summary.corrected.len() as i64))],
                )
                .yellow()
                .to_string(),
        );
    }

    if !detection.trashed.is_empty() {
        line(
            bundle
                .format(
                    keys::REPAIR_CAPTURE_TIME_TRASHED,
                    &[("count", Value::Int(detection.trashed.len() as i64))],
                )
                .yellow()
                .to_string(),
        );
    }

    if detection.affected.is_empty() && detection.unreadable.is_empty() {
        line(
            bundle
                .format(keys::REPAIR_CAPTURE_TIME_NOTHING, &[])
                .green()
                .to_string(),
        );
    } else {
        line(bundle.format(
            keys::REPAIR_CAPTURE_TIME_SUMMARY,
            &[
                ("affected", Value::Int(detection.affected.len() as i64)),
                ("corrected", Value::Int(summary.corrected.len() as i64)),
                ("agrees", Value::Int(detection.agrees as i64)),
                ("no_instant", Value::Int(detection.no_instant as i64)),
                ("unreadable", Value::Int(detection.unreadable.len() as i64)),
                ("trashed", Value::Int(detection.trashed.len() as i64)),
            ],
        ));
    }
    if !summary.corrected.is_empty() {
        line(
            bundle
                .format(keys::REPAIR_CAPTURE_TIME_DRIFT_NOTICE, &[])
                .dimmed()
                .to_string(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use capsule_core::crypto::primitives::Argon2Params;
    use capsule_core::crypto::verify_asset::VerifyOutcome;

    use super::*;

    const FAST_KDF: Argon2Params = Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    };

    /// The instant `exif_jpeg` carries: 2019-03-04 05:06:07 at +00:00.
    const EXIF_SECS: i64 = 1_551_675_967;

    /// A JPEG container holding one EXIF APP1 segment with `DateTimeOriginal` and, when
    /// `with_offset`, `OffsetTimeOriginal` +00:00 — the pair the importer resolves to an
    /// instant. Without the offset the time is floating, which the importer (and so this
    /// pass) does not resolve.
    fn exif_jpeg(with_offset: bool, salt: &[u8]) -> Vec<u8> {
        const DTO: &[u8] = b"2019:03:04 05:06:07\0";
        const OTO: &[u8] = b"+00:00\0";
        let entries: u32 = if with_offset { 2 } else { 1 };
        let ifd0_at: u32 = 8;
        let exif_ifd_at = ifd0_at + 2 + 12 + 4;
        let data_at = exif_ifd_at + 2 + entries * 12 + 4;
        let dto_at = data_at;
        let oto_at = dto_at + DTO.len() as u32;

        fn entry(tiff: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: [u8; 4]) {
            tiff.extend_from_slice(&tag.to_be_bytes());
            tiff.extend_from_slice(&kind.to_be_bytes());
            tiff.extend_from_slice(&count.to_be_bytes());
            tiff.extend_from_slice(&value);
        }

        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"MM");
        tiff.extend_from_slice(&42u16.to_be_bytes());
        tiff.extend_from_slice(&ifd0_at.to_be_bytes());
        tiff.extend_from_slice(&1u16.to_be_bytes());
        entry(&mut tiff, 0x8769, 4, 1, exif_ifd_at.to_be_bytes());
        tiff.extend_from_slice(&0u32.to_be_bytes());
        tiff.extend_from_slice(&(entries as u16).to_be_bytes());
        entry(&mut tiff, 0x9003, 2, DTO.len() as u32, dto_at.to_be_bytes());
        if with_offset {
            entry(&mut tiff, 0x9011, 2, OTO.len() as u32, oto_at.to_be_bytes());
        }
        tiff.extend_from_slice(&0u32.to_be_bytes());
        assert_eq!(tiff.len() as u32, data_at);
        tiff.extend_from_slice(DTO);
        if with_offset {
            tiff.extend_from_slice(OTO);
        }

        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1];
        jpeg.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        jpeg.extend_from_slice(&app1);
        jpeg.extend_from_slice(&[0xFF, 0xFE]);
        jpeg.extend_from_slice(&((salt.len() + 2) as u16).to_be_bytes());
        jpeg.extend_from_slice(salt);
        jpeg.extend_from_slice(&[0xFF, 0xD9]);
        jpeg
    }

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("capsule-cli-repair-{}", nanoid::nanoid!()));
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Self(dir)
        }

        fn file(&self, name: &str, bytes: &[u8]) -> std::path::PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, bytes).expect("fixture file");
            path
        }

        fn workspace(&self) -> Workspace {
            let lib = self.0.join("lib");
            std::fs::create_dir_all(&lib).expect("library dir");
            let mut ws =
                Workspace::create_with_params(&lib, b"pw", FAST_KDF).expect("create workspace");
            let album = ws.default_album_id();
            ws.create_album_with_id(album, "Imports")
                .expect("create album");
            ws
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_second(secs).expect("in range")
    }

    // ── detect_one ───────────────────────────────────────────────────────────

    #[test]
    fn an_agreeing_timestamp_is_not_affected() {
        let scratch = Scratch::new();
        let original = scratch.file("a.jpg", &exif_jpeg(true, b"a"));
        assert_eq!(
            detect_one(&ts(EXIF_SECS).to_string(), &original),
            Verdict::Agrees
        );
    }

    #[test]
    fn a_recorded_import_time_is_affected_with_the_exif_instant_recovered() {
        let scratch = Scratch::new();
        let original = scratch.file("a.jpg", &exif_jpeg(true, b"a"));
        let now = ts(Timestamp::now().as_second());
        assert_eq!(
            detect_one(&now.to_string(), &original),
            Verdict::Affected {
                recorded: Some(now),
                recovered: ts(EXIF_SECS),
            }
        );
        // An unparseable record is affected too, with nothing to compute a delta from.
        assert_eq!(
            detect_one("not a timestamp", &original),
            Verdict::Affected {
                recorded: None,
                recovered: ts(EXIF_SECS),
            }
        );
    }

    /// The importer's own rule: a floating `DateTimeOriginal` resolves to no instant, so the
    /// pass has nothing to compare and must not guess UTC.
    #[test]
    fn a_floating_exif_time_or_no_exif_is_no_instant_not_affected() {
        let scratch = Scratch::new();
        let floating = scratch.file("floating.jpg", &exif_jpeg(false, b"f"));
        assert_eq!(
            detect_one(&Timestamp::now().to_string(), &floating),
            Verdict::NoInstant
        );
        let plain = scratch.file("plain.jpg", b"\xFF\xD8\xFF no exif at all");
        assert_eq!(
            detect_one(&Timestamp::now().to_string(), &plain),
            Verdict::NoInstant
        );
    }

    #[test]
    fn a_missing_original_is_unreadable_not_no_instant() {
        let scratch = Scratch::new();
        let missing = scratch.0.join("gone.jpg");
        assert!(matches!(
            detect_one("2019-03-04T05:06:07Z", &missing),
            Verdict::Unreadable(_)
        ));
    }

    // ── detect / apply over a workspace ──────────────────────────────────────

    /// The `S-B17` acceptance case, in-process: a library whose sidecars carry the wrong
    /// instant reports every affected asset, corrects them under `--apply` as one signed
    /// revision each, and a second pass finds nothing — while a post-`S-B16` import is
    /// untouched from the start.
    #[test]
    fn detect_and_apply_correct_only_the_assets_that_disagree_with_their_exif() {
        let scratch = Scratch::new();
        let mut ws = scratch.workspace();
        let album = ws.default_album_id();
        let good = ws
            .import_asset(album, &scratch.file("good.jpg", &exif_jpeg(true, b"good")))
            .expect("import");
        let broken = ws
            .import_asset(
                album,
                &scratch.file("broken.jpg", &exif_jpeg(true, b"broken")),
            )
            .expect("import");
        let floating = ws
            .import_asset(
                album,
                &scratch.file("floating.jpg", &exif_jpeg(false, b"floating")),
            )
            .expect("import");

        // A post-S-B16 import already carries the EXIF instant: a no-op by construction.
        assert_eq!(
            ws.asset(&good).expect("asset").sidecar.capture_timestamp,
            ts(EXIF_SECS).to_string()
        );
        let clean = detect(&ws);
        assert_eq!(clean.scanned, 3);
        assert!(clean.affected.is_empty(), "{clean:?}");
        assert_eq!((clean.agrees, clean.no_instant), (2, 1));

        // Reproduce the pre-S-B16 state on one asset: its sidecar says "now".
        let wrong = Timestamp::now();
        ws.set_capture_timestamp(&broken, wrong).expect("stamp");

        let detection = detect(&ws);
        assert_eq!(detection.affected.len(), 1);
        let item = &detection.affected[0];
        assert_eq!(item.asset_id, broken);
        assert_eq!(item.recovered, ts(EXIF_SECS));
        assert_eq!(item.recorded_text, wrong.to_string());
        assert_eq!(
            item.delta_seconds(),
            Some(EXIF_SECS - wrong.as_second()),
            "the delta is recovered minus recorded"
        );

        // A dry run writes nothing.
        let dry = run(&mut ws, RepairRequest::default()).expect("dry run");
        assert!(!dry.applied);
        assert!(dry.corrected.is_empty());
        assert_eq!(ws.asset(&broken).expect("asset").chain.records().len(), 2);

        // `--apply` corrects it as a third signed record, and only it.
        let applied = run(
            &mut ws,
            RepairRequest {
                apply: true,
                limit: None,
            },
        )
        .expect("apply");
        assert_eq!(applied.corrected, vec![broken]);
        assert!(!applied.limit_reached);
        let fixed = ws.asset(&broken).expect("asset");
        assert_eq!(fixed.sidecar.capture_timestamp, ts(EXIF_SECS).to_string());
        assert_eq!(fixed.chain.records().len(), 3);
        assert_eq!(ws.verify(&broken).expect("verify"), VerifyOutcome::Accept);
        assert_eq!(ws.asset(&good).expect("asset").chain.records().len(), 1);
        assert_eq!(ws.asset(&floating).expect("asset").chain.records().len(), 1);

        // Idempotent: a second pass has nothing left to do.
        let again = detect(&ws);
        assert!(again.affected.is_empty(), "{again:?}");
        assert_eq!((again.agrees, again.no_instant), (2, 1));
    }

    #[test]
    fn a_limit_stops_after_that_many_corrections_and_says_so() {
        let scratch = Scratch::new();
        let mut ws = scratch.workspace();
        let album = ws.default_album_id();
        let ids: Vec<Uuid> = (0..3)
            .map(|n| {
                let file = scratch.file(
                    &format!("p{n}.jpg"),
                    &exif_jpeg(true, format!("asset {n}").as_bytes()),
                );
                ws.import_asset(album, &file).expect("import")
            })
            .collect();
        for id in &ids {
            ws.set_capture_timestamp(id, Timestamp::now())
                .expect("stamp");
        }

        let first = run(
            &mut ws,
            RepairRequest {
                apply: true,
                limit: Some(2),
            },
        )
        .expect("apply");
        assert_eq!(first.corrected.len(), 2);
        assert!(first.limit_reached);
        assert_eq!(
            first.detection.affected.len(),
            3,
            "detection is not limited"
        );

        let second = run(
            &mut ws,
            RepairRequest {
                apply: true,
                limit: Some(2),
            },
        )
        .expect("apply");
        assert_eq!(second.corrected.len(), 1, "the one the limit left over");
        assert!(!second.limit_reached);
        assert!(detect(&ws).affected.is_empty());
    }

    /// A trashed asset is neither corrected nor counted as affected, whatever its sidecar
    /// says; it is its own category, and a restore brings it back into the next run.
    #[test]
    fn a_trashed_asset_is_skipped_and_counted_not_corrected() {
        let scratch = Scratch::new();
        let mut ws = scratch.workspace();
        let album = ws.default_album_id();
        let trashed = ws
            .import_asset(album, &scratch.file("t.jpg", &exif_jpeg(true, b"trashed")))
            .expect("import");
        ws.set_capture_timestamp(&trashed, Timestamp::now())
            .expect("stamp");
        ws.soft_delete(&trashed, 30).expect("trash");
        let records = ws.asset(&trashed).expect("asset").chain.records().len();

        let summary = run(
            &mut ws,
            RepairRequest {
                apply: true,
                limit: None,
            },
        )
        .expect("apply");
        assert!(summary.detection.affected.is_empty(), "{summary:?}");
        assert_eq!(summary.detection.trashed, vec![trashed]);
        assert_eq!(summary.detection.scanned, 1);
        assert!(summary.corrected.is_empty());
        assert_eq!(
            ws.asset(&trashed).expect("asset").chain.records().len(),
            records,
            "nothing was appended to a trashed asset's chain"
        );

        ws.restore(&trashed).expect("restore");
        let detection = detect(&ws);
        assert_eq!(
            detection.affected.len(),
            1,
            "restored, it is affected again"
        );
        assert!(detection.trashed.is_empty());
    }

    /// The tripwire: `apply` writes exactly `Affected::recovered` and nothing else. A
    /// hand-built `Affected` naming an arbitrary instant lands as that instant, so any change
    /// that made `apply` read a different field would fail here, and the fixture's EXIF
    /// equality in the tests above shows the instant `detect` supplies is the EXIF one.
    #[test]
    fn apply_writes_exactly_the_recovered_instant() {
        let scratch = Scratch::new();
        let mut ws = scratch.workspace();
        let album = ws.default_album_id();
        let id = ws
            .import_asset(album, &scratch.file("a.jpg", &exif_jpeg(true, b"a")))
            .expect("import");
        let arbitrary = ts(1_234_567_890);
        let corrected = apply(
            &mut ws,
            &[Affected {
                asset_id: id,
                recorded_text: "irrelevant".into(),
                recorded: None,
                recovered: arbitrary,
            }],
            None,
        )
        .expect("apply");
        assert_eq!(corrected, vec![id]);
        assert_eq!(
            ws.asset(&id).expect("asset").sidecar.capture_timestamp,
            arbitrary.to_string()
        );
        // And the detect → apply path lands the EXIF instant itself, not merely "a change".
        let affected = detect(&ws).affected;
        apply(&mut ws, &affected, None).expect("apply");
        assert_eq!(
            ws.asset(&id).expect("asset").sidecar.capture_timestamp,
            ts(EXIF_SECS).to_string()
        );
    }

    // ── render ───────────────────────────────────────────────────────────────

    fn sample_summary(apply: bool, corrected: bool) -> RepairSummary {
        let asset_id = Uuid::nil();
        RepairSummary {
            detection: Detection {
                scanned: 4,
                affected: vec![Affected {
                    asset_id,
                    recorded_text: "2026-09-02T10:00:00Z".into(),
                    recorded: Some(ts(1_788_688_800)),
                    recovered: ts(EXIF_SECS),
                }],
                agrees: 2,
                no_instant: 1,
                unreadable: vec![],
                trashed: vec![],
            },
            applied: apply,
            corrected: if corrected { vec![asset_id] } else { vec![] },
            limit_reached: false,
        }
    }

    #[test]
    fn a_dry_run_page_reports_the_affected_asset_and_says_nothing_was_written() {
        let bundle = Bundle::for_locale("en");
        let page = render(
            &bundle,
            RepairRequest::default(),
            &sample_summary(false, false),
        );
        assert!(page.contains("Dry run"), "{page}");
        assert!(page.contains("--apply"), "{page}");
        assert!(page.contains("2026-09-02T10:00:00Z"), "{page}");
        assert!(page.contains("2019-03-04T05:06:07Z"), "{page}");
        assert!(
            page.contains("1 affected, 0 corrected, 2 already correct, 1 without"),
            "{page}"
        );
        assert!(!page.contains("cli.repair."), "no raw key leaks:\n{page}");
    }

    #[test]
    fn an_apply_page_names_the_corrected_asset_and_the_expected_drift() {
        let bundle = Bundle::for_locale("en");
        let request = RepairRequest {
            apply: true,
            limit: None,
        };
        let page = render(&bundle, request, &sample_summary(true, true));
        assert!(!page.contains("Dry run"), "{page}");
        assert!(page.contains(&Uuid::nil().to_string()), "{page}");
        assert!(page.contains("1 affected, 1 corrected"), "{page}");
        let drift = bundle.format(keys::REPAIR_CAPTURE_TIME_DRIFT_NOTICE, &[]);
        assert!(page.contains(&drift), "{page}");
    }

    #[test]
    fn a_clean_library_reports_nothing_to_repair() {
        let bundle = Bundle::for_locale("en");
        let summary = RepairSummary {
            detection: Detection {
                scanned: 2,
                agrees: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        let page = render(&bundle, RepairRequest::default(), &summary);
        let nothing = bundle.format(keys::REPAIR_CAPTURE_TIME_NOTHING, &[]);
        assert!(page.contains(&nothing), "{page}");
        assert!(!page.contains("affected,"), "{page}");
    }
}
