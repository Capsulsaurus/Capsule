use capsule_core::cbor;
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::crypto::verify_asset::VerifyOutcome;
use capsule_core::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};
use tempfile::TempDir;

use super::super::fast_workspace;
use super::*;
use crate::library::{open_library, rebuild_index};

fn fast_params() -> Argon2Params {
    Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    }
}

fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}

fn int(i: i64) -> Value {
    Value::Integer(i.into())
}

/// A legacy unsigned record exactly as the retired serializer wrote it: text keys, the
/// required fields, `version: 1`, plus `extra` entries appended.
fn legacy_map(asset_id: Uuid, original: &[u8], extra: Vec<(&str, Value)>) -> Value {
    let mut entries = vec![
        ("version", int(1)),
        ("uuid", text(&asset_id.to_string())),
        ("asset_type", text("photo")),
        ("original_filename", text("IMG_0001.JPG")),
        ("import_timestamp", int(1_720_000_000)),
        ("modified_timestamp", int(1_720_000_000)),
        ("hash_sha256", text(&hash::hash_bytes(original).to_hex())),
        ("file_size", int(original.len() as i64)),
        ("is_deleted", Value::Bool(false)),
        ("rating", int(0)),
        ("tags", Value::Array(vec![])),
        ("import_mode", text("copy")),
        ("importer_version", text("0.1.0")),
        ("rawshift_version", text("0.1.0")),
    ];
    entries.extend(extra);
    Value::Map(entries.into_iter().map(|(k, v)| (text(k), v)).collect())
}

fn stack_hint(key: &str, role: &str) -> Value {
    Value::Map(vec![
        (text("detection_key"), text(key)),
        (text("detection_method"), text("filename_stem")),
        (text("member_role"), text(role)),
        (text("stack_type"), text("raw_jpeg")),
    ])
}

/// Write a legacy asset — the original `{uuid}.jpg` and its unsigned `{uuid}.cbor` —
/// into `media/1970/1970-01` (where a `None` capture time landed). Returns the sidecar
/// path and the legacy bytes.
fn write_legacy(
    root: &Path,
    asset_id: Uuid,
    original: &[u8],
    extra: Vec<(&str, Value)>,
) -> (PathBuf, Vec<u8>) {
    write_legacy_in(root, "media/1970/1970-01", "jpg", asset_id, original, extra)
}

/// As [`write_legacy`], into `rel_dir` under `root`, with the original named
/// `{uuid}.{ext}`.
fn write_legacy_in(
    root: &Path,
    rel_dir: &str,
    ext: &str,
    asset_id: Uuid,
    original: &[u8],
    extra: Vec<(&str, Value)>,
) -> (PathBuf, Vec<u8>) {
    let dir = root.join(rel_dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(format!("{}.{ext}", asset_id.simple())), original).unwrap();
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&legacy_map(asset_id, original, extra), &mut bytes).unwrap();
    let path = dir.join(format!("{}.cbor", asset_id.simple()));
    fs::write(&path, &bytes).unwrap();
    (path, bytes)
}

fn opts(fallback: Uuid) -> UnsignedMigrationOptions {
    UnsignedMigrationOptions {
        fallback_album: fallback,
        trash_retain_days: 30,
    }
}

/// Three legacy assets, one of them the "rich" one (rating, tags, GPS) and one carrying a
/// key no build ever defined. Returns `(rich, future, plain)`.
fn three_legacy_assets(root: &Path) -> (Uuid, Uuid, Uuid) {
    let rich = Uuid::from_u128(0xA1);
    let future = Uuid::from_u128(0xA2);
    let plain = Uuid::from_u128(0xA3);
    write_legacy(
        root,
        rich,
        b"\xFF\xD8\xFF rich legacy asset",
        vec![
            ("rating", int(4)),
            ("tags", Value::Array(vec![text("trip"), text("2024")])),
            ("gps_lat", Value::Float(48.8584)),
            ("gps_lon", Value::Float(2.2945)),
            ("capture_utc", int(1_700_000_000)),
        ],
    );
    write_legacy(
        root,
        future,
        b"\xFF\xD8\xFF legacy asset from the future",
        vec![("future_field", text("kept verbatim"))],
    );
    write_legacy(root, plain, b"\xFF\xD8\xFF plain legacy asset", vec![]);
    (rich, future, plain)
}

fn read_signed(ws: &Workspace, id: &Uuid) -> SidecarV1 {
    let bytes = fs::read(ws.sidecar_path(ws.asset(id).unwrap())).unwrap();
    SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap()
}

// ── T1: the acceptance case ─────────────────────────────────────────────

/// **The acceptance case.** After the verb, every legacy asset is a signed asset:
/// `verify_asset` accepts it, its sidecar verifies under the user IK, its chain, sealed
/// metadata blob, and index row exist, and the legacy rating, tags, GPS, and capture time
/// are carried into their signed homes.
#[test]
fn migrated_assets_verify_and_carry_their_legacy_metadata() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Imports").unwrap();
    let (rich, future, plain) = three_legacy_assets(lib.path());

    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert_eq!(report.migrated, vec![rich, future, plain]);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert!(report.trashed.is_empty());
    assert!(report.stacks.is_empty());

    let ik = ws.user_ik_public();
    for id in [rich, future, plain] {
        assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept, "{id}");
        let asset = ws.asset(&id).unwrap();
        assert_eq!(asset.album_id, album);
        assert!(
            asset.sidecar.verify(&ik),
            "sidecar signed under the user IK"
        );
        assert!(ws.provenance_path(asset).exists());
        assert!(ws.metadata_blob_path(asset).exists());
        assert!(!asset.metadata_blob.is_empty());
        assert_eq!(asset.chain.records().len(), 1, "one create record");
        assert!(
            ws.db().find_by_uuid(&id.to_string()).unwrap().is_some(),
            "index row written"
        );
        // The files stayed in their legacy bucket.
        assert_eq!(asset.capture_utc, 0, "1970-01 bucket");
        assert!(ws.media_path(asset).exists());
        // The sidecar's import time is the legacy import time, not now.
        assert_eq!(asset.sidecar.import_timestamp, "2024-07-03T09:46:40Z");
    }

    // The rich record's metadata landed in the signed registers.
    let sidecar = read_signed(&ws, &rich);
    assert_eq!(sidecar.rating.get(), Some(&4));
    let tags = sidecar.tags_user.value();
    assert!(tags.contains("trip") && tags.contains("2024"));
    let gps = sidecar.gps.expect("legacy GPS carried");
    assert_eq!((gps.lat, gps.lon), (48.8584, 2.2945));
    assert_eq!(
        gps.source,
        GpsSource::Manual,
        "a record-held fix is not attributed to these file bytes"
    );
    // The legacy capture time is the sidecar's capture timestamp (these bytes carry no
    // EXIF, so the fold wins over the import clock).
    assert_eq!(sidecar.capture_timestamp, "2023-11-14T22:13:20Z");
    assert_eq!(
        ws.db()
            .find_by_uuid(&rich.to_string())
            .unwrap()
            .unwrap()
            .rating,
        4
    );
    assert_eq!(ws.db().tags_for(&rich.to_string()).unwrap().len(), 2);
    // A record with no capture time falls back to its legacy import time, never `now`.
    assert_eq!(
        read_signed(&ws, &plain).capture_timestamp,
        "2024-07-03T09:46:40Z"
    );
}

// ── T2: the never-strip tripwire ────────────────────────────────────────

/// **The never-strip tripwire.** The whole legacy map — including a key no build has ever
/// defined — rides in the signed sidecar's `_unknown` under [`LEGACY_FOLD_KEY`], and the
/// signature covers it: the fold is byte-equal to the canonicalised legacy map, and
/// removing the fold (or one key inside it) fails `verify`. A fold that dropped
/// `future_field` would fail the equality assertion below.
#[test]
fn the_legacy_map_is_folded_verbatim_and_covered_by_the_signature() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Imports").unwrap();
    let (_, future, _) = three_legacy_assets(lib.path());
    let legacy_bytes = fs::read(
        lib.path()
            .join("media/1970/1970-01")
            .join(format!("{}.cbor", future.simple())),
    )
    .unwrap();

    ws.migrate_unsigned_sidecars(&opts(album)).unwrap();

    let sidecar = read_signed(&ws, &future);
    let fold = sidecar
        .unknown
        .get(LEGACY_FOLD_KEY)
        .expect("the legacy record is folded into _unknown");
    // Byte-equal to the legacy map, canonically re-encoded.
    assert_eq!(
        cbor::value_to_canonical_vec(fold),
        cbor::canonicalize(&legacy_bytes).unwrap(),
        "the fold is the entire legacy map, verbatim"
    );
    let Value::Map(entries) = fold else {
        panic!("the fold is a map");
    };
    assert!(
        entries.contains(&(text("future_field"), text("kept verbatim"))),
        "a key no build defined survives inside the fold"
    );
    assert!(
        entries.iter().any(|(k, _)| *k == text("original_filename")),
        "fields with no signed home of their own survive inside the fold"
    );

    // The signature covers the fold: stripping it, or a key inside it, invalidates it.
    let ik = ws.user_ik_public();
    assert!(sidecar.verify(&ik));
    let mut stripped = sidecar.clone();
    stripped.unknown.remove(LEGACY_FOLD_KEY);
    assert!(
        !stripped.verify(&ik),
        "stripping the fold breaks the signature"
    );
    let mut trimmed = sidecar.clone();
    let Some(Value::Map(inner)) = trimmed.unknown.get_mut(LEGACY_FOLD_KEY) else {
        panic!("fold present")
    };
    inner.retain(|(k, _)| *k != text("future_field"));
    assert!(
        !trimmed.verify(&ik),
        "dropping one legacy key from the fold breaks the signature"
    );
}

// ── T3: bytes preserved ─────────────────────────────────────────────────

/// The legacy bytes survive verbatim in quarantine with a reason file, and are still there
/// after a reopen (the startup scrub deletes only `.tmp` files). The original is signed
/// where it lies: its file is not rewritten, which its unchanged mtime proves.
#[test]
fn legacy_bytes_are_preserved_verbatim_in_quarantine() {
    use std::time::{Duration, SystemTime};

    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Imports").unwrap();
    let id = Uuid::from_u128(0xB1);
    let (sidecar_path, legacy_bytes) =
        write_legacy(lib.path(), id, b"\xFF\xD8\xFF preserved", vec![]);
    let original = lib
        .path()
        .join("media/1970/1970-01")
        .join(format!("{}.jpg", id.simple()));
    let long_ago = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    fs::File::options()
        .write(true)
        .open(&original)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();

    ws.migrate_unsigned_sidecars(&opts(album)).unwrap();

    assert_eq!(
        fs::metadata(&original).unwrap().modified().unwrap(),
        long_ago,
        "the original is signed in place, never rewritten over itself"
    );
    assert_eq!(fs::read(&original).unwrap(), b"\xFF\xD8\xFF preserved");

    let quarantine = lib.path().join(".library/quarantine");
    let twin = quarantine.join(format!("{}.cbor", id.simple()));
    assert_eq!(fs::read(&twin).unwrap(), legacy_bytes, "byte-equal copy");
    let reason: serde_json::Value = serde_json::from_slice(
        &fs::read(quarantine.join(format!("{}.reason.json", id.simple()))).unwrap(),
    )
    .unwrap();
    assert_eq!(reason["reason"], QUARANTINE_REASON);
    assert_eq!(
        reason["sha256_of_legacy_bytes"],
        hash::hash_bytes(&legacy_bytes).to_hex()
    );
    assert!(reason["migrated_at"].is_string());
    // The signed sidecar was written over the legacy one, in place.
    assert_ne!(fs::read(&sidecar_path).unwrap(), legacy_bytes);
    assert!(SidecarV1::from_canonical_slice(&fs::read(&sidecar_path).unwrap(), 1).is_ok());

    drop(ws);
    let _ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    assert_eq!(
        fs::read(&twin).unwrap(),
        legacy_bytes,
        "a reopen leaves it alone"
    );
}

/// **Bucket pinning.** A legacy asset in a later month bucket stays exactly where it is:
/// its `AssetState::capture_utc` is that bucket's first instant, every derived path
/// resolves to the files already there, and the original is neither moved nor rewritten.
#[test]
fn a_legacy_asset_in_a_later_bucket_stays_in_it() {
    use std::time::{Duration, SystemTime};

    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Imports").unwrap();
    let id = Uuid::from_u128(0xB7);
    let (sidecar_path, _) = write_legacy_in(
        lib.path(),
        "media/2024/2024-07",
        "jpg",
        id,
        b"\xFF\xD8\xFF july 2024",
        vec![("capture_utc", int(1_720_000_000))],
    );
    let original = lib
        .path()
        .join("media/2024/2024-07")
        .join(format!("{}.jpg", id.simple()));
    let long_ago = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    fs::File::options()
        .write(true)
        .open(&original)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();

    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert_eq!(report.migrated, vec![id]);
    let asset = ws.asset(&id).unwrap();
    assert_eq!(
        asset.capture_utc, 1_719_792_000,
        "2024-07-01T00:00:00Z, the bucket"
    );
    assert_eq!(
        ws.media_path(asset),
        original,
        "the lifecycle addresses the file in place"
    );
    assert_eq!(ws.sidecar_path(asset), sidecar_path);
    assert!(
        ws.provenance_path(asset)
            .starts_with(lib.path().join("media/2024/2024-07"))
    );
    assert_eq!(
        fs::metadata(&original).unwrap().modified().unwrap(),
        long_ago
    );
    assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept);
    assert_eq!(
        read_signed(&ws, &id).capture_timestamp,
        "2024-07-03T09:46:40Z"
    );
    assert!(
        !lib.path().join("media/1970").exists(),
        "nothing was relocated"
    );
}

/// A sidecar outside a `media/{YYYY}/{YYYY-MM}` bucket, or an original whose extension is
/// not lowercase single-segment, cannot be addressed by the lifecycle's derived paths
/// without moving or renaming it — so it is refused, reported, and left exactly as found.
#[test]
fn a_sidecar_outside_a_month_bucket_or_with_an_odd_extension_is_refused() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Imports").unwrap();

    let loose = Uuid::from_u128(0xB8);
    let (loose_path, loose_bytes) = write_legacy_in(
        lib.path(),
        "media/loose",
        "jpg",
        loose,
        b"\xFF\xD8\xFF loose",
        vec![],
    );
    let shouty = Uuid::from_u128(0xB9);
    let (shouty_path, shouty_bytes) =
        write_legacy(lib.path(), shouty, b"\xFF\xD8\xFF SHOUTY", vec![]);
    let shouty_dir = lib.path().join("media/1970/1970-01");
    fs::rename(
        shouty_dir.join(format!("{}.jpg", shouty.simple())),
        shouty_dir.join(format!("{}.JPG", shouty.simple())),
    )
    .unwrap();

    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert!(report.migrated.is_empty());
    let mut skips = report.skipped.clone();
    skips.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        skips,
        vec![
            (
                shouty_path.clone(),
                MigrationSkip::UnusualExtension {
                    asset_id: shouty,
                    ext: "JPG".to_string(),
                },
            ),
            (
                loose_path.clone(),
                MigrationSkip::OutsideMonthBucket {
                    asset_id: loose,
                    dir: lib.path().join("media/loose"),
                },
            ),
        ]
    );
    assert_eq!(fs::read(&loose_path).unwrap(), loose_bytes);
    assert_eq!(fs::read(&shouty_path).unwrap(), shouty_bytes);
    assert!(
        !lib.path().join(".library/quarantine").exists(),
        "nothing was written"
    );
    assert!(ws.asset(&loose).is_none() && ws.asset(&shouty).is_none());
}

// ── T4: idempotent ──────────────────────────────────────────────────────

/// A second run migrates nothing and changes no bytes; a second rebuild yields the same
/// rows and no duplicate stack members.
#[test]
fn a_second_run_is_a_no_op() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Imports").unwrap();
    let primary = Uuid::from_u128(0xC1);
    let raw = Uuid::from_u128(0xC2);
    write_legacy(
        lib.path(),
        primary,
        b"\xFF\xD8\xFF stack primary",
        vec![("stack_hint", stack_hint("img_0042", "primary"))],
    );
    write_legacy(
        lib.path(),
        raw,
        b"\xFF\xD8\xFF stack raw",
        vec![("stack_hint", stack_hint("img_0042", "raw"))],
    );

    let first = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert_eq!(first.migrated.len(), 2);
    assert_eq!(first.stacks.len(), 1);
    let snapshot = |ws: &Workspace| {
        [primary, raw].map(|id| {
            let asset = ws.asset(&id).unwrap();
            (
                fs::read(ws.sidecar_path(asset)).unwrap(),
                fs::read(ws.provenance_path(asset)).unwrap(),
                asset.chain.records().len(),
            )
        })
    };
    let before = snapshot(&ws);
    let rows_before = ws.db().query_timeline(0, 100).unwrap();

    let second = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert_eq!(
        second,
        UnsignedMigrationReport::default(),
        "nothing left to do"
    );
    assert_eq!(snapshot(&ws), before, "no bytes changed");
    assert!(ws.unmigrated_sidecars().is_empty());

    rebuild_index(&ws.library).unwrap();
    assert_eq!(ws.db().query_timeline(0, 100).unwrap(), rows_before);
    let stack_id = first.stacks[0];
    assert_eq!(
        ws.db()
            .list_stack_members(&stack_id.to_string())
            .unwrap()
            .len(),
        2,
        "no duplicate stack members after repeated rebuilds"
    );
}

/// The receipt and attestation logs share the sidecar's `.cbor` suffix; beside a signed
/// asset they are evidence, never an unanchored sidecar for the verb to pick up.
#[test]
fn the_evidence_logs_beside_a_signed_asset_are_not_unmigrated_sidecars() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("signed.jpg");
    fs::write(&img, b"\xFF\xD8\xFF evidence beside it").unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Imports").unwrap();
    let id = ws.import_asset(album, &img).unwrap();
    let capture = Some(ws.asset(&id).unwrap().capture_utc);
    for path in [
        crate::library::receipts_path(lib.path(), &id, capture),
        crate::library::attestations_path(lib.path(), &id, capture),
    ] {
        fs::write(path, b"not a sidecar").unwrap();
    }

    assert!(unmigrated_sidecars(lib.path()).is_empty());
}

/// The derivative bundle the lifecycle writes as `derivatives/{uuid}.derivatives.cbor`
/// shares the sidecar's `.cbor` suffix too. A library holding one reports no unmigrated
/// sidecar on open, keyless or unlocked, and `rebuild_index` does not read it as one.
#[test]
fn a_derivative_bundle_is_not_an_unmigrated_sidecar() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("signed.jpg");
    fs::write(&img, b"\xFF\xD8\xFF a bundle beside it").unwrap();
    let (id, capture) = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        let id = ws.import_asset(album, &img).unwrap();
        (id, ws.asset(&id).unwrap().capture_utc)
    };
    let dir = super::super::media_dir(lib.path(), capture).join("derivatives");
    fs::create_dir_all(&dir).unwrap();
    // An empty `Vec<DerivativeManifest>`, the bundle's own encoding.
    fs::write(
        dir.join(format!("{}.derivatives.cbor", id.simple())),
        [0x80],
    )
    .unwrap();

    assert!(unmigrated_sidecars(lib.path()).is_empty());
    let ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    assert!(ws.unmigrated_sidecars().is_empty());
    assert_eq!(ws.asset_ids(), vec![id]);
    rebuild_index(&ws.library).unwrap();
    assert_eq!(ws.db().query_timeline(0, 100).unwrap().len(), 1);
}

// ── T5: the open outcome, before and after ──────────────────────────────

/// Before the verb, `Workspace::open` still succeeds on a library holding unsigned
/// sidecars — the signed asset is restored and verifies — and reports the legacy files
/// through `unmigrated_sidecars`. After the verb and a reopen, nothing is left to report
/// and every asset verifies.
#[test]
fn open_reports_unmigrated_sidecars_until_the_verb_runs() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("signed.jpg");
    fs::write(&img, b"\xFF\xD8\xFF a signed asset").unwrap();

    let (album, signed) = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        (album, ws.import_asset(album, &img).unwrap())
    };
    let (rich, future, plain) = three_legacy_assets(lib.path());

    let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    let unmigrated = ws.unmigrated_sidecars();
    assert_eq!(unmigrated.len(), 3);
    let mut ids: Vec<Uuid> = unmigrated.iter().filter_map(|u| u.asset_id).collect();
    ids.sort();
    assert_eq!(ids, vec![rich, future, plain]);
    assert!(
        unmigrated
            .iter()
            .all(|u| u.shape == UnmigratedShape::LegacyUnsigned)
    );
    assert_eq!(
        ws.asset_ids(),
        vec![signed],
        "only the signed asset is restored"
    );
    assert_eq!(ws.verify(&signed).unwrap(), VerifyOutcome::Accept);

    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert_eq!(report.migrated.len(), 3);
    assert!(ws.unmigrated_sidecars().is_empty());
    drop(ws);

    let ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    assert!(ws.unmigrated_sidecars().is_empty());
    let mut all = ws.asset_ids();
    all.sort();
    let mut expected = vec![signed, rich, future, plain];
    expected.sort();
    assert_eq!(all, expected);
    for id in all {
        assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept, "{id}");
    }
}

// ── T5b: the keyless scan ───────────────────────────────────────────────

/// A root with no `media/` directory — a freshly initialised library, or a path that is
/// not a library at all — has nothing unmigrated, and the scan does not error.
#[test]
fn keyless_scan_of_a_library_without_media_is_empty() {
    let lib = TempDir::new().unwrap();
    assert!(unmigrated_sidecars(lib.path()).is_empty());
    assert!(unmigrated_sidecars(&lib.path().join("missing")).is_empty());
}

/// The keyless scan needs no workspace: on a tree holding only legacy files — no keys,
/// no vault, nothing a passphrase could unlock — it still finds every legacy sidecar,
/// with its asset id and shape, in path order.
#[test]
fn keyless_scan_finds_legacy_sidecars_without_any_key_material() {
    let lib = TempDir::new().unwrap();
    let (rich, future, plain) = three_legacy_assets(lib.path());

    let found = unmigrated_sidecars(lib.path());
    let ids: Vec<Uuid> = found.iter().filter_map(|u| u.asset_id).collect();
    assert_eq!(ids, vec![rich, future, plain]);
    assert!(
        found
            .iter()
            .all(|u| u.shape == UnmigratedShape::LegacyUnsigned)
    );
    let mut sorted: Vec<PathBuf> = found.iter().map(|u| u.path.clone()).collect();
    sorted.sort();
    assert_eq!(
        found.iter().map(|u| u.path.clone()).collect::<Vec<_>>(),
        sorted,
        "deterministic path order"
    );
}

/// The keyless scan reports exactly what `Workspace::open` reports: it skips a signed
/// asset its chain anchors and counts an unknown-shape file, and after the verb it
/// finds nothing.
#[test]
fn keyless_scan_agrees_with_open_before_and_after_the_verb() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("signed.jpg");
    fs::write(&img, b"\xFF\xD8\xFF a signed asset").unwrap();

    let album = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        ws.import_asset(album, &img).unwrap();
        album
    };
    three_legacy_assets(lib.path());
    let junk_dir = lib.path().join("media/1970/1970-01");
    let junk = Uuid::from_u128(0xA4);
    fs::write(
        junk_dir.join(format!("{}.cbor", junk.simple())),
        b"not cbor",
    )
    .unwrap();

    let keyless = unmigrated_sidecars(lib.path());
    assert_eq!(keyless.len(), 4, "three legacy plus the unknown file");
    assert!(
        keyless
            .iter()
            .any(|u| u.asset_id == Some(junk) && u.shape == UnmigratedShape::Unknown)
    );

    let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    assert_eq!(keyless.as_slice(), ws.unmigrated_sidecars());

    ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert_eq!(
        unmigrated_sidecars(lib.path()).as_slice(),
        ws.unmigrated_sidecars()
    );
    assert_eq!(
        unmigrated_sidecars(lib.path())
            .iter()
            .filter(|u| u.shape == UnmigratedShape::LegacyUnsigned)
            .count(),
        0,
        "every legacy sidecar was migrated"
    );
}

// ── T6: trash carry-over ────────────────────────────────────────────────

/// A legacy `is_deleted: true` becomes a signed `delete` record with a retention window:
/// the chain is Create then Delete, the asset is in Recently Deleted and out of the
/// timeline. The `delete` write, like the create, leaves the original untouched (its mtime
/// proves it): no path rewrites an original whose bytes already carry the signed hash.
#[test]
fn a_deleted_legacy_asset_lands_in_trash() {
    use std::time::{Duration, SystemTime};

    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Imports").unwrap();
    let gone = Uuid::from_u128(0xD1);
    let kept = Uuid::from_u128(0xD2);
    write_legacy(
        lib.path(),
        gone,
        b"\xFF\xD8\xFF trashed legacy asset",
        vec![
            ("is_deleted", Value::Bool(true)),
            ("deleted_at", int(1_720_100_000)),
        ],
    );
    write_legacy(lib.path(), kept, b"\xFF\xD8\xFF kept legacy asset", vec![]);
    let gone_original = lib
        .path()
        .join("media/1970/1970-01")
        .join(format!("{}.jpg", gone.simple()));
    let long_ago = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    fs::File::options()
        .write(true)
        .open(&gone_original)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();

    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert_eq!(report.trashed, vec![gone]);
    assert_eq!(
        fs::metadata(&gone_original).unwrap().modified().unwrap(),
        long_ago,
        "neither the create nor the delete rewrote the original"
    );
    assert_eq!(
        fs::read(&gone_original).unwrap(),
        b"\xFF\xD8\xFF trashed legacy asset"
    );

    let actions: Vec<Action> = ws
        .asset(&gone)
        .unwrap()
        .chain
        .records()
        .iter()
        .map(|r| r.manifest.core.action)
        .collect();
    assert_eq!(actions, vec![Action::Create, Action::Delete]);
    let delete = &ws.asset(&gone).unwrap().chain.records()[1].manifest.core;
    assert!(
        delete.retention_until.is_some(),
        "the delete carries retention"
    );
    assert_eq!(ws.verify(&gone).unwrap(), VerifyOutcome::Accept);

    let timeline = ws.db().query_timeline(0, 100).unwrap();
    assert_eq!(timeline.len(), 1);
    assert_eq!(timeline[0].uuid, kept.to_string());
    let trash = ws.db().query_trash(0, 100).unwrap();
    assert_eq!(trash.len(), 1);
    assert_eq!(trash[0].uuid, gone.to_string());
}

// ── T7: stack carry-over ────────────────────────────────────────────────

/// Two legacy members sharing a `(filename_stem, img_0042)` hint land under one
/// deterministic stack id, written into each signed `stack_membership` register at
/// create: the timeline collapses to the primary, both are in `stack_members`, and the id
/// is a pure function of the user id and the group key.
#[test]
fn legacy_stack_hints_become_one_signed_stack() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Imports").unwrap();
    let primary = Uuid::from_u128(0xE1);
    let raw = Uuid::from_u128(0xE2);
    let loner = Uuid::from_u128(0xE3);
    write_legacy(
        lib.path(),
        primary,
        b"\xFF\xD8\xFF stack primary",
        vec![("stack_hint", stack_hint("img_0042", "primary"))],
    );
    write_legacy(
        lib.path(),
        raw,
        b"\xFF\xD8\xFF stack raw",
        vec![("stack_hint", stack_hint("img_0042", "raw"))],
    );
    // A hint with no partner: no stack, the hint survives only in the fold.
    write_legacy(
        lib.path(),
        loner,
        b"\xFF\xD8\xFF lonely hint",
        vec![("stack_hint", stack_hint("img_0099", "primary"))],
    );

    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    let expected = legacy_stack_id(&ws.user_id(), "filename_stem", "img_0042");
    assert_eq!(
        report.stacks,
        vec![expected],
        "a pure function of user id + key"
    );
    assert_eq!(expected.get_version_num(), 8, "an RFC 9562 custom (v8) id");

    for (id, role, index) in [
        (primary, StackRole::Primary, 0),
        (raw, StackRole::Member, 1),
    ] {
        let sidecar = read_signed(&ws, &id);
        let membership = sidecar
            .stack_membership
            .get()
            .and_then(Option::as_ref)
            .expect("the register is written at create");
        assert_eq!(membership.stack_id, expected);
        assert_eq!(membership.role, role);
        assert_eq!(membership.member_index, Some(index));
        assert_eq!(membership.stack_type, StackType::RawJpeg);
        assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept);
    }
    assert_eq!(read_signed(&ws, &loner).stack_membership.get(), None);

    let timeline = ws.db().query_timeline(0, 100).unwrap();
    let mut shown: Vec<String> = timeline.iter().map(|r| r.uuid.clone()).collect();
    shown.sort();
    let mut want = vec![primary.to_string(), loner.to_string()];
    want.sort();
    assert_eq!(shown, want, "the raw member is collapsed under the primary");
    assert_eq!(
        ws.db()
            .list_stack_members(&expected.to_string())
            .unwrap()
            .len(),
        2
    );

    // Determinism across libraries: the same user migrating a copy derives the same id.
    let copy = TempDir::new().unwrap();
    let mut ws2 = fast_workspace(copy.path());
    let album2 = ws2.create_album("Imports").unwrap();
    write_legacy(
        copy.path(),
        primary,
        b"\xFF\xD8\xFF stack primary",
        vec![("stack_hint", stack_hint("img_0042", "primary"))],
    );
    write_legacy(
        copy.path(),
        raw,
        b"\xFF\xD8\xFF stack raw",
        vec![("stack_hint", stack_hint("img_0042", "raw"))],
    );
    let report2 = ws2.migrate_unsigned_sidecars(&opts(album2)).unwrap();
    assert_eq!(
        report2.stacks,
        vec![legacy_stack_id(&ws2.user_id(), "filename_stem", "img_0042")]
    );
    assert_ne!(
        report2.stacks, report.stacks,
        "a different user derives a different id"
    );
}

// ── T8: refusals ────────────────────────────────────────────────────────

/// A missing original or a hash mismatch is refused with nothing written — the sidecar
/// bytes, the quarantine directory, and the workspace's assets are all untouched — while
/// an unknown legacy `album_id` lands the asset in the fallback album.
#[test]
fn refusals_write_nothing_and_unknown_albums_fall_back() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Imports").unwrap();

    let orphan = Uuid::from_u128(0xF1);
    let (orphan_path, orphan_bytes) =
        write_legacy(lib.path(), orphan, b"\xFF\xD8\xFF orphaned", vec![]);
    fs::remove_file(
        lib.path()
            .join("media/1970/1970-01")
            .join(format!("{}.jpg", orphan.simple())),
    )
    .unwrap();

    let corrupt = Uuid::from_u128(0xF2);
    let (corrupt_path, corrupt_bytes) =
        write_legacy(lib.path(), corrupt, b"\xFF\xD8\xFF as recorded", vec![]);
    fs::write(
        lib.path()
            .join("media/1970/1970-01")
            .join(format!("{}.jpg", corrupt.simple())),
        b"\xFF\xD8\xFF silently altered",
    )
    .unwrap();

    let stray = Uuid::from_u128(0xF3);
    write_legacy(
        lib.path(),
        stray,
        b"\xFF\xD8\xFF unknown album",
        vec![("album_id", text(&Uuid::from_u128(0xBAD).to_string()))],
    );

    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert_eq!(report.migrated, vec![stray]);
    assert_eq!(ws.asset(&stray).unwrap().album_id, album, "fallback album");

    let mut skips: Vec<(PathBuf, MigrationSkip)> = report.skipped.clone();
    skips.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(skips.len(), 2);
    assert_eq!(
        skips[0],
        (orphan_path.clone(), MigrationSkip::OriginalMissing(orphan))
    );
    assert!(matches!(
        &skips[1].1,
        MigrationSkip::HashMismatch { asset_id, .. } if *asset_id == corrupt
    ));
    assert_eq!(skips[1].0, corrupt_path);

    // Nothing was written for either refusal.
    assert_eq!(fs::read(&orphan_path).unwrap(), orphan_bytes);
    assert_eq!(fs::read(&corrupt_path).unwrap(), corrupt_bytes);
    assert!(ws.asset(&orphan).is_none());
    assert!(ws.asset(&corrupt).is_none());
    let quarantine = lib.path().join(".library/quarantine");
    assert!(
        !quarantine
            .join(format!("{}.cbor", orphan.simple()))
            .exists()
    );
    assert!(
        !quarantine
            .join(format!("{}.cbor", corrupt.simple()))
            .exists()
    );
    // They are still reported as unmigrated, and the closing `rebuild_index` indexed
    // neither: a refusal writes nothing, the index included.
    assert_eq!(ws.unmigrated_sidecars().len(), 2);
    assert!(ws.db().find_by_uuid(&orphan.to_string()).unwrap().is_none());
    assert!(
        ws.db()
            .find_by_uuid(&corrupt.to_string())
            .unwrap()
            .is_none()
    );
}

/// A legacy `album_id` naming an album this device can write into but does not own (a
/// shared album: write tier, no admin capability) is not honoured: the unsigned claim
/// lands the asset in the fallback album, while an owned legacy album is still honoured.
#[test]
fn a_writable_album_the_user_does_not_own_falls_back() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let fallback = ws.create_album("Imports").unwrap();
    let shared = ws.create_album("Shared with me").unwrap();
    let owned = ws.create_album("Mine").unwrap();
    // A shared album: the write capability without the admin capability.
    ws.albums.get_mut(&shared).unwrap().admin = None;
    assert!(ws.albums[&shared].write_tier.is_some(), "still writable");

    let planted = Uuid::from_u128(0xF5);
    write_legacy(
        lib.path(),
        planted,
        b"\xFF\xD8\xFF planted into shared",
        vec![("album_id", text(&shared.to_string()))],
    );
    let mine = Uuid::from_u128(0xF6);
    write_legacy(
        lib.path(),
        mine,
        b"\xFF\xD8\xFF legacy owned",
        vec![("album_id", text(&owned.to_string()))],
    );

    let report = ws.migrate_unsigned_sidecars(&opts(fallback)).unwrap();
    let mut migrated = report.migrated.clone();
    migrated.sort();
    assert_eq!(migrated, vec![planted, mine]);
    assert_eq!(
        ws.asset(&planted).unwrap().album_id,
        fallback,
        "a shared album's unsigned claim falls back"
    );
    assert_eq!(
        ws.asset(&mine).unwrap().album_id,
        owned,
        "an owned legacy album is honoured"
    );
    assert_eq!(ws.verify(&planted).unwrap(), VerifyOutcome::Accept);
}

/// A read-only fallback album (recovered from a backup: content keys, no write
/// capability) is a typed refusal before anything is written.
#[test]
fn a_read_only_fallback_album_is_refused_before_any_write() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Imports").unwrap();
    // Strip the write capability, exactly the shape `import_backup` restores.
    ws.albums.get_mut(&album).unwrap().write_tier = None;
    let id = Uuid::from_u128(0xF4);
    let (path, bytes) = write_legacy(lib.path(), id, b"\xFF\xD8\xFF read-only", vec![]);

    assert!(matches!(
        ws.migrate_unsigned_sidecars(&opts(album)),
        Err(LifecycleError::AlbumReadOnly(a)) if a == album
    ));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(!lib.path().join(".library/quarantine").exists());
    assert!(ws.asset(&id).is_none());

    // An album the workspace does not hold at all is `NotFound`, not a minted album.
    assert!(matches!(
        ws.migrate_unsigned_sidecars(&opts(Uuid::from_u128(0x404))),
        Err(LifecycleError::NotFound(_))
    ));
    assert_eq!(ws.albums().len(), 1);
}

/// A signed asset with the legacy id already restored is refused: migrating over it would
/// replace a signed record with a re-derived one.
#[test]
fn a_colliding_signed_asset_is_refused() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("signed.jpg");
    fs::write(&img, b"\xFF\xD8\xFF a signed asset").unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Imports").unwrap();
    let signed = ws.import_asset(album, &img).unwrap();
    // A legacy sidecar claiming the same id, in a different bucket.
    let (path, bytes) = write_legacy(lib.path(), signed, b"\xFF\xD8\xFF impostor", vec![]);

    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert!(report.migrated.is_empty());
    assert_eq!(
        report.skipped,
        vec![(path.clone(), MigrationSkip::IdCollision(signed))]
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(ws.verify(&signed).unwrap(), VerifyOutcome::Accept);
}

// ── resumability ────────────────────────────────────────────────────────

/// An interrupted run that wrote the signed sidecar but died before the chain leaves a
/// signed sidecar with no `.provenance.cbor` and a quarantine twin. `open` reports it as
/// `SignedWithoutChain`; the next run resumes from the quarantine copy and lands a
/// verifying asset.
#[test]
fn an_interrupted_migration_resumes_from_quarantine() {
    let lib = TempDir::new().unwrap();
    let id = Uuid::from_u128(0x1A);
    let album = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        write_legacy(lib.path(), id, b"\xFF\xD8\xFF interrupted", vec![]);
        ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        // Simulate the crash window: the chain never made it to disk.
        fs::remove_file(ws.provenance_path(ws.asset(&id).unwrap())).unwrap();
        album
    };

    let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    assert!(ws.asset(&id).is_none(), "no chain, not restored");
    assert_eq!(ws.unmigrated_sidecars().len(), 1);
    assert_eq!(
        ws.unmigrated_sidecars()[0].shape,
        UnmigratedShape::SignedWithoutChain { schema: 1 }
    );

    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert_eq!(report.migrated, vec![id]);
    assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept);
    assert!(ws.unmigrated_sidecars().is_empty());
    let sidecar = read_signed(&ws, &id);
    assert!(sidecar.unknown.contains_key(LEGACY_FOLD_KEY));
}

/// A migrated asset that was *edited* after migration and then lost its chain is not an
/// interrupted create: redoing the create would discard the signed edit. It is refused as
/// stranded and its sidecar bytes are left exactly as found.
#[test]
fn a_chainless_sidecar_carrying_a_later_write_is_not_resumed() {
    let lib = TempDir::new().unwrap();
    let id = Uuid::from_u128(0x1B);
    let (album, sidecar_path, edited_bytes) = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        write_legacy(lib.path(), id, b"\xFF\xD8\xFF edited later", vec![]);
        ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        ws.tag_add(&id, "kept").unwrap();
        let path = ws.sidecar_path(ws.asset(&id).unwrap());
        fs::remove_file(ws.provenance_path(ws.asset(&id).unwrap())).unwrap();
        let bytes = fs::read(&path).unwrap();
        (album, path, bytes)
    };

    let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert!(report.migrated.is_empty());
    assert_eq!(
        report.skipped,
        vec![(sidecar_path.clone(), MigrationSkip::Stranded(id))]
    );
    assert_eq!(
        fs::read(&sidecar_path).unwrap(),
        edited_bytes,
        "left as found"
    );
    assert!(
        lib.path()
            .join(".library/quarantine")
            .join(format!("{}.cbor", id.simple()))
            .exists()
    );
}

/// A torn write of the signed sidecar leaves bytes that are neither shape. With the
/// quarantine copy present the run resumes from it; without one, the file is reported as
/// unknown and left alone.
#[test]
fn a_torn_sidecar_resumes_from_quarantine_or_is_reported_unknown() {
    let lib = TempDir::new().unwrap();
    let torn = Uuid::from_u128(0x1C);
    let album = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        write_legacy(lib.path(), torn, b"\xFF\xD8\xFF torn write", vec![]);
        ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        let asset = ws.asset(&torn).unwrap();
        let sidecar = ws.sidecar_path(asset);
        let bytes = fs::read(&sidecar).unwrap();
        fs::write(&sidecar, &bytes[..bytes.len() / 2]).unwrap();
        fs::remove_file(ws.provenance_path(asset)).unwrap();
        album
    };
    // And a garbage sidecar with no twin at all.
    let garbage = Uuid::from_u128(0x1D);
    let garbage_path = lib
        .path()
        .join("media/1970/1970-01")
        .join(format!("{}.cbor", garbage.simple()));
    fs::write(&garbage_path, b"\xFF\x00 not cbor").unwrap();

    let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    assert!(
        ws.unmigrated_sidecars()
            .iter()
            .all(|u| u.shape == UnmigratedShape::Unknown)
    );
    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert_eq!(report.migrated, vec![torn]);
    assert_eq!(ws.verify(&torn).unwrap(), VerifyOutcome::Accept);
    assert_eq!(
        report.skipped,
        vec![(garbage_path.clone(), MigrationSkip::UnknownShape)]
    );
    assert_eq!(fs::read(&garbage_path).unwrap(), b"\xFF\x00 not cbor");
}

/// The signed sidecar replaces the legacy one by a single-file atomic rename: a failure
/// between the `.tmp` write and the rename leaves the legacy sidecar byte-for-byte intact
/// (and its quarantine copy in place), and the next run completes the migration.
#[test]
fn a_failure_before_the_sidecar_rename_leaves_the_legacy_sidecar_intact() {
    use super::super::import::with_sidecar_rename_fault;

    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Imports").unwrap();
    let id = Uuid::from_u128(0x1A7);
    let (sidecar_path, legacy_bytes) =
        write_legacy(lib.path(), id, b"\xFF\xD8\xFF renamed into place", vec![]);

    let failed = with_sidecar_rename_fault(|| ws.migrate_unsigned_sidecars(&opts(album)));
    assert!(
        matches!(failed, Err(LifecycleError::Io(ref m)) if m.contains("injected fault")),
        "{failed:?}"
    );
    assert_eq!(
        fs::read(&sidecar_path).unwrap(),
        legacy_bytes,
        "the legacy sidecar is untouched: only a `.tmp` was written"
    );
    assert!(ws.asset(&id).is_none(), "nothing was admitted");
    assert_eq!(
        fs::read(ws.quarantine_sidecar_path(&id)).unwrap(),
        legacy_bytes,
        "the quarantine copy landed before the signed write started"
    );

    // The rerun finds a legacy sidecar whose quarantine twin matches, and completes.
    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert_eq!(report.migrated, vec![id]);
    assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept);
    assert!(
        !sidecar_path.with_extension("cbor.tmp").exists(),
        "the rename consumed the staged file"
    );
}

/// The chain is staged and renamed into place last: a crash after the sidecar and blob
/// landed but before the chain did leaves no `.provenance.cbor` at all — never a torn one
/// that would count as anchored and hide the asset — so `open` reports a chainless signed
/// sidecar and the next run resumes it into a verifying asset.
#[test]
fn a_failure_before_the_chain_rename_leaves_a_resumable_chainless_sidecar() {
    use super::super::import::{WriteFault, with_write_fault};

    let lib = TempDir::new().unwrap();
    let id = Uuid::from_u128(0x1A8);
    let dir = lib.path().join("media/1970/1970-01");
    let chain_path = dir.join(format!("{}.provenance.cbor", id.simple()));
    let album = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        write_legacy(lib.path(), id, b"\xFF\xD8\xFF chain renamed last", vec![]);
        let failed = with_write_fault(WriteFault::ChainRename, 0, || {
            ws.migrate_unsigned_sidecars(&opts(album))
        });
        assert!(
            matches!(failed, Err(LifecycleError::Io(ref m)) if m.contains("injected fault")),
            "{failed:?}"
        );
        album
    };
    assert!(!chain_path.exists(), "no chain landed, so none is torn");
    assert!(
        dir.join(format!("{}.metadata.bin", id.simple())).exists(),
        "the blob lands before the chain anchors the asset"
    );

    let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    assert!(ws.asset(&id).is_none(), "no chain, not restored");
    assert_eq!(
        ws.unmigrated_sidecars()
            .iter()
            .map(|u| u.shape)
            .collect::<Vec<_>>(),
        vec![UnmigratedShape::SignedWithoutChain { schema: 1 }]
    );

    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert_eq!(report.migrated, vec![id]);
    assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept);
    assert!(ws.unmigrated_sidecars().is_empty());
    assert!(
        !crate::utils::paths::tmp_path(&chain_path).exists(),
        "the rerun's rename consumed the staged chain"
    );
}

/// A run that crashes between two members of one legacy stack leaves the first admitted
/// with its membership and the second still legacy. The resumed run counts the admitted
/// member, so the second lands in the same stack with the index the interrupted run would
/// have given it — not as a group of one that leaves the stack with a single member.
#[test]
fn a_crash_mid_stack_resumes_into_the_full_stack() {
    use super::super::import::{WriteFault, with_write_fault};

    let lib = TempDir::new().unwrap();
    let primary = Uuid::from_u128(0xE7);
    let raw = Uuid::from_u128(0xE8);
    let album = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        write_legacy(
            lib.path(),
            primary,
            b"\xFF\xD8\xFF interrupted stack primary",
            vec![("stack_hint", stack_hint("img_0077", "primary"))],
        );
        write_legacy(
            lib.path(),
            raw,
            b"\xFF\xD8\xFF interrupted stack raw",
            vec![("stack_hint", stack_hint("img_0077", "raw"))],
        );
        // The first sidecar rename (the primary's) succeeds; the second (the raw's) fails.
        let failed = with_write_fault(WriteFault::SidecarRename, 1, || {
            ws.migrate_unsigned_sidecars(&opts(album))
        });
        assert!(
            matches!(failed, Err(LifecycleError::Io(ref m)) if m.contains("injected fault")),
            "{failed:?}"
        );
        album
    };

    let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    assert!(
        ws.asset(&primary).is_some(),
        "the first member was admitted"
    );
    assert!(ws.asset(&raw).is_none(), "the second was not");
    let expected = legacy_stack_id(&ws.user_id(), "filename_stem", "img_0077");

    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert_eq!(report.migrated, vec![raw]);
    assert_eq!(report.stacks, vec![expected]);
    for (id, role, index) in [
        (primary, StackRole::Primary, 0),
        (raw, StackRole::Member, 1),
    ] {
        let sidecar = read_signed(&ws, &id);
        let membership = sidecar
            .stack_membership
            .get()
            .and_then(Option::as_ref)
            .expect("both members carry the stack register");
        assert_eq!(membership.stack_id, expected);
        assert_eq!(membership.role, role);
        assert_eq!(membership.member_index, Some(index));
        assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept);
    }
    assert_eq!(
        ws.db()
            .list_stack_members(&expected.to_string())
            .unwrap()
            .len(),
        2,
        "the stack holds both members, not one"
    );
}

/// The `member_index` of a stack's member, from its signed sidecar.
fn member_index(ws: &Workspace, id: &Uuid) -> Option<u32> {
    read_signed(ws, id)
        .stack_membership
        .get()
        .and_then(Option::as_ref)
        .and_then(|m| m.member_index)
}

/// A member whose sidecar appears only after an earlier run completed its stack without
/// it joins that stack under an index no placed member holds, even where its asset id
/// sorts between theirs; the placed members keep the indices they were signed with.
#[test]
fn a_late_member_joins_a_completed_stack_without_reusing_an_index() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Imports").unwrap();
    let first = Uuid::from_u128(0xF1);
    let late = Uuid::from_u128(0xF2);
    let last = Uuid::from_u128(0xF3);
    write_legacy(
        lib.path(),
        first,
        b"\xFF\xD8\xFF completed stack primary",
        vec![("stack_hint", stack_hint("img_0123", "primary"))],
    );
    write_legacy(
        lib.path(),
        last,
        b"\xFF\xD8\xFF completed stack raw",
        vec![("stack_hint", stack_hint("img_0123", "raw"))],
    );
    ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    let expected = legacy_stack_id(&ws.user_id(), "filename_stem", "img_0123");
    assert_eq!(member_index(&ws, &first), Some(0));
    assert_eq!(member_index(&ws, &last), Some(1));

    // The member skipped then (its sidecar was not there) sorts between the two.
    write_legacy(
        lib.path(),
        late,
        b"\xFF\xD8\xFF late stack member",
        vec![("stack_hint", stack_hint("img_0123", "raw"))],
    );
    drop(ws);
    let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert_eq!(report.migrated, vec![late]);
    assert_eq!(report.stacks, vec![expected]);
    let sidecar = read_signed(&ws, &late);
    let membership = sidecar
        .stack_membership
        .get()
        .and_then(Option::as_ref)
        .expect("the late member joins the stack");
    assert_eq!(membership.stack_id, expected);
    assert_eq!(membership.member_index, Some(2), "after the placed members");
    assert_eq!(member_index(&ws, &first), Some(0), "untouched");
    assert_eq!(member_index(&ws, &last), Some(1), "untouched");
    assert_eq!(
        ws.db()
            .list_stack_members(&expected.to_string())
            .unwrap()
            .len(),
        3
    );
}

/// An earlier-migrated asset sharing the hint but outside the derived stack — admitted as
/// a singleton (no register), or moved by the user to another stack — is not pulled into
/// the stack a later run derives, and its register is left as it was.
#[test]
fn an_earlier_asset_outside_the_derived_stack_is_not_pulled_in() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Imports").unwrap();

    // A singleton: its partner was not there when it was admitted.
    let single = Uuid::from_u128(0xF4);
    write_legacy(
        lib.path(),
        single,
        b"\xFF\xD8\xFF earlier singleton",
        vec![("stack_hint", stack_hint("img_0200", "primary"))],
    );
    // A completed stack whose primary the user then moves to a stack of their own.
    let moved = Uuid::from_u128(0xF5);
    let stayed = Uuid::from_u128(0xF6);
    write_legacy(
        lib.path(),
        moved,
        b"\xFF\xD8\xFF user-moved member",
        vec![("stack_hint", stack_hint("img_0300", "primary"))],
    );
    write_legacy(
        lib.path(),
        stayed,
        b"\xFF\xD8\xFF member left in place",
        vec![("stack_hint", stack_hint("img_0300", "raw"))],
    );
    ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert_eq!(read_signed(&ws, &single).stack_membership.get(), None);
    let derived = legacy_stack_id(&ws.user_id(), "filename_stem", "img_0300");
    for id in [moved, stayed] {
        let sidecar = read_signed(&ws, &id);
        assert_eq!(
            sidecar
                .stack_membership
                .get()
                .and_then(Option::as_ref)
                .map(|m| m.stack_id),
            Some(derived),
            "{id} was placed in the derived stack first"
        );
    }
    let user_stack = StackMembership {
        stack_id: Uuid::from_u128(0xBEEF),
        stack_type: StackType::RawJpeg,
        role: StackRole::Primary,
        member_index: Some(0),
    };
    ws.set_stack_membership(&moved, Some(user_stack.clone()))
        .unwrap();
    // Then the user takes the other member out of the derived stack too.
    ws.set_stack_membership(&stayed, None).unwrap();

    // The singleton's partner, and a newcomer to the stack both earlier members left.
    let partner = Uuid::from_u128(0xF7);
    let newcomer = Uuid::from_u128(0xF8);
    write_legacy(
        lib.path(),
        partner,
        b"\xFF\xD8\xFF singleton's late partner",
        vec![("stack_hint", stack_hint("img_0200", "raw"))],
    );
    write_legacy(
        lib.path(),
        newcomer,
        b"\xFF\xD8\xFF newcomer to a left stack",
        vec![("stack_hint", stack_hint("img_0300", "raw"))],
    );
    drop(ws);
    let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    let mut migrated = report.migrated.clone();
    migrated.sort();
    assert_eq!(migrated, vec![partner, newcomer]);
    assert!(report.stacks.is_empty(), "{:?}", report.stacks);
    for id in [single, partner, stayed, newcomer] {
        let sidecar = read_signed(&ws, &id);
        assert_eq!(
            sidecar.stack_membership.get().and_then(Option::as_ref),
            None,
            "{id} is in no stack"
        );
    }
    assert_eq!(
        read_signed(&ws, &moved).stack_membership.get(),
        Some(&Some(user_stack)),
        "the user's move is respected"
    );
}

/// A crash between the create and the `delete` record leaves an admitted asset whose fold
/// says `is_deleted` but whose chain does not. The next run applies the delete it owes —
/// and never re-deletes an asset the user has since restored from trash by hand.
#[test]
fn an_owed_delete_record_is_applied_on_the_next_run_but_a_restore_is_respected() {
    let lib = TempDir::new().unwrap();
    let owed = Uuid::from_u128(0x1E);
    let restored = Uuid::from_u128(0x1F);
    let album = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        for id in [owed, restored] {
            write_legacy(
                lib.path(),
                id,
                &[b"\xFF\xD8\xFF deleted ".as_slice(), id.as_bytes()].concat(),
                vec![("is_deleted", Value::Bool(true))],
            );
        }
        let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
        assert_eq!(report.trashed, vec![owed, restored]);
        // Simulate the crash window for `owed`: the chain holds the create only.
        let asset = ws.asset(&owed).unwrap();
        let create_only = cbor::to_canonical_vec(&vec![asset.chain.records()[0].clone()]).unwrap();
        fs::write(ws.provenance_path(asset), create_only).unwrap();
        // The user restores `restored` by hand.
        ws.restore(&restored).unwrap();
        album
    };

    let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    assert!(ws.unmigrated_sidecars().is_empty(), "both are anchored");
    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert!(report.migrated.is_empty());
    assert_eq!(report.trashed, vec![owed], "the owed delete, and only that");
    let actions = |ws: &Workspace, id: &Uuid| -> Vec<Action> {
        ws.asset(id)
            .unwrap()
            .chain
            .records()
            .iter()
            .map(|r| r.manifest.core.action)
            .collect()
    };
    assert_eq!(actions(&ws, &owed), vec![Action::Create, Action::Delete]);
    assert_eq!(
        actions(&ws, &restored),
        vec![Action::Create, Action::Delete, Action::TrashRestore],
        "a hand restore is left alone"
    );
    let trash: Vec<String> = ws
        .db()
        .query_trash(0, 100)
        .unwrap()
        .into_iter()
        .map(|r| r.uuid)
        .collect();
    assert_eq!(trash, vec![owed.to_string()]);
    // Idempotent from here.
    assert_eq!(
        ws.migrate_unsigned_sidecars(&opts(album)).unwrap(),
        UnsignedMigrationReport::default()
    );
}

/// An owed delete whose album has since lost its write capability is reported, not
/// written, and does not abort the run: the other candidates still migrate.
#[test]
fn an_owed_delete_into_a_read_only_album_is_reported_not_fatal() {
    let lib = TempDir::new().unwrap();
    let owed = Uuid::from_u128(0x3A);
    let fresh = Uuid::from_u128(0x3B);
    let (album, fallback) = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Recovered").unwrap();
        let fallback = ws.create_album("Imports").unwrap();
        write_legacy(
            lib.path(),
            owed,
            b"\xFF\xD8\xFF owed into read-only",
            vec![("is_deleted", Value::Bool(true))],
        );
        assert_eq!(
            ws.migrate_unsigned_sidecars(&opts(album)).unwrap().trashed,
            vec![owed]
        );
        // The crash window: the chain holds the create only.
        let asset = ws.asset(&owed).unwrap();
        let create_only = cbor::to_canonical_vec(&vec![asset.chain.records()[0].clone()]).unwrap();
        fs::write(ws.provenance_path(asset), create_only).unwrap();
        (album, fallback)
    };
    write_legacy(lib.path(), fresh, b"\xFF\xD8\xFF still migrates", vec![]);

    let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    // The album the owed asset lives in is read-only now (the backup-recovered shape).
    ws.albums.get_mut(&album).unwrap().write_tier = None;
    let owed_sidecar = ws.sidecar_path(ws.asset(&owed).unwrap());

    let report = ws.migrate_unsigned_sidecars(&opts(fallback)).unwrap();
    assert_eq!(report.migrated, vec![fresh], "the run went on");
    assert!(report.trashed.is_empty());
    assert_eq!(
        report.skipped,
        vec![(
            owed_sidecar,
            MigrationSkip::AlbumReadOnly {
                asset_id: owed,
                album_id: album,
            },
        )]
    );
    assert_eq!(
        ws.asset(&owed).unwrap().chain.records().len(),
        1,
        "no delete was authored into the read-only album"
    );
}

/// Ids and names are checked before anything is written: the same id in two month
/// buckets admits one and refuses the other; a record whose `uuid` field does not name
/// its file is refused; a stem that is not a UUID is refused; and a quarantine twin that
/// holds *different* bytes is a conflict, not something to overwrite.
#[test]
fn duplicate_ids_mismatched_records_and_conflicting_twins_are_refused() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Imports").unwrap();

    // The same id in two buckets.
    let twice = Uuid::from_u128(0x2B);
    write_legacy(lib.path(), twice, b"\xFF\xD8\xFF first bucket", vec![]);
    let other_dir = lib.path().join("media/2024/2024-07");
    fs::create_dir_all(&other_dir).unwrap();
    let second_original = b"\xFF\xD8\xFF second bucket";
    fs::write(
        other_dir.join(format!("{}.jpg", twice.simple())),
        second_original,
    )
    .unwrap();
    let mut second = Vec::new();
    ciborium::ser::into_writer(&legacy_map(twice, second_original, vec![]), &mut second).unwrap();
    let second_path = other_dir.join(format!("{}.cbor", twice.simple()));
    fs::write(&second_path, &second).unwrap();

    // A record naming a different uuid than its file.
    let misnamed = Uuid::from_u128(0x2C);
    let (misnamed_path, _) = write_legacy(
        lib.path(),
        misnamed,
        b"\xFF\xD8\xFF misnamed",
        vec![("uuid", text(&Uuid::from_u128(0x2CC).to_string()))],
    );

    // A stem that is not a uuid.
    let not_an_id = lib.path().join("media/1970/1970-01/not-an-asset-id.cbor");
    let mut junk = Vec::new();
    ciborium::ser::into_writer(&legacy_map(Uuid::from_u128(0x2D), b"x", vec![]), &mut junk)
        .unwrap();
    fs::write(&not_an_id, &junk).unwrap();

    // A quarantine twin holding different bytes.
    let conflicted = Uuid::from_u128(0x2E);
    let (conflicted_path, conflicted_bytes) =
        write_legacy(lib.path(), conflicted, b"\xFF\xD8\xFF conflicted", vec![]);
    let quarantine = lib.path().join(".library/quarantine");
    fs::create_dir_all(&quarantine).unwrap();
    fs::write(
        quarantine.join(format!("{}.cbor", conflicted.simple())),
        b"\xA1\x67version\x01",
    )
    .unwrap();

    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert_eq!(report.migrated, vec![twice]);
    assert_eq!(
        ws.read_plaintext(&twice).unwrap(),
        b"\xFF\xD8\xFF first bucket",
        "the 1970 bucket sorts first"
    );
    let mut skips = report.skipped.clone();
    skips.sort_by(|a, b| a.0.cmp(&b.0));
    let mut want = vec![
        (second_path.clone(), MigrationSkip::IdCollision(twice)),
        (
            conflicted_path.clone(),
            MigrationSkip::QuarantineConflict(conflicted),
        ),
        (
            not_an_id.clone(),
            MigrationSkip::InvalidAssetId("not-an-asset-id".to_string()),
        ),
    ];
    want.sort_by(|a, b| a.0.cmp(&b.0));
    let misnamed_skip = skips
        .iter()
        .position(|(p, _)| *p == misnamed_path)
        .expect("the misnamed record is refused");
    assert!(
        matches!(&skips[misnamed_skip].1, MigrationSkip::Undecodable(m) if m.contains("does not name"))
    );
    skips.remove(misnamed_skip);
    assert_eq!(skips, want);
    assert_eq!(fs::read(&second_path).unwrap(), second, "untouched");
    assert_eq!(
        fs::read(&conflicted_path).unwrap(),
        conflicted_bytes,
        "untouched"
    );
    assert!(ws.asset(&misnamed).is_none());
    assert!(ws.asset(&conflicted).is_none());
}

/// A signed sidecar with no chain and no quarantine copy is nothing this verb can rebuild:
/// reported as stranded, never touched.
#[test]
fn a_stranded_signed_sidecar_is_reported_not_touched() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("signed.jpg");
    fs::write(&img, b"\xFF\xD8\xFF loses its chain").unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Imports").unwrap();
    let id = ws.import_asset(album, &img).unwrap();
    let chain = ws.provenance_path(ws.asset(&id).unwrap());
    let sidecar_path = ws.sidecar_path(ws.asset(&id).unwrap());
    fs::remove_file(&chain).unwrap();
    let bytes = fs::read(&sidecar_path).unwrap();
    drop(ws);

    let mut ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    let report = ws.migrate_unsigned_sidecars(&opts(album)).unwrap();
    assert_eq!(
        report.skipped,
        vec![(sidecar_path.clone(), MigrationSkip::Stranded(id))]
    );
    assert_eq!(fs::read(&sidecar_path).unwrap(), bytes);
}

// ── keyless rebuild on the un-migrated fixture ──────────────────────────

/// A keyless `rebuild_index` over an un-migrated library succeeds, indexes the signed
/// asset, and indexes nothing for the legacy files — the one user-visible regression the
/// deletion of the unsigned reader accepts, and the reason the verb exists.
#[test]
fn keyless_rebuild_reports_legacy_files_and_indexes_none_of_them() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("signed.jpg");
    fs::write(&img, b"\xFF\xD8\xFF a signed asset").unwrap();
    let signed = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        ws.import_asset(album, &img).unwrap()
    };
    let (rich, future, plain) = three_legacy_assets(lib.path());

    fs::remove_file(lib.path().join("index/library.sqlite")).unwrap();
    let library = open_library(lib.path()).unwrap();
    rebuild_index(&library).unwrap();

    assert!(
        library
            .db
            .find_by_uuid(&signed.to_string())
            .unwrap()
            .is_some()
    );
    for id in [rich, future, plain] {
        assert!(
            library.db.find_by_uuid(&id.to_string()).unwrap().is_none(),
            "a keyless rebuild cannot sign, so it indexes no legacy asset"
        );
    }
    assert_eq!(library.db.query_timeline(0, 100).unwrap().len(), 1);
}

// ── the private decoder ─────────────────────────────────────────────────

#[test]
fn the_legacy_decoder_projects_its_fields_and_keeps_the_map() {
    let id = Uuid::from_u128(0x2A);
    let map = legacy_map(
        id,
        b"bytes",
        vec![
            ("rating", int(3)),
            ("tags", Value::Array(vec![text("a")])),
            ("capture_timestamp", int(1_600_000_000)),
            ("capture_utc", Value::Null),
            ("gps_lat", Value::Float(1.5)),
            ("gps_lon", Value::Float(-2.5)),
            ("stack_hint", stack_hint("k", "raw")),
            ("album_id", text("not-a-uuid")),
            ("mystery", Value::Bytes(vec![1, 2, 3])),
        ],
    );
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&map, &mut bytes).unwrap();
    let record = LegacyRecord::decode(&bytes).unwrap();
    assert_eq!(record.uuid, id.to_string());
    assert_eq!(record.import_timestamp, 1_720_000_000);
    assert_eq!(record.rating, 3);
    assert_eq!(record.tags, vec!["a".to_string()]);
    assert_eq!(record.capture_utc, None, "null counts as absent");
    assert_eq!(record.capture_timestamp, Some(1_600_000_000));
    assert_eq!(record.gps, Some((1.5, -2.5)));
    assert_eq!(record.album_id.as_deref(), Some("not-a-uuid"));
    assert!(!record.is_deleted);
    let hint = record.stack_hint.as_ref().unwrap();
    assert_eq!(
        (hint.detection_key.as_str(), hint.member_role.as_str()),
        ("k", "raw")
    );
    assert_eq!(hint.detection_method, "filename_stem");
    assert_eq!(hint.stack_type, StackType::RawJpeg);
    assert_eq!(
        record.map, map,
        "the whole map is kept, unknown keys included"
    );
    // Precedence inside the fallback: capture_utc, then capture_timestamp, then import.
    assert_eq!(record.capture_fallback().as_second(), 1_600_000_000);

    // Required fields are required; a wrong type is an error, not a default.
    let mut missing = Vec::new();
    ciborium::ser::into_writer(
        &Value::Map(vec![(text("version"), int(1)), (text("uuid"), text("x"))]),
        &mut missing,
    )
    .unwrap();
    assert!(
        LegacyRecord::decode(&missing)
            .unwrap_err()
            .contains("hash_sha256")
    );
    let mut wrong = Vec::new();
    ciborium::ser::into_writer(
        &legacy_map(id, b"bytes", vec![("rating", text("four"))]),
        &mut wrong,
    )
    .unwrap();
    assert!(LegacyRecord::decode(&wrong).unwrap_err().contains("rating"));
    assert!(LegacyRecord::decode(b"garbage").is_err());
}
