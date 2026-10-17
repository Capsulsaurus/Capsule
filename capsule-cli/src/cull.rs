//! `capsule cull` — the standalone culling command (slice `S-D16`).
//!
//! The photographer's review pass, exposed over the landed culling engine (`S-D13`, in
//! `capsule_core::lifecycle`) and `S-A10`'s durable open plumbing. One invocation runs the
//! design's loop in order:
//!
//! 1. **Flag** — write the trinary `cull` LWW register for the named assets
//!    ([`Workspace::set_cull`]). Never touches bytes and is fully reversible (`--neutral` clears).
//! 2. **Filtered view** — read back the live, non-trashed assets carrying each flag
//!    ([`Workspace::assets_by_cull`]), optionally narrowed to one flag for listing.
//! 3. **Reject sweep** — `--sweep` batch-moves every `reject`-flagged asset to trash
//!    ([`Workspace::reject_sweep`]). The **only** destructive step, and soft per the retention
//!    window, so it is restorable until that window elapses.
//!
//! The view is captured **before** the sweep on purpose: it is the set the sweep acts on, and
//! reporting the post-sweep view would show an empty reject list next to a non-zero sweep count.
//!
//! Because each invocation opens the library afresh, the loop spans processes: flags written by
//! one `capsule cull` run are read back and swept by the next. That durability is `S-A10`'s, and
//! `tests/cull_round_trip.rs` proves it across a real process boundary.
//!
//! SSoT: [Organization — Culling](https://docs/design/organization/#culling).

use capsule_core::lifecycle::Workspace;
use capsule_core::sidecar::CullFlag;
use capsule_i18n::Bundle;
use colored::Colorize as _;
use thiserror::Error;
use uuid::Uuid;

use crate::cli::CullFlagArg;
use crate::i18n::{Value, keys};

/// The retention window a reject sweep stamps when none is given, in days. Matches the design's
/// default trash retention ([Organization — Recycling]).
///
/// [Organization — Recycling]: https://docs/design/organization/#recycling
pub const DEFAULT_RETAIN_DAYS: i64 = 30;

/// One `capsule cull` invocation, independent of the argument parser so the loop is drivable
/// (and testable) without a `clap` round trip.
#[derive(Debug, Clone, Default)]
pub struct CullRequest {
    /// Assets to flag as keepers.
    pub pick: Vec<Uuid>,
    /// Assets whose flag is cleared back to the never-flagged default.
    pub neutral: Vec<Uuid>,
    /// Assets to flag for rejection.
    pub reject: Vec<Uuid>,
    /// Narrow the printed view to one flag, listing its asset ids.
    pub filter: Option<CullFlag>,
    /// Run the reject sweep after flagging.
    pub sweep: bool,
    /// Retention window the sweep's soft delete stamps.
    pub retain_days: i64,
}

/// What one invocation did: the flags written, the filtered view as it stood **before** any
/// sweep, and what the sweep moved to trash.
#[derive(Debug, Clone, Default)]
pub struct CullSummary {
    /// How many `cull` registers this invocation wrote, across all three flags.
    pub flagged: usize,
    /// Live assets flagged `pick`.
    pub picks: Vec<Uuid>,
    /// Live assets carrying no flag (the never-flagged default reads as `neutral`).
    pub neutrals: Vec<Uuid>,
    /// Live assets flagged `reject` — the set a sweep acts on.
    pub rejects: Vec<Uuid>,
    /// Assets the sweep moved to trash. Empty when `--sweep` was not requested.
    pub swept: Vec<Uuid>,
}

/// Why a cull invocation could not complete.
#[derive(Debug, Error)]
pub enum CullError {
    /// A flag was requested for an asset this library does not manage. Refused rather than
    /// ignored: silently dropping the flag would make the command report success for a review
    /// decision that was never recorded.
    #[error("unknown asset {0}")]
    UnknownAsset(Uuid),
    /// The lifecycle refused a flag write or the sweep.
    #[error("{0}")]
    Lifecycle(String),
}

impl From<CullFlagArg> for CullFlag {
    fn from(arg: CullFlagArg) -> Self {
        match arg {
            CullFlagArg::Pick => Self::Pick,
            CullFlagArg::Neutral => Self::Neutral,
            CullFlagArg::Reject => Self::Reject,
        }
    }
}

/// Run the flag → filtered view → reject-sweep loop against an open workspace.
#[tracing::instrument(skip_all, fields(
    pick = request.pick.len(),
    neutral = request.neutral.len(),
    reject = request.reject.len(),
    sweep = request.sweep,
))]
pub fn apply(ws: &mut Workspace, request: &CullRequest) -> Result<CullSummary, CullError> {
    // ── 1. Flag ──
    let mut flagged = 0usize;
    for (flag, ids) in [
        (CullFlag::Pick, &request.pick),
        (CullFlag::Neutral, &request.neutral),
        (CullFlag::Reject, &request.reject),
    ] {
        for id in ids {
            if ws.asset(id).is_none() {
                return Err(CullError::UnknownAsset(*id));
            }
            ws.set_cull(id, flag)
                .map_err(|e| CullError::Lifecycle(e.to_string()))?;
            flagged += 1;
            tracing::debug!(asset_id = %id, ?flag, "cull: flag written");
        }
    }

    // ── 2. Filtered view, as it stands before any sweep ──
    let mut summary = CullSummary {
        flagged,
        picks: ws.assets_by_cull(CullFlag::Pick),
        neutrals: ws.assets_by_cull(CullFlag::Neutral),
        rejects: ws.assets_by_cull(CullFlag::Reject),
        swept: Vec::new(),
    };
    tracing::debug!(
        picks = summary.picks.len(),
        neutrals = summary.neutrals.len(),
        rejects = summary.rejects.len(),
        "cull: filtered view"
    );

    // ── 3. Reject sweep — the only destructive step, and soft per retention ──
    if request.sweep {
        summary.swept = ws
            .reject_sweep(request.retain_days)
            .map_err(|e| CullError::Lifecycle(e.to_string()))?;
        tracing::info!(
            swept = summary.swept.len(),
            retain_days = request.retain_days,
            "cull: reject sweep complete"
        );
    }

    Ok(summary)
}

/// The catalog key naming a flag in prose.
const fn flag_key(flag: CullFlag) -> &'static str {
    match flag {
        CullFlag::Pick => keys::CULL_FLAG_PICK,
        CullFlag::Neutral => keys::CULL_FLAG_NEUTRAL,
        CullFlag::Reject => keys::CULL_FLAG_REJECT,
    }
}

/// The localized name of a flag, for interpolation into the other messages.
fn flag_name(bundle: &Bundle, flag: CullFlag) -> String {
    bundle.format(flag_key(flag), &[])
}

/// Localize a [`CullError`] for the user-facing failure line.
pub fn describe_error(bundle: &Bundle, error: &CullError) -> String {
    match error {
        CullError::UnknownAsset(id) => bundle.format(
            keys::CULL_UNKNOWN_ASSET,
            &[("asset_id", Value::Str(&id.to_string()))],
        ),
        CullError::Lifecycle(detail) => detail.clone(),
    }
}

/// Print what the invocation did: the flags written, the view, and the sweep result.
pub fn render(bundle: &Bundle, request: &CullRequest, summary: &CullSummary) {
    for (flag, ids) in [
        (CullFlag::Pick, &request.pick),
        (CullFlag::Neutral, &request.neutral),
        (CullFlag::Reject, &request.reject),
    ] {
        if ids.is_empty() {
            continue;
        }
        let name = flag_name(bundle, flag);
        println!(
            "{}",
            bundle
                .format(
                    keys::CULL_FLAGGED,
                    &[
                        ("count", Value::Int(ids.len() as i64)),
                        ("flag", Value::Str(&name)),
                    ],
                )
                .green()
        );
    }

    println!(
        "{}",
        bundle
            .format(
                keys::CULL_VIEW,
                &[
                    ("pick", Value::Int(summary.picks.len() as i64)),
                    ("neutral", Value::Int(summary.neutrals.len() as i64)),
                    ("reject", Value::Int(summary.rejects.len() as i64)),
                ],
            )
            .cyan()
    );

    if let Some(filter) = request.filter {
        let ids = match filter {
            CullFlag::Pick => &summary.picks,
            CullFlag::Neutral => &summary.neutrals,
            CullFlag::Reject => &summary.rejects,
        };
        let name = flag_name(bundle, filter);
        if ids.is_empty() {
            println!(
                "{}",
                bundle
                    .format(keys::CULL_FILTER_EMPTY, &[("flag", Value::Str(&name))])
                    .yellow()
            );
        } else {
            println!(
                "{}",
                bundle.format(
                    keys::CULL_FILTER_HEADER,
                    &[
                        ("count", Value::Int(ids.len() as i64)),
                        ("flag", Value::Str(&name)),
                    ],
                )
            );
            for id in ids {
                println!(
                    "{}",
                    bundle.format(keys::CULL_ROW, &[("asset_id", Value::Str(&id.to_string()))])
                );
            }
        }
    }

    if request.sweep {
        if summary.swept.is_empty() {
            println!(
                "{}",
                bundle.format(keys::CULL_NOTHING_TO_SWEEP, &[]).yellow()
            );
        } else {
            println!(
                "{}",
                bundle
                    .format(
                        keys::CULL_SWEPT,
                        &[
                            ("count", Value::Int(summary.swept.len() as i64)),
                            ("retain_days", Value::Int(request.retain_days)),
                        ],
                    )
                    .green()
            );
        }
    }
}
