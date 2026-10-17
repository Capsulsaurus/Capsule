//! `capsule show` — what an imported asset's signed sidecar actually records (slice `S-B18`).
//!
//! The importer folds a third-party export's caption, favourite, album membership, capture
//! time and GPS into each asset's **signed** sidecar (`S-B10`), and every one of those mappings
//! is lossy by design: a favourite becomes five stars, an album becomes a tag. Nothing in the
//! CLI printed the result back, so a user could not check a mapping they might disagree with
//! until correcting it cost a signed `metadata-update` per asset — and the migration guide had
//! to say so instead of instructing the check. This verb is that check.
//!
//! It reads the sidecar and the provenance chain off [`Workspace::asset`] and writes nothing of
//! its own; the only write on its path is the default-album resolution every library verb
//! shares when it opens the workspace. No index is queried. The shape mirrors `cull.rs` — a
//! selector → [`resolve`] → [`collect`] into a plain [`AssetView`] → [`render`] through the
//! catalog — so the projection and the rendering are unit-testable without a `clap` round trip
//! or a spawned binary.
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

use capsule_core::domain::GpsDatum;
use capsule_core::lifecycle::{AssetState, Workspace};
use capsule_core::sidecar::{CullFlag, GpsSource, StackRole};
use capsule_i18n::Bundle;
use colored::Colorize as _;
use thiserror::Error;
use uuid::Uuid;

use crate::i18n::{Value, keys};

/// The shortest hash prefix the selector accepts. Eight hex digits is 32 bits — ample against
/// a personal library, and short enough to type from a `shasum` listing.
pub const MIN_HASH_PREFIX: usize = 8;

/// Why a selector did not resolve to exactly one asset.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ShowError {
    /// Neither an asset id nor a hash prefix matched anything in the library.
    ///
    /// The `Display` strings on this enum are developer-facing (the selector and the count,
    /// nothing else); the user sees [`describe_error`], which goes through the catalog.
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
    /// The signed capture instant, RFC 3339.
    pub capture_timestamp: String,
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
    /// The fix, when any.
    pub gps: Option<Fix>,
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
        capture_timestamp: sidecar.capture_timestamp.clone(),
        import_timestamp: sidecar.import_timestamp.clone(),
        caption: sidecar.caption.get().cloned(),
        rating: sidecar.rating.get().copied(),
        tags_user,
        tags_ai,
        gps: sidecar.gps.as_ref().map(|g| Fix {
            lat: g.lat,
            lon: g.lon,
            source: g.source,
            datum: g.datum,
        }),
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

/// Localize a [`ShowError`] for the failure line.
pub fn describe_error(bundle: &Bundle, error: &ShowError) -> String {
    match error {
        ShowError::UnknownAsset(selector) => bundle.format(
            keys::SHOW_UNKNOWN_ASSET,
            &[("selector", Value::Str(selector))],
        ),
        ShowError::Ambiguous { selector, count } => bundle.format(
            keys::SHOW_AMBIGUOUS,
            &[
                ("selector", Value::Str(selector)),
                ("count", Value::Int(*count as i64)),
            ],
        ),
        ShowError::InvalidSelector(selector) => bundle.format(
            keys::SHOW_INVALID_SELECTOR,
            &[
                ("selector", Value::Str(selector)),
                ("min", Value::Int(MIN_HASH_PREFIX as i64)),
            ],
        ),
    }
}

const fn gps_source_key(source: GpsSource) -> &'static str {
    match source {
        GpsSource::Exif => keys::SHOW_GPS_SOURCE_EXIF,
        GpsSource::Manual => keys::SHOW_GPS_SOURCE_MANUAL,
        GpsSource::Derived => keys::SHOW_GPS_SOURCE_DERIVED,
    }
}

const fn gps_datum_key(datum: GpsDatum) -> &'static str {
    match datum {
        GpsDatum::Wgs84 => keys::SHOW_GPS_DATUM_WGS84,
        GpsDatum::Gcj02 => keys::SHOW_GPS_DATUM_GCJ02,
    }
}

const fn stack_role_key(role: StackRole) -> &'static str {
    match role {
        StackRole::Primary => keys::SHOW_STACK_ROLE_PRIMARY,
        StackRole::Member => keys::SHOW_STACK_ROLE_MEMBER,
        StackRole::Proxy => keys::SHOW_STACK_ROLE_PROXY,
    }
}

const fn cull_key(flag: CullFlag) -> &'static str {
    match flag {
        CullFlag::Pick => keys::CULL_FLAG_PICK,
        CullFlag::Neutral => keys::CULL_FLAG_NEUTRAL,
        CullFlag::Reject => keys::CULL_FLAG_REJECT,
    }
}

/// Render the view as the lines `capsule show` prints, one catalog message per line, every
/// absent value spelled out as unset.
#[must_use]
pub fn render(bundle: &Bundle, view: &AssetView) -> String {
    let unset = bundle.format(keys::SHOW_VALUE_UNSET, &[]);
    let separator = bundle.format(keys::SHOW_VALUE_LIST_SEPARATOR, &[]);
    let yes_no = |flag: bool| {
        bundle.format(
            if flag {
                keys::SHOW_VALUE_YES
            } else {
                keys::SHOW_VALUE_NO
            },
            &[],
        )
    };
    let or_unset = |value: Option<String>| value.unwrap_or_else(|| unset.clone());
    let list = |items: &[String]| {
        if items.is_empty() {
            unset.clone()
        } else {
            items.join(&separator)
        }
    };

    let dimensions = or_unset(view.dimensions.map(|(width, height)| {
        bundle.format(
            keys::SHOW_VALUE_DIMENSIONS,
            &[
                ("width", Value::Int(i64::from(width))),
                ("height", Value::Int(i64::from(height))),
            ],
        )
    }));
    let rating = or_unset(view.rating.map(|stars| {
        bundle.format(
            keys::SHOW_VALUE_RATING,
            &[("stars", Value::Int(i64::from(stars)))],
        )
    }));
    let gps = or_unset(view.gps.map(|fix| {
        let source = bundle.format(gps_source_key(fix.source), &[]);
        let datum = bundle.format(gps_datum_key(fix.datum), &[]);
        bundle.format(
            keys::SHOW_VALUE_GPS,
            &[
                ("lat", Value::Str(&format!("{:.6}", fix.lat))),
                ("lon", Value::Str(&format!("{:.6}", fix.lon))),
                ("datum", Value::Str(&datum)),
                ("source", Value::Str(&source)),
            ],
        )
    }));
    let stack = or_unset(view.stack.map(|(stack_id, role)| {
        let role = bundle.format(stack_role_key(role), &[]);
        bundle.format(
            keys::SHOW_VALUE_STACK,
            &[
                ("stack_id", Value::Str(&stack_id.to_string())),
                ("role", Value::Str(&role)),
            ],
        )
    }));
    let lqip = if view.lqip {
        bundle.format(keys::SHOW_VALUE_PRESENT, &[])
    } else {
        unset.clone()
    };
    let cull = bundle.format(cull_key(view.cull), &[]);

    let header = bundle.format(
        keys::SHOW_HEADER,
        &[("asset_id", Value::Str(&view.asset_id.to_string()))],
    );
    let rows: [(&str, String); 17] = [
        (keys::SHOW_ALBUM, view.album_id.to_string()),
        (keys::SHOW_CONTENT_TYPE, view.content_type.clone()),
        (keys::SHOW_HASH, view.hash.clone()),
        (keys::SHOW_DIMENSIONS, dimensions),
        (keys::SHOW_CAPTURED, view.capture_timestamp.clone()),
        (keys::SHOW_IMPORTED, view.import_timestamp.clone()),
        (keys::SHOW_CAPTION, or_unset(view.caption.clone())),
        (keys::SHOW_RATING, rating),
        (keys::SHOW_TAGS_USER, list(&view.tags_user)),
        (keys::SHOW_TAGS_AI, list(&view.tags_ai)),
        (keys::SHOW_GPS, gps),
        (keys::SHOW_CULL, cull),
        (keys::SHOW_HIDDEN, yes_no(view.hidden)),
        (keys::SHOW_IN_TRASH, yes_no(view.in_trash)),
        (keys::SHOW_STACK, stack),
        (keys::SHOW_LQIP, lqip),
        (
            keys::SHOW_PROVENANCE_RECORDS,
            view.provenance_records.to_string(),
        ),
    ];

    let mut out = format!("{}\n", header.green());
    for (key, value) in rows {
        out.push_str(&bundle.format(key, &[("value", Value::Str(&value))]));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use capsule_core::crypto::primitives::Argon2Params;

    use super::*;

    const FAST_KDF: Argon2Params = Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    };

    /// A fast-cost workspace with `count` imported assets of distinct bytes, in a scratch
    /// directory that is removed on drop.
    struct Fixture {
        dir: std::path::PathBuf,
        ws: Workspace,
        ids: Vec<Uuid>,
    }

    impl Fixture {
        fn with_assets(count: usize) -> Self {
            let dir = std::env::temp_dir().join(format!("capsule-cli-show-{}", nanoid::nanoid!()));
            let lib = dir.join("lib");
            std::fs::create_dir_all(&lib).expect("scratch library dir");
            let mut ws =
                Workspace::create_with_params(&lib, b"pw", FAST_KDF).expect("create workspace");
            let album = ws.default_album_id();
            ws.create_album_with_id(album, "Imports")
                .expect("create album");
            let ids = (0..count)
                .map(|n| {
                    let src = dir.join(format!("photo-{n}.jpg"));
                    let mut bytes = b"\xFF\xD8\xFF".to_vec();
                    bytes.extend_from_slice(format!(" asset {n}").as_bytes());
                    std::fs::write(&src, bytes).expect("fixture file");
                    ws.import_asset(album, &src).expect("import")
                })
                .collect();
            Self { dir, ws, ids }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn bundle() -> Bundle {
        Bundle::for_locale("en")
    }

    /// Three candidates whose hashes share a 10-character prefix, so ambiguity is a fact
    /// about the listing rather than a lottery over real digests.
    fn candidates() -> Vec<(Uuid, String)> {
        let ids = [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()];
        vec![
            (ids[0], format!("0123456789{}", "a".repeat(54))),
            (ids[1], format!("0123456789{}", "b".repeat(54))),
            (ids[2], format!("fedcba9876{}", "c".repeat(54))),
        ]
    }

    #[test]
    fn an_asset_id_resolves_to_itself_in_any_spelling() {
        let list = candidates();
        let id = list[1].0;
        assert_eq!(resolve_among(list.clone(), &id.to_string()), Ok(id));
        assert_eq!(
            resolve_among(list.clone(), &format!("  {}  ", id.simple())),
            Ok(id),
            "the simple (hyphen-less) spelling and surrounding whitespace are tolerated"
        );
        assert_eq!(
            resolve_among(list, &id.to_string().to_ascii_uppercase()),
            Ok(id)
        );
    }

    #[test]
    fn a_hash_prefix_resolves_to_the_one_candidate_carrying_it() {
        let list = candidates();
        assert_eq!(resolve_among(list.clone(), "0123456789a"), Ok(list[0].0));
        assert_eq!(
            resolve_among(list.clone(), "0123456789B"),
            Ok(list[1].0),
            "case-folded"
        );
        assert_eq!(resolve_among(list.clone(), "fedcba98"), Ok(list[2].0));
        let (expected, full) = list[2].clone();
        assert_eq!(resolve_among(list, &full), Ok(expected), "the whole hash");
    }

    #[test]
    fn an_ambiguous_prefix_is_refused_with_the_count() {
        let list = candidates();
        assert_eq!(
            resolve_among(list, "01234567"),
            Err(ShowError::Ambiguous {
                selector: "01234567".into(),
                count: 2
            })
        );
    }

    #[test]
    fn a_short_or_non_hex_selector_is_invalid_and_a_missing_one_is_unknown() {
        let list = candidates();
        assert_eq!(
            resolve_among(list.clone(), "abc"),
            Err(ShowError::InvalidSelector("abc".into()))
        );
        assert_eq!(
            resolve_among(list.clone(), "not-a-hash-or-id"),
            Err(ShowError::InvalidSelector("not-a-hash-or-id".into()))
        );
        assert_eq!(
            resolve_among(list.clone(), "deadbeef00"),
            Err(ShowError::UnknownAsset("deadbeef00".into()))
        );
        let ghost = Uuid::now_v7();
        assert_eq!(
            resolve_among(list, &ghost.to_string()),
            Err(ShowError::UnknownAsset(ghost.to_string()))
        );
    }

    /// A 32-hex-digit hash prefix parses as a bare UUID; it must still reach the prefix arm.
    #[test]
    fn a_thirty_two_digit_hash_prefix_is_not_mistaken_for_an_unknown_id() {
        let list = candidates();
        let prefix = &list[2].1[..32];
        assert!(
            Uuid::parse_str(prefix).is_ok(),
            "the premise: it parses as a UUID"
        );
        assert_eq!(resolve_among(list.clone(), prefix), Ok(list[2].0));
    }

    #[test]
    fn resolve_reads_the_workspaces_sidecar_hashes() {
        let fx = Fixture::with_assets(2);
        for id in &fx.ids {
            let hex = fx.ws.asset(id).expect("asset").sidecar.hash.to_hex();
            assert_eq!(resolve(&fx.ws, &hex[..MIN_HASH_PREFIX]), Ok(*id));
            assert_eq!(resolve(&fx.ws, &id.to_string()), Ok(*id));
        }
    }

    #[test]
    fn collect_projects_the_signed_sidecar_and_the_chain() {
        let fx = Fixture::with_assets(1);
        let id = fx.ids[0];
        let asset = fx.ws.asset(&id).expect("asset");
        let view = collect(&fx.ws, &id).expect("managed");
        assert_eq!(view.asset_id, id);
        assert_eq!(view.album_id, fx.ws.default_album_id());
        assert_eq!(view.content_type, asset.sidecar.content_type);
        assert_eq!(view.hash, asset.sidecar.hash.to_hex());
        assert_eq!(view.capture_timestamp, asset.sidecar.capture_timestamp);
        assert_eq!(view.caption, None);
        assert_eq!(view.rating, None);
        assert!(view.tags_user.is_empty());
        assert_eq!(view.cull, CullFlag::Neutral);
        assert!(!view.hidden);
        assert!(!view.in_trash);
        assert_eq!(view.stack, None);
        assert_eq!(
            collect(&fx.ws, &Uuid::now_v7()),
            None,
            "an unknown id has no view"
        );
        assert!(!view.lqip);
        assert_eq!(view.provenance_records, asset.chain.records().len());
    }

    #[test]
    fn collect_reflects_metadata_edits() {
        let mut fx = Fixture::with_assets(1);
        let id = fx.ids[0];
        let before = collect(&fx.ws, &id).expect("managed").provenance_records;
        fx.ws.set_caption(&id, "On the beach").expect("caption");
        fx.ws.tag_add(&id, "Vacation 2021").expect("tag");
        fx.ws.set_cull(&id, CullFlag::Pick).expect("cull");
        let view = collect(&fx.ws, &id).expect("managed");
        assert_eq!(view.caption.as_deref(), Some("On the beach"));
        assert_eq!(view.tags_user, vec!["Vacation 2021".to_string()]);
        assert_eq!(view.cull, CullFlag::Pick);
        assert_eq!(
            view.provenance_records,
            before + 3,
            "three signed metadata-updates on top of the create"
        );
    }

    fn sample_view() -> AssetView {
        AssetView {
            asset_id: Uuid::nil(),
            album_id: Uuid::max(),
            content_type: "image/jpeg".into(),
            hash: "ab".repeat(32),
            dimensions: Some((8, 8)),
            capture_timestamp: "2019-03-04T05:06:07Z".into(),
            import_timestamp: "2026-09-02T00:00:00Z".into(),
            caption: Some("On the beach".into()),
            rating: Some(5),
            tags_user: vec!["Vacation 2021".into(), "beach".into()],
            tags_ai: vec![],
            gps: Some(Fix {
                lat: 10.0,
                lon: 20.0,
                source: GpsSource::Exif,
                datum: GpsDatum::Wgs84,
            }),
            cull: CullFlag::Neutral,
            hidden: false,
            in_trash: false,
            stack: None,
            lqip: false,
            provenance_records: 1,
        }
    }

    /// Every field the guide asks a user to verify is on the page, and every absent value is
    /// spelled out rather than omitted.
    #[test]
    fn render_prints_every_field_and_spells_out_absent_values() {
        let bundle = bundle();
        let page = render(&bundle, &sample_view());
        for expected in [
            "00000000-0000-0000-0000-000000000000",
            "image/jpeg",
            &"ab".repeat(32),
            "8×8",
            "2019-03-04T05:06:07Z",
            "On the beach",
            "5/5",
            "Vacation 2021, beach",
            "10.000000, 20.000000 (WGS-84, EXIF)",
            "neutral",
        ] {
            assert!(page.contains(expected), "missing {expected:?} in:\n{page}");
        }
        let unset = bundle.format(keys::SHOW_VALUE_UNSET, &[]);
        assert_ne!(unset, keys::SHOW_VALUE_UNSET, "the key is in the catalog");
        // AI tags, stack and LQIP are absent in the sample: three unset rows.
        assert_eq!(page.matches(&unset).count(), 3, "{page}");
        assert_eq!(
            page.lines().count(),
            18,
            "a header plus seventeen rows:\n{page}"
        );
        assert!(!page.contains("cli.show."), "no raw key leaks:\n{page}");
    }

    #[test]
    fn render_names_a_stack_placement_a_gcj02_manual_fix_and_the_flags() {
        let bundle = bundle();
        let stack_id = Uuid::now_v7();
        let view = AssetView {
            gps: Some(Fix {
                lat: 39.9,
                lon: 116.4,
                source: GpsSource::Manual,
                datum: GpsDatum::Gcj02,
            }),
            stack: Some((stack_id, StackRole::Primary)),
            hidden: true,
            in_trash: true,
            lqip: true,
            dimensions: None,
            caption: None,
            ..sample_view()
        };
        let page = render(&bundle, &view);
        assert!(
            page.contains("39.900000, 116.400000 (GCJ-02, manual)"),
            "a non-default datum is named, never silently read as WGS-84:\n{page}"
        );
        assert!(page.contains(&format!("{stack_id} (primary)")), "{page}");
        let yes = bundle.format(keys::SHOW_VALUE_YES, &[]);
        assert_eq!(
            page.matches(&yes).count(),
            2,
            "hidden and in trash:\n{page}"
        );
        let present = bundle.format(keys::SHOW_VALUE_PRESENT, &[]);
        assert!(page.contains(&present), "{page}");
    }

    #[test]
    fn describe_error_localizes_each_variant_with_its_detail() {
        let bundle = bundle();
        let ambiguous = describe_error(
            &bundle,
            &ShowError::Ambiguous {
                selector: "abcdef01".into(),
                count: 3,
            },
        );
        assert!(
            ambiguous.contains("abcdef01") && ambiguous.contains('3'),
            "{ambiguous}"
        );
        let invalid = describe_error(&bundle, &ShowError::InvalidSelector("zz".into()));
        assert!(
            invalid.contains("zz") && invalid.contains(&MIN_HASH_PREFIX.to_string()),
            "{invalid}"
        );
        let unknown = describe_error(&bundle, &ShowError::UnknownAsset("deadbeef".into()));
        assert!(unknown.contains("deadbeef"), "{unknown}");
        for text in [&ambiguous, &invalid, &unknown] {
            assert!(!text.contains("cli.show."), "raw key leaked: {text}");
        }
    }

    /// A swept asset prints the row the catalog already described: the trash fact comes from
    /// `Workspace::is_trashed`, the one place the chain is replayed.
    #[test]
    fn a_swept_asset_shows_in_trash() {
        let mut fx = Fixture::with_assets(1);
        let id = fx.ids[0];
        fx.ws.set_cull(&id, CullFlag::Reject).expect("cull");
        let swept = fx.ws.reject_sweep(30).expect("sweep");
        assert_eq!(swept, vec![id]);
        let view = collect(&fx.ws, &id).expect("a trashed asset is still managed");
        assert!(view.in_trash);
        let bundle = bundle();
        let page = render(&bundle, &view);
        let row = bundle.format(
            keys::SHOW_IN_TRASH,
            &[(
                "value",
                Value::Str(&bundle.format(keys::SHOW_VALUE_YES, &[])),
            )],
        );
        assert!(page.contains(&row), "{page}");
    }
}
