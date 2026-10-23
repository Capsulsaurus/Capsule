//! `capsule show` — what an imported asset's signed sidecar actually records.
//!
//! The importer folds a third-party export's caption, favourite, album membership, capture
//! time and GPS into each asset's **signed** sidecar, and every one of those mappings
//! is lossy by design: a favourite becomes five stars, an album becomes a tag. Nothing in the
//! CLI printed the result back, so a user could not check a mapping they might disagree with
//! until correcting it cost a signed `metadata-update` per asset — and the migration guide had
//! to say so instead of instructing the check. This verb is that check.
//!
//! It reads the sidecar and the provenance chain off [`Workspace::asset`] and writes nothing of
//! its own; the only write on its path is the default-album resolution every library verb
//! shares when it opens the workspace. No index is queried. The shape mirrors `cull.rs` — a
//! selector → [`resolve`] → [`collect`] into a plain [`AssetView`] → [`render`] — so the
//! projection and the rendering are unit-testable without a `clap` round trip or a spawned
//! binary.
//!
//! ## Naming an asset
//!
//! Nothing `capsule import` prints is an asset id, but a user following the migration guide
//! already holds the **source hashes** from its spot-hash step, and Capsule imports bytes
//! unchanged — so the sidecar's `hash` is the source file's SHA-256. The positional therefore
//! accepts either an asset id or a hex prefix of that hash (at least [`MIN_HASH_PREFIX`]
//! characters); a prefix matching several assets is refused with the count rather than
//! guessed at. There is no source-filename arm because the sidecar stores no filename.
//!
//! Every absent field is printed as *unset* rather than omitted: an absent caption is a fact
//! the user came to verify, not a row to hide. A fix is printed with its datum, because a
//! GCJ-02 coordinate is stored verbatim and would otherwise read as WGS-84.
//!
//! The capture time and fix printed are the **effective** ones — a user's correction when one
//! is recorded. A corrected asset also prints the as-imported value on the row after it, so
//! what the original said stays visible beside what the user chose.

use std::fmt::Write as _;

use capsule_core::domain::GpsDatum;
use capsule_core::sidecar::{CullFlag, Gps, GpsSource, StackRole};
use capsule_library::lifecycle::{AssetState, Workspace};
use colored::Colorize as _;
use thiserror::Error;
use uuid::Uuid;

/// The shortest hash prefix the selector accepts. Eight hex digits is 32 bits — ample against
/// a personal library, and short enough to type from a `shasum` listing.
pub const MIN_HASH_PREFIX: usize = 8;

/// Why a selector did not resolve to exactly one asset.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ShowError {
    /// Neither an asset id nor a hash prefix matched anything in the library.
    ///
    /// The `Display` strings on this enum are developer-facing (the selector and the count,
    /// nothing else); the user sees [`describe_error`]'s sentence.
    #[error("unknown: {0}")]
    UnknownAsset(String),
    /// A hash prefix matched more than one asset. Refused rather than guessed: printing the
    /// wrong asset's metadata under a selector the user believes is unique would defeat the
    /// verification this command exists for.
    #[error("ambiguous: {selector} ({count})")]
    Ambiguous {
        /// The prefix as given.
        selector: String,
        /// How many assets share it.
        count: usize,
    },
    /// The selector is neither a UUID nor a long-enough hex prefix.
    #[error("invalid: {0}")]
    InvalidSelector(String),
}

/// A location fix as the sidecar stores it: coordinates in their own datum, plus where the
/// fix came from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fix {
    /// Latitude, in `datum`.
    pub lat: f64,
    /// Longitude, in `datum`.
    pub lon: f64,
    /// Provenance of the fix.
    pub source: GpsSource,
    /// The datum the coordinates are expressed in — stored verbatim, never converted.
    pub datum: GpsDatum,
}

impl From<&Gps> for Fix {
    fn from(gps: &Gps) -> Self {
        Self {
            lat: gps.lat,
            lon: gps.lon,
            source: gps.source,
            datum: gps.datum,
        }
    }
}

/// The sidecar projection `capsule show` prints — a plain value so the rendering can be
/// tested against a hand-built one and the collection against a real [`AssetState`].
#[derive(Debug, Clone, PartialEq)]
pub struct AssetView {
    /// The asset id.
    pub asset_id: Uuid,
    /// The owning album.
    pub album_id: Uuid,
    /// The closed content-type string the importer derived from the extension.
    pub content_type: String,
    /// The plaintext SHA-256, lowercase hex — the same digest `shasum -a 256` prints for the
    /// source file.
    pub hash: String,
    /// Pixel dimensions, when EXIF carried them.
    pub dimensions: Option<(u32, u32)>,
    /// The capture instant the asset is dated by, RFC 3339: the user's correction when one is
    /// recorded, else the as-imported value.
    pub capture_timestamp: String,
    /// The as-imported capture instant, present only when a user correction overrides it.
    pub capture_timestamp_imported: Option<String>,
    /// The signed import instant, RFC 3339.
    pub import_timestamp: String,
    /// The caption register's current value.
    pub caption: Option<String>,
    /// The rating register's current value (0–5).
    pub rating: Option<u8>,
    /// User tags, sorted.
    pub tags_user: Vec<String>,
    /// AI tag texts, sorted and deduplicated across model versions.
    pub tags_ai: Vec<String>,
    /// The effective fix, when any: the user's location edit when one is recorded (which may
    /// be "no location"), else the as-imported fix.
    pub gps: Option<Fix>,
    /// Whether a user location edit is recorded, so [`gps_imported`](Self::gps_imported) is
    /// shown beside it.
    pub location_edited: bool,
    /// The as-imported fix, when any.
    pub gps_imported: Option<Fix>,
    /// The culling flag (a never-written register reads as `Neutral`).
    pub cull: CullFlag,
    /// Whether the asset is hidden from default views.
    pub hidden: bool,
    /// Whether the asset is currently in trash — [`Workspace::is_trashed`], the chain replay
    /// the workspace itself applies.
    pub in_trash: bool,
    /// Stack placement, when the asset is a stack member: `(stack_id, role)`.
    pub stack: Option<(Uuid, StackRole)>,
    /// Whether a display placeholder (LQIP) is stored.
    pub lqip: bool,
    /// How many signed records the provenance chain holds — the `create` plus every
    /// lifecycle write since (metadata edits, repairs, derivative writes, trash moves).
    pub provenance_records: usize,
}

/// Resolve `selector` — an asset id, or a hex prefix of an asset's content hash — to exactly
/// one asset of `ws`.
#[tracing::instrument(skip(ws))]
pub fn resolve(ws: &Workspace, selector: &str) -> Result<Uuid, ShowError> {
    let candidates = ws
        .asset_ids()
        .into_iter()
        .filter_map(|id| ws.asset(&id).map(|asset| (id, asset.sidecar.hash.to_hex())));
    resolve_among(candidates, selector)
}

/// [`resolve`] over an explicit `(asset id, content hash hex)` listing, so the arms are
/// testable against hand-built candidates.
///
/// A UUID is tried first; if no candidate carries that id and the text also qualifies as a
/// hash prefix (a 32-hex-digit hash prefix parses as a bare UUID), the prefix arm runs before
/// the id is reported unknown.
pub fn resolve_among(
    candidates: impl IntoIterator<Item = (Uuid, String)>,
    selector: &str,
) -> Result<Uuid, ShowError> {
    let selector = selector.trim();
    let as_id = Uuid::parse_str(selector).ok();
    let prefix = selector.to_ascii_lowercase();
    let as_prefix =
        prefix.len() >= MIN_HASH_PREFIX && prefix.bytes().all(|b| b.is_ascii_hexdigit());

    let mut by_prefix: Vec<Uuid> = Vec::new();
    for (id, hash) in candidates {
        if as_id == Some(id) {
            tracing::debug!(asset_id = %id, "show: selector resolved as an asset id");
            return Ok(id);
        }
        if as_prefix && hash.starts_with(&prefix) {
            by_prefix.push(id);
        }
    }

    if !as_prefix {
        return Err(if as_id.is_some() {
            ShowError::UnknownAsset(selector.to_owned())
        } else {
            ShowError::InvalidSelector(selector.to_owned())
        });
    }
    tracing::debug!(
        prefix = %prefix,
        matches = by_prefix.len(),
        "show: selector matched as a hash prefix"
    );
    match by_prefix.as_slice() {
        [] => Err(ShowError::UnknownAsset(selector.to_owned())),
        [id] => Ok(*id),
        many => Err(ShowError::Ambiguous {
            selector: selector.to_owned(),
            count: many.len(),
        }),
    }
}

/// Project a managed asset's in-memory state onto the view. Reads the signed sidecar and the
/// provenance chain only; `None` for an id the workspace does not manage.
#[must_use]
pub fn collect(ws: &Workspace, asset_id: &Uuid) -> Option<AssetView> {
    let asset: &AssetState = ws.asset(asset_id)?;
    let sidecar = &asset.sidecar;
    let mut tags_user: Vec<String> = sidecar.tags_user.value().into_iter().collect();
    tags_user.sort();
    let mut tags_ai: Vec<String> = sidecar
        .tags_ai
        .value()
        .into_iter()
        .map(|tag| tag.tag)
        .collect();
    tags_ai.sort();
    tags_ai.dedup();

    Some(AssetView {
        asset_id: asset.asset_id,
        album_id: asset.album_id,
        content_type: sidecar.content_type.clone(),
        hash: sidecar.hash.to_hex(),
        dimensions: sidecar.dimensions.as_ref().map(|d| (d.width, d.height)),
        capture_timestamp: sidecar.effective_capture_timestamp().to_owned(),
        capture_timestamp_imported: sidecar
            .capture_timestamp_user
            .get()
            .map(|_| sidecar.capture_timestamp.clone()),
        import_timestamp: sidecar.import_timestamp.clone(),
        caption: sidecar.caption.get().cloned(),
        rating: sidecar.rating.get().copied(),
        tags_user,
        tags_ai,
        gps: sidecar.effective_gps().map(Fix::from),
        location_edited: sidecar.gps_user.get().is_some(),
        gps_imported: sidecar.gps.as_ref().map(Fix::from),
        cull: sidecar.cull.get().copied().unwrap_or_default(),
        hidden: sidecar.hidden.get().copied().unwrap_or(false),
        in_trash: ws.is_trashed(asset_id),
        stack: sidecar
            .stack_membership
            .get()
            .and_then(Option::as_ref)
            .map(|m| (m.stack_id, m.role)),
        lqip: sidecar.lqip.is_some(),
        provenance_records: asset.chain.records().len(),
    })
}

/// A human-readable reason for a [`ShowError`], for the failure line.
pub fn describe_error(error: &ShowError) -> String {
    match error {
        ShowError::UnknownAsset(selector) => {
            format!("No asset in this library matches {selector}.")
        }
        ShowError::Ambiguous { selector, count } => format!(
            "{selector} matches {count} assets; give more of the hash, or the full asset id."
        ),
        ShowError::InvalidSelector(selector) => format!(
            "{selector} is neither an asset id nor a hex prefix of at least {MIN_HASH_PREFIX} \
             characters of a content hash."
        ),
    }
}

pub(crate) const fn gps_source_name(source: GpsSource) -> &'static str {
    match source {
        GpsSource::Exif => "EXIF",
        GpsSource::Manual => "manual",
        GpsSource::Derived => "derived",
    }
}

pub(crate) const fn gps_datum_name(datum: GpsDatum) -> &'static str {
    match datum {
        GpsDatum::Wgs84 => "WGS-84",
        GpsDatum::Gcj02 => "GCJ-02",
    }
}

const fn stack_role_name(role: StackRole) -> &'static str {
    match role {
        StackRole::Primary => "primary",
        StackRole::Member => "member",
        StackRole::Proxy => "proxy",
    }
}

const fn cull_name(flag: CullFlag) -> &'static str {
    match flag {
        CullFlag::Pick => "pick",
        CullFlag::Neutral => "neutral",
        CullFlag::Reject => "reject",
    }
}

/// How an absent value is spelled in a row.
const UNSET: &str = "(unset)";

/// Render the view as the lines `capsule show` prints, one labelled row per field, every
/// absent value spelled out as unset.
#[must_use]
pub fn render(view: &AssetView) -> String {
    let header = format!("Asset {}", view.asset_id).green();
    let mut out = format!("{header}\n");
    for (label, value) in rows(view) {
        let _ = writeln!(out, "  {label:<17}{value}");
    }
    out
}

/// The labelled rows [`render`] prints, uncoloured, in order: one per field, every absent
/// value spelled out as unset. The TUI's library browser draws its detail pane from these, so
/// the two never word a field differently.
#[must_use]
pub fn rows(view: &AssetView) -> Vec<(&'static str, String)> {
    let yes_no = |flag: bool| if flag { "yes" } else { "no" }.to_owned();
    let or_unset = |value: Option<String>| value.unwrap_or_else(|| UNSET.to_owned());
    let list = |items: &[String]| {
        if items.is_empty() {
            UNSET.to_owned()
        } else {
            items.join(", ")
        }
    };

    let dimensions = or_unset(
        view.dimensions
            .map(|(width, height)| format!("{width}×{height}")),
    );
    let rating = or_unset(view.rating.map(|stars| format!("{stars}/5")));
    let fix = |fix: Option<Fix>| {
        or_unset(fix.map(|fix| {
            format!(
                "{:.6}, {:.6} ({}, {})",
                fix.lat,
                fix.lon,
                gps_datum_name(fix.datum),
                gps_source_name(fix.source)
            )
        }))
    };
    let gps = fix(view.gps);
    let stack = or_unset(
        view.stack
            .map(|(stack_id, role)| format!("{stack_id} ({})", stack_role_name(role))),
    );
    let lqip = if view.lqip { "present" } else { UNSET }.to_owned();

    // The as-imported rows appear only beside a user edit, so an unedited asset prints exactly
    // the rows it always did.
    let original_capture = view
        .capture_timestamp_imported
        .clone()
        .map(|imported| ("Orig. capture:", imported));
    let original_gps = view
        .location_edited
        .then(|| ("Orig. GPS:", fix(view.gps_imported)));
    [
        Some(("Album:", view.album_id.to_string())),
        Some(("Content type:", view.content_type.clone())),
        Some(("SHA-256:", view.hash.clone())),
        Some(("Dimensions:", dimensions)),
        Some(("Captured:", view.capture_timestamp.clone())),
        original_capture,
        Some(("Imported:", view.import_timestamp.clone())),
        Some(("Caption:", or_unset(view.caption.clone()))),
        Some(("Rating:", rating)),
        Some(("User tags:", list(&view.tags_user))),
        Some(("AI tags:", list(&view.tags_ai))),
        Some(("GPS:", gps)),
        original_gps,
    ]
    .into_iter()
    .flatten()
    .chain([
        ("Cull flag:", cull_name(view.cull).to_owned()),
        ("Hidden:", yes_no(view.hidden)),
        ("In trash:", yes_no(view.in_trash)),
        ("Stack:", stack),
        ("Placeholder:", lqip),
        (
            "Provenance:",
            format!("{} signed record(s)", view.provenance_records),
        ),
    ])
    .collect()
}

#[cfg(test)]
mod tests;
