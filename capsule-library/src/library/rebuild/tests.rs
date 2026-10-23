use std::collections::BTreeMap;

use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::primitives::{Argon2Params, CRYPTO_SUITE_ID};
use capsule_core::metadata::crdt::{Lww, OrSet};
use capsule_core::sidecar::sidecar_v1::{CullFlag, Dimensions};
use tempfile::TempDir;
use uuid::Uuid;

use super::*;
use crate::library::init::init_library;
use crate::library::open::open_library;
use crate::lifecycle::Workspace;

/// The media directory every fixture in this module writes into (`capture_timestamp`
/// below resolves here).
const FIXTURE_MEDIA: &str = "media/1970/1970-01";

fn fast_params() -> Argon2Params {
    Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    }
}

// ── the signed shape ────────────────────────────────────────────────────

/// A signed sidecar as `lifecycle::write_asset_files` would leave it on disk, minus the
/// signature: `rebuild_index` holds no keys and verifies nothing, so a fixture does not
/// need a valid one to exercise the path faithfully.
fn signed_sidecar(uuid: Uuid, hash_byte: u8) -> SidecarV1 {
    SidecarV1 {
        sidecar_schema: SIDECAR_SCHEMA_V1,
        crypto_suite_id: CRYPTO_SUITE_ID,
        uuid,
        hash: Hash32([hash_byte; 32]),
        capture_timestamp: "1970-01-05T00:00:00Z".into(),
        capture_timestamp_user: Lww::new(),
        import_timestamp: "1970-01-06T00:00:00Z".into(),
        content_type: "image/jpeg".into(),
        dimensions: Some(Dimensions {
            width: 4032,
            height: 3024,
        }),
        lqip: None,
        tags_user: OrSet::new(),
        tags_ai: OrSet::new(),
        caption: Lww::new(),
        rating: Lww::new(),
        stack_membership: Lww::new(),
        regions: Default::default(),
        cull: Lww::new(),
        hidden: Lww::new(),
        camera_id: None,
        device_id: Uuid::from_u128(0xD1),
        session_id: Uuid::from_u128(0x5E),
        gps: None,
        gps_user: Lww::new(),
        provenance_chain_hash: None,
        unknown: BTreeMap::new(),
        signature: None,
    }
}

fn write_signed(dir: &Path, sidecar: &SidecarV1) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join(format!("{}.cbor", sidecar.uuid.simple())),
        sidecar.to_canonical_vec(),
    )
    .unwrap();
}

/// A legacy unsigned sidecar exactly as the retired serializer wrote it: text keys and
/// `version: 1`, no integer key `0`.
fn legacy_sidecar_bytes(uuid: &str, hash_hex: &str) -> Vec<u8> {
    use ciborium::value::Value;
    let text = |s: &str| Value::Text(s.to_string());
    let map = Value::Map(vec![
        (text("version"), Value::Integer(1.into())),
        (text("uuid"), text(uuid)),
        (text("asset_type"), text("photo")),
        (text("hash_sha256"), text(hash_hex)),
        (
            text("import_timestamp"),
            Value::Integer(1_720_000_000.into()),
        ),
    ]);
    let mut out = Vec::new();
    ciborium::ser::into_writer(&map, &mut out).unwrap();
    out
}

/// The two shapes are disjoint on the wire, so the probe cannot mis-route a file: a
/// signed sidecar has integer field 0 and no `version` key, an unsigned one has `version`
/// and no field 0. This is also *why* a rebuild that only knew the unsigned shape
/// would lose the register state silently — it would not mis-read signed sidecars, it
/// would skip every one of them, so a signed library rebuilt to nothing at all.
#[test]
fn the_two_sidecar_shapes_probe_disjointly() {
    let signed = signed_sidecar(Uuid::from_u128(0xD15), 0x66).to_canonical_vec();
    assert_eq!(shape::probe(&signed), SidecarShape::Signed { schema: 1 });
    assert!(SidecarV1::from_canonical_slice(&signed, SIDECAR_SCHEMA_V1).is_ok());

    let legacy = legacy_sidecar_bytes("eeee0000-0000-0000-0000-000000000005", &"e".repeat(64));
    assert_eq!(shape::probe(&legacy), SidecarShape::LegacyUnsigned);
    assert!(
        SidecarV1::from_canonical_slice(&legacy, SIDECAR_SCHEMA_V1).is_err(),
        "the signed reader must reject a pre-signed-path sidecar"
    );
}

/// **Unsigned sidecars are reported, not indexed.** An unsigned pre-signed-path sidecar is
/// reported and *not* indexed: a rebuild holds no keys and cannot admit it, and indexing
/// it would show an asset the workspace cannot verify, export, or upload. The signed asset
/// beside it rebuilds as before, and the run still succeeds — one legacy file must not
/// cost the library its index.
#[test]
fn an_unsigned_sidecar_is_reported_not_indexed() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("lib");
    let lib = init_library(&root, "T").unwrap();
    let media_dir = root.join(FIXTURE_MEDIA);
    fs::create_dir_all(&media_dir).unwrap();

    fs::write(
        media_dir.join("dddd000000000000000000000000004.cbor"),
        legacy_sidecar_bytes("dddd0000-0000-0000-0000-000000000004", &"d".repeat(64)),
    )
    .unwrap();
    let signed_id = Uuid::from_u128(0x11D);
    let mut signed = signed_sidecar(signed_id, 0x44);
    signed
        .hidden
        .set(true, "2026-08-01T00:00:00Z", Uuid::from_u128(0xD1));
    write_signed(&media_dir, &signed);

    rebuild_index(&lib).unwrap();

    assert!(
        lib.db.find_by_hash(&"d".repeat(64)).unwrap().is_none(),
        "the unsigned sidecar is not indexed"
    );
    let signed_row = lib
        .db
        .find_by_uuid(&signed_id.to_string())
        .unwrap()
        .unwrap();
    assert!(signed_row.is_hidden);
    assert!(lib.db.query_timeline(0, 100).unwrap().is_empty());
    assert_eq!(lib.db.query_hidden(0, 100).unwrap().len(), 1);
}

/// **The acceptance case.** A hidden asset survives a rebuild still hidden — and
/// therefore still absent from the default projections, which is what the hidden state is
/// *for*. Before the fix the rebuilt row came back `is_hidden = false` and the asset
/// reappeared in the timeline: a gate bypass on the one path a user reaches for after
/// losing an index.
#[test]
fn signed_rebuild_keeps_a_hidden_asset_hidden() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("lib");
    let lib = init_library(&root, "T").unwrap();

    let hidden_id = Uuid::from_u128(0xA1DE);
    let visible_id = Uuid::from_u128(0x5EE);
    let mut hidden = signed_sidecar(hidden_id, 0xAA);
    hidden
        .hidden
        .set(true, "2026-08-01T00:00:00Z", Uuid::from_u128(0xD1));
    let visible = signed_sidecar(visible_id, 0xBB);

    let dir = root.join(FIXTURE_MEDIA);
    write_signed(&dir, &hidden);
    write_signed(&dir, &visible);

    rebuild_index(&lib).unwrap();

    let row = lib
        .db
        .find_by_uuid(&hidden_id.to_string())
        .unwrap()
        .expect("the hidden asset is back in the index");
    assert!(row.is_hidden, "the sidecar `hidden` register must survive");

    // ...and the projections hide the asset from every default view.
    let timeline = lib.db.query_timeline(0, 100).unwrap();
    assert_eq!(
        timeline.len(),
        1,
        "the hidden asset stays out of the timeline"
    );
    assert_eq!(timeline[0].uuid, visible_id.to_string());

    let hidden_view = lib.db.query_hidden(0, 100).unwrap();
    assert_eq!(hidden_view.len(), 1);
    assert_eq!(hidden_view[0].uuid, hidden_id.to_string());
}

/// The control for the case above: a never-written `hidden` register is the wire-absent
/// default, and rebuilds as visible. (A "fix" that hid everything would pass the test
/// above and fail this one.)
#[test]
fn signed_rebuild_keeps_a_visible_asset_visible() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("lib");
    let lib = init_library(&root, "T").unwrap();

    let id = Uuid::from_u128(0x5E1);
    let mut sidecar = signed_sidecar(id, 0xCC);
    sidecar
        .rating
        .set(4, "2026-08-01T00:00:00Z", Uuid::from_u128(0xD1));
    write_signed(&root.join(FIXTURE_MEDIA), &sidecar);

    rebuild_index(&lib).unwrap();

    let row = lib.db.find_by_uuid(&id.to_string()).unwrap().unwrap();
    assert!(!row.is_hidden);
    assert!(!row.is_deleted);
    assert_eq!(row.rating, 4, "the rating register is projected too");
    assert_eq!(row.width, Some(4032));
    assert_eq!(row.hash_sha256, Hash32([0xCC; 32]).to_hex());
    assert_eq!(lib.db.query_timeline(0, 100).unwrap().len(), 1);
    assert!(lib.db.query_hidden(0, 100).unwrap().is_empty());
}

/// `stack_membership` rides the same register set as `hidden`, so it is lost the same way:
/// the unsigned shape's `stack_hint` is not what a signed library holds. A rebuilt stack
/// must put the members back under one stack id with only the primary in the timeline.
#[test]
fn signed_rebuild_projects_stack_membership_into_the_index() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("lib");
    let lib = init_library(&root, "T").unwrap();

    let stack_id = Uuid::from_u128(0x57ACC);
    let primary_id = Uuid::from_u128(0x91);
    let member_id = Uuid::from_u128(0x92);

    let mut primary = signed_sidecar(primary_id, 0x11);
    primary.stack_membership.set(
        Some(StackMembership {
            stack_id,
            stack_type: StackType::RawJpeg,
            role: StackRole::Primary,
            member_index: Some(0),
        }),
        "2026-08-01T00:00:00Z",
        Uuid::from_u128(0xD1),
    );
    let mut member = signed_sidecar(member_id, 0x22);
    member.stack_membership.set(
        Some(StackMembership {
            stack_id,
            stack_type: StackType::RawJpeg,
            role: StackRole::Member,
            member_index: Some(1),
        }),
        "2026-08-01T00:00:00Z",
        Uuid::from_u128(0xD1),
    );

    let dir = root.join(FIXTURE_MEDIA);
    write_signed(&dir, &primary);
    write_signed(&dir, &member);

    rebuild_index(&lib).unwrap();

    let primary_row = lib
        .db
        .find_by_uuid(&primary_id.to_string())
        .unwrap()
        .unwrap();
    let member_row = lib
        .db
        .find_by_uuid(&member_id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(
        primary_row.stack_id.as_deref(),
        Some(stack_id.to_string().as_str())
    );
    assert_eq!(
        member_row.stack_id.as_deref(),
        Some(stack_id.to_string().as_str())
    );
    assert!(!primary_row.is_stack_hidden);
    assert!(
        member_row.is_stack_hidden,
        "a non-primary member is suppressed from the timeline"
    );

    let timeline = lib.db.query_timeline(0, 100).unwrap();
    assert_eq!(timeline.len(), 1, "the stack collapses to its primary");
    assert_eq!(timeline[0].uuid, primary_id.to_string());

    let members = lib.db.list_stack_members(&stack_id.to_string()).unwrap();
    assert_eq!(members.len(), 2, "both members are in `stack_members`");
}

/// The compatibility case for the older importer. A stack formed by an importer *of that
/// vintage* was
/// recorded only as `assets.stack_id` / `is_stack_hidden` — its signed sidecar got no
/// `stack_membership` register, so that placement exists nowhere on disk. Rebuilding a
/// *live* index (`capsule rebuild`) must leave it alone rather than overwrite the only copy
/// with NULL. Current imports write the register (see
/// `lifecycle::import::importer_formed_stack_survives_index_loss_and_rebuild`), so this
/// covers existing libraries, not new writes.
#[test]
fn signed_rebuild_preserves_index_only_stack_placement() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("lib");
    let lib = init_library(&root, "T").unwrap();

    let id = Uuid::from_u128(0x5AFE);
    let sidecar = signed_sidecar(id, 0x55);
    write_signed(&root.join(FIXTURE_MEDIA), &sidecar);

    // An importer-stacked secondary member, as the write path indexed it.
    rebuild_index(&lib).unwrap();
    let mut row = lib.db.find_by_uuid(&id.to_string()).unwrap().unwrap();
    row.stack_id = Some("stack-abc".to_string());
    row.is_stack_hidden = true;
    lib.db.upsert_asset(&row).unwrap();

    rebuild_index(&lib).unwrap();

    let after = lib.db.find_by_uuid(&id.to_string()).unwrap().unwrap();
    assert_eq!(after.stack_id.as_deref(), Some("stack-abc"));
    assert!(
        after.is_stack_hidden,
        "the rebuild must not resurrect a stacked secondary into the timeline"
    );
    assert!(lib.db.query_timeline(0, 100).unwrap().is_empty());
}

/// Leaving a stack is a **stamped** `None`, and the register says so — so a rebuild must
/// clear the columns rather than reconstruct the old placement from a surviving index row.
///
/// This is the other half of `signed_rebuild_preserves_index_only_stack_placement`, and the
/// two together are why the projection matches on `get()` instead of flattening: a stamped
/// `None` and a never-written register are different facts. Flattened, they collapse into one
/// arm and an asset the user pulled out of a stack silently returns to it — still suppressed
/// from the timeline — on the next rebuild.
#[test]
fn signed_rebuild_clears_placement_when_the_register_says_the_asset_left() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("lib");
    let lib = init_library(&root, "T").unwrap();

    let id = Uuid::from_u128(0x1EF7);
    let mut sidecar = signed_sidecar(id, 0x1E);
    // Stamped `None`: this asset was in a stack and was taken out of it.
    sidecar
        .stack_membership
        .set(None, "2026-08-02T00:00:00Z", Uuid::from_u128(0xD1));
    write_signed(&root.join(FIXTURE_MEDIA), &sidecar);

    // An index row still carrying the placement from before it left.
    rebuild_index(&lib).unwrap();
    let mut row = lib.db.find_by_uuid(&id.to_string()).unwrap().unwrap();
    row.stack_id = Some("stack-stale".to_string());
    row.is_stack_hidden = true;
    lib.db.upsert_asset(&row).unwrap();

    rebuild_index(&lib).unwrap();

    let after = lib.db.find_by_uuid(&id.to_string()).unwrap().unwrap();
    assert_eq!(
        after.stack_id, None,
        "a stamped `None` is authoritative; the stale index placement must not survive"
    );
    assert!(!after.is_stack_hidden);
    assert_eq!(
        lib.db.query_timeline(0, 100).unwrap().len(),
        1,
        "an asset that left a stack belongs back in the timeline"
    );
}

/// Recovery must not depend on a second full pass being harmless-in-practice: rebuilding
/// an already-rebuilt index is a no-op, stacks included.
#[test]
fn signed_rebuild_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("lib");
    let lib = init_library(&root, "T").unwrap();

    let id = Uuid::from_u128(0x1D);
    let mut sidecar = signed_sidecar(id, 0xDD);
    sidecar
        .hidden
        .set(true, "2026-08-01T00:00:00Z", Uuid::from_u128(0xD1));
    sidecar.stack_membership.set(
        Some(StackMembership {
            stack_id: Uuid::from_u128(0x57AC2),
            stack_type: StackType::Burst,
            role: StackRole::Primary,
            member_index: None,
        }),
        "2026-08-01T00:00:00Z",
        Uuid::from_u128(0xD1),
    );
    write_signed(&root.join(FIXTURE_MEDIA), &sidecar);

    rebuild_index(&lib).unwrap();
    rebuild_index(&lib).unwrap();

    let row = lib.db.find_by_uuid(&id.to_string()).unwrap().unwrap();
    assert!(row.is_hidden, "the second pass did not un-hide it");
    assert_eq!(
        lib.db
            .list_stack_members(&Uuid::from_u128(0x57AC2).to_string())
            .unwrap()
            .len(),
        1,
        "the member was not duplicated"
    );
}

/// The sibling logs share the `.cbor` suffix and must never be parsed as sidecars — one
/// asset on disk is one row, not four — and the evidence logs come out of a rebuild
/// byte-identical: a rebuild re-derives the index, never the evidence.
#[test]
fn signed_rebuild_ignores_provenance_receipt_and_attestation_siblings() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("lib");
    let lib = init_library(&root, "T").unwrap();

    let id = Uuid::from_u128(0x51B);
    let sidecar = signed_sidecar(id, 0xEE);
    let dir = root.join(FIXTURE_MEDIA);
    write_signed(&dir, &sidecar);
    // Garbage in the sibling logs: they must not even be opened as sidecars.
    fs::write(
        dir.join(format!("{}.provenance.cbor", id.simple())),
        b"not a sidecar",
    )
    .unwrap();
    fs::write(
        dir.join(format!("{}.receipts.cbor", id.simple())),
        b"not a sidecar either",
    )
    .unwrap();
    let attestations = dir.join(format!("{}.attestations.cbor", id.simple()));
    fs::write(&attestations, b"nor is the attestation log").unwrap();

    rebuild_index(&lib).unwrap();

    assert_eq!(lib.db.query_timeline(0, 100).unwrap().len(), 1);
    assert_eq!(
        fs::read(&attestations).unwrap(),
        b"nor is the attestation log",
        "the attestation log survives the rebuild untouched"
    );
}

/// An asset whose chain is missing is still worth indexing — but it is indexed as
/// **not** trashed, which is the permissive answer, so the `warn` in `chain_facts` is the
/// only record that the state was unknown rather than known-false.
#[test]
fn signed_rebuild_without_a_chain_indexes_the_asset_as_not_trashed() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("lib");
    let lib = init_library(&root, "T").unwrap();

    let id = Uuid::from_u128(0x00C0_FFEE);
    write_signed(&root.join(FIXTURE_MEDIA), &signed_sidecar(id, 0x33));

    rebuild_index(&lib).unwrap();

    let row = lib.db.find_by_uuid(&id.to_string()).unwrap().unwrap();
    assert!(!row.is_deleted);
    assert_eq!(row.album_id, None, "no chain, no album to claim");
}

/// Overwrite an asset's signed sidecar with `mutate` applied.
///
/// This is how a test produces a hidden asset today: `hidden` has an index projection and
/// a gated view but no `Workspace` setter yet, so the register can only be
/// written straight onto the on-disk record — which is exactly the shape a peer device's
/// write would arrive in. The signature goes stale, and that costs nothing here:
/// `rebuild_index` holds no keys and verifies none (see the module docs).
fn mutate_signed_sidecar_on_disk(
    root: &Path,
    asset_id: &Uuid,
    mutate: impl FnOnce(&mut SidecarV1),
) {
    let name = format!("{}.cbor", asset_id.simple());
    let path = WalkDir::new(root.join("media"))
        .into_iter()
        .filter_map(std::result::Result::ok)
        .map(|e| e.path().to_path_buf())
        .find(|p| p.file_name().unwrap_or_default().to_string_lossy() == name)
        .expect("the asset's signed sidecar is on disk");
    let bytes = fs::read(&path).unwrap();
    let mut sidecar = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
    mutate(&mut sidecar);
    fs::write(&path, sidecar.to_canonical_vec()).unwrap();
}

/// **The acceptance case on a real signed library**, rather than on a hand-built
/// sidecar: import two assets through the signed write path, hide one, delete the whole
/// index file (the actual recovery scenario), and rebuild. The hidden asset must come
/// back hidden and stay out of the timeline.
#[test]
fn hidden_survives_index_loss_on_a_real_signed_library() {
    let lib_dir = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let visible_src = src.path().join("visible.jpg");
    let hidden_src = src.path().join("hidden.jpg");
    fs::write(&visible_src, b"\xFF\xD8\xFF visible asset").unwrap();
    fs::write(&hidden_src, b"\xFF\xD8\xFF hidden asset").unwrap();

    let (visible, hidden) = {
        let mut ws =
            Workspace::create_with_params(lib_dir.path(), b"passphrase", fast_params()).unwrap();
        let album = ws.create_album("Private").unwrap();
        let visible = ws.import_asset(album, &visible_src).unwrap();
        let hidden = ws.import_asset(album, &hidden_src).unwrap();
        let device = ws.device_id();
        mutate_signed_sidecar_on_disk(lib_dir.path(), &hidden, |s| {
            s.hidden.set(true, "2026-08-01T00:00:00Z", device);
        });
        (visible, hidden)
    };

    fs::remove_file(lib_dir.path().join("index/library.sqlite")).unwrap();
    let lib = open_library(lib_dir.path()).unwrap();
    rebuild_index(&lib).unwrap();

    let row = lib
        .db
        .find_by_uuid(&hidden.to_string())
        .unwrap()
        .expect("the hidden asset is back in the index");
    assert!(row.is_hidden, "it must come back still hidden");

    let timeline = lib.db.query_timeline(0, 100).unwrap();
    assert_eq!(
        timeline.len(),
        1,
        "the hidden asset stays out of the timeline"
    );
    assert_eq!(timeline[0].uuid, visible.to_string());
    let hidden_view = lib.db.query_hidden(0, 100).unwrap();
    assert_eq!(hidden_view.len(), 1);
    assert_eq!(hidden_view[0].uuid, hidden.to_string());
}

/// **The trash half of the rebuild, end to end on a real signed library.** The
/// signed sidecar carries no deletion state at all — it lives in the provenance chain —
/// so a rebuild that reads only sidecars resurrects every trashed asset into the default
/// timeline. Here the whole index file is deleted (the actual recovery scenario) and
/// rebuilt from disk.
#[test]
fn signed_rebuild_recovers_trash_state_and_album_from_the_provenance_chain() {
    let lib_dir = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let kept_src = src.path().join("kept.jpg");
    let trashed_src = src.path().join("trashed.jpg");
    fs::write(&kept_src, b"\xFF\xD8\xFF kept asset").unwrap();
    fs::write(&trashed_src, b"\xFF\xD8\xFF trashed asset").unwrap();

    let (album, kept, trashed) = {
        let mut ws =
            Workspace::create_with_params(lib_dir.path(), b"passphrase", fast_params()).unwrap();
        let album = ws.create_album("Trip").unwrap();
        let kept = ws.import_asset(album, &kept_src).unwrap();
        let trashed = ws.import_asset(album, &trashed_src).unwrap();
        ws.soft_delete(&trashed, 30).unwrap();
        (album, kept, trashed)
    };

    // Lose the index outright, then reopen and rebuild from the signed artifacts.
    fs::remove_file(lib_dir.path().join("index/library.sqlite")).unwrap();
    let lib = open_library(lib_dir.path()).unwrap();
    assert!(
        lib.db.find_by_uuid(&kept.to_string()).unwrap().is_none(),
        "the index really is gone"
    );

    rebuild_index(&lib).unwrap();

    let trashed_row = lib
        .db
        .find_by_uuid(&trashed.to_string())
        .unwrap()
        .expect("the trashed asset is back in the index");
    assert!(trashed_row.is_deleted, "it must come back still trashed");
    assert!(trashed_row.deleted_at.is_some());
    assert_eq!(
        trashed_row.album_id.as_deref(),
        Some(album.to_string().as_str()),
        "the album comes back from the chain head"
    );

    let timeline = lib.db.query_timeline(0, 100).unwrap();
    assert_eq!(
        timeline.len(),
        1,
        "a trashed asset stays out of the timeline"
    );
    assert_eq!(timeline[0].uuid, kept.to_string());
    let trash = lib.db.query_trash(0, 100).unwrap();
    assert_eq!(trash.len(), 1);
    assert_eq!(trash[0].uuid, trashed.to_string());
}

/// **The `cull` half of the audit.** `cull` has no column in `assets` and no query
/// projects it: the culling views read the register off the signed sidecars that
/// `Workspace::open` restores. So losing and rebuilding the index costs it nothing —
/// which this proves rather than assumes, since "it is fine" is exactly the kind of claim
/// that rots.
#[test]
fn cull_survives_index_loss_because_it_is_not_an_index_projection() {
    let lib_dir = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("rejected.jpg");
    fs::write(&img, b"\xFF\xD8\xFF rejected asset").unwrap();

    let id = {
        let mut ws =
            Workspace::create_with_params(lib_dir.path(), b"passphrase", fast_params()).unwrap();
        let album = ws.create_album("Cull").unwrap();
        let id = ws.import_asset(album, &img).unwrap();
        ws.set_cull(&id, CullFlag::Reject).unwrap();
        id
    };

    fs::remove_file(lib_dir.path().join("index/library.sqlite")).unwrap();
    {
        let lib = open_library(lib_dir.path()).unwrap();
        rebuild_index(&lib).unwrap();
        assert!(lib.db.find_by_uuid(&id.to_string()).unwrap().is_some());
    }

    let ws = Workspace::open(lib_dir.path(), b"passphrase", fast_params()).unwrap();
    assert_eq!(
        ws.cull_flag(&id),
        CullFlag::Reject,
        "the cull register is read from the sidecar, not the index"
    );
    assert_eq!(ws.assets_by_cull(CullFlag::Reject), vec![id]);
}
