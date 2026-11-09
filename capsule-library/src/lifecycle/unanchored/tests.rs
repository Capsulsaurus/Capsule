use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::crypto::verify_asset::VerifyOutcome;
use ciborium::value::Value;
use tempfile::TempDir;

use super::super::import::{WriteFault, with_write_fault};
use super::super::{LifecycleError, fast_workspace};
use super::*;
use crate::library::rebuild_index;

fn fast_params() -> Argon2Params {
    Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    }
}

/// A retired unsigned pre-signed-path record, as its serializer wrote it, at
/// `media/1970/1970-01/{id}.cbor` beside a `{id}.jpg` original. Returns the sidecar path.
fn write_retired_unsigned(root: &Path, id: Uuid) -> PathBuf {
    let text = |s: &str| Value::Text(s.to_string());
    let map = Value::Map(vec![
        (text("version"), Value::Integer(1.into())),
        (text("uuid"), text(&id.to_string())),
        (text("hash_sha256"), text(&"a".repeat(64))),
        (
            text("import_timestamp"),
            Value::Integer(1_720_000_000.into()),
        ),
    ]);
    let dir = root.join("media/1970/1970-01");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{}.jpg", id.simple())),
        b"\xFF\xD8\xFF retired",
    )
    .unwrap();
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&map, &mut bytes).unwrap();
    let path = dir.join(format!("{}.cbor", id.simple()));
    fs::write(&path, &bytes).unwrap();
    path
}

/// Import one signed asset into a fresh library at `root`. Returns its id.
fn import_one(root: &Path, bytes: &[u8]) -> Uuid {
    let src = TempDir::new().unwrap();
    let img = src.path().join("signed.jpg");
    fs::write(&img, bytes).unwrap();
    let mut ws = fast_workspace(root);
    let album = ws.create_album("Imports").unwrap();
    ws.import_asset(album, &img).unwrap()
}

/// The receipt and attestation logs share the sidecar's `.cbor` suffix; beside a signed
/// asset they are evidence, never an unanchored sidecar.
#[test]
fn the_evidence_logs_beside_a_signed_asset_are_not_unanchored_sidecars() {
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

    assert!(unanchored_sidecars(lib.path()).is_empty());
}

/// The derivative bundle the lifecycle writes as `derivatives/{uuid}.derivatives.cbor`
/// shares the sidecar's `.cbor` suffix too. A library holding one reports no unanchored
/// sidecar on open, keyless or unlocked, and `rebuild_index` does not read it as one.
#[test]
fn a_derivative_bundle_is_not_an_unanchored_sidecar() {
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

    assert!(unanchored_sidecars(lib.path()).is_empty());
    let ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    assert!(ws.unanchored_sidecars().is_empty());
    assert_eq!(ws.asset_ids(), vec![id]);
    rebuild_index(&ws.library).unwrap();
    assert_eq!(ws.db().query_timeline(0, 100).unwrap().len(), 1);
}

/// A root with no `media/` directory — a freshly initialised library, or a path that is
/// not a library at all — has nothing unanchored, and the scan does not error.
#[test]
fn keyless_scan_of_a_library_without_media_is_empty() {
    let lib = TempDir::new().unwrap();
    assert!(unanchored_sidecars(lib.path()).is_empty());
    assert!(unanchored_sidecars(&lib.path().join("missing")).is_empty());
}

/// A retired unsigned record is no longer told apart from any other foreign file: open
/// still succeeds beside it, restores the signed asset, and reports the record as
/// `Unknown` — never silently skipped, never touched, never admitted.
#[test]
fn a_retired_unsigned_sidecar_is_reported_unknown_and_left_alone() {
    let lib = TempDir::new().unwrap();
    let signed = import_one(lib.path(), b"\xFF\xD8\xFF a signed asset");
    let retired = Uuid::from_u128(0xA1);
    let path = write_retired_unsigned(lib.path(), retired);
    let before = fs::read(&path).unwrap();

    let ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    assert_eq!(
        ws.unanchored_sidecars(),
        [UnanchoredSidecar {
            path: path.clone(),
            asset_id: Some(retired),
            shape: UnanchoredShape::Unknown,
        }]
    );
    assert_eq!(
        ws.asset_ids(),
        vec![signed],
        "only the signed asset is restored"
    );
    assert_eq!(ws.verify(&signed).unwrap(), VerifyOutcome::Accept);
    drop(ws);
    assert_eq!(
        fs::read(&path).unwrap(),
        before,
        "the record is left exactly as found"
    );
}

/// The chain is staged and renamed into place last: an import that crashes after the
/// sidecar and blob landed but before the chain did leaves no `.provenance.cbor` at all —
/// never a torn one that would count as anchored and hide the asset — so `open` reports a
/// chainless signed sidecar and restores nothing for it.
#[test]
fn a_failure_before_the_chain_rename_leaves_a_reported_chainless_sidecar() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("chainless.jpg");
    fs::write(&img, b"\xFF\xD8\xFF chain renamed last").unwrap();
    {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        let failed = with_write_fault(WriteFault::ChainRename, 0, || ws.import_asset(album, &img));
        assert!(
            matches!(failed, Err(LifecycleError::Io(ref m)) if m.contains("injected fault")),
            "{failed:?}"
        );
    }

    let ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    assert!(ws.asset_ids().is_empty(), "no chain, not restored");
    let found = ws.unanchored_sidecars();
    assert_eq!(
        found.iter().map(|u| u.shape).collect::<Vec<_>>(),
        vec![UnanchoredShape::SignedWithoutChain { schema: 1 }]
    );
    let id = found[0]
        .asset_id
        .expect("the sidecar is named by its asset id");
    let dir = found[0].path.parent().unwrap();
    assert!(
        !dir.join(format!("{}.provenance.cbor", id.simple()))
            .exists(),
        "no chain landed, so none is torn"
    );
    assert!(
        dir.join(format!("{}.metadata.bin", id.simple())).exists(),
        "the blob lands before the chain anchors the asset"
    );
}

/// The signed sidecar is renamed into place first: an import that crashes before that
/// rename leaves only a staged `.tmp`, which is not a sidecar, so nothing is unanchored and
/// nothing is restored.
#[test]
fn a_failure_before_the_sidecar_rename_leaves_nothing_unanchored() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("staged.jpg");
    fs::write(&img, b"\xFF\xD8\xFF sidecar renamed first").unwrap();
    {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        let failed = with_write_fault(WriteFault::SidecarRename, 0, || {
            ws.import_asset(album, &img)
        });
        assert!(
            matches!(failed, Err(LifecycleError::Io(ref m)) if m.contains("injected fault")),
            "{failed:?}"
        );
    }

    assert!(unanchored_sidecars(lib.path()).is_empty());
    let ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    assert!(ws.unanchored_sidecars().is_empty());
    assert!(ws.asset_ids().is_empty());
}

/// The keyless scan reports exactly what `Workspace::open` reports, in path order: it
/// skips a signed asset its chain anchors and counts a non-CBOR file and a retired
/// unsigned record, each with the asset id its stem names.
#[test]
fn keyless_scan_agrees_with_open() {
    let lib = TempDir::new().unwrap();
    import_one(lib.path(), b"\xFF\xD8\xFF a signed asset");
    let retired = Uuid::from_u128(0xA2);
    write_retired_unsigned(lib.path(), retired);
    let junk = Uuid::from_u128(0xA4);
    fs::write(
        lib.path()
            .join("media/1970/1970-01")
            .join(format!("{}.cbor", junk.simple())),
        b"not cbor",
    )
    .unwrap();

    let keyless = unanchored_sidecars(lib.path());
    assert_eq!(
        keyless
            .iter()
            .map(|u| (u.asset_id, u.shape))
            .collect::<Vec<_>>(),
        vec![
            (Some(retired), UnanchoredShape::Unknown),
            (Some(junk), UnanchoredShape::Unknown),
        ]
    );
    let mut sorted: Vec<PathBuf> = keyless.iter().map(|u| u.path.clone()).collect();
    sorted.sort();
    assert_eq!(
        keyless.iter().map(|u| u.path.clone()).collect::<Vec<_>>(),
        sorted,
        "deterministic path order"
    );

    let ws = Workspace::open(lib.path(), b"passphrase", fast_params()).unwrap();
    assert_eq!(keyless.as_slice(), ws.unanchored_sidecars());
}
