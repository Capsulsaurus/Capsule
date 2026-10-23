use std::path::PathBuf;

use capsule_core::crypto::keys::HybridSigningKey;
use tempfile::TempDir;

use super::super::{FAST_PARAMS, fast_workspace};
use super::*;

// ── Write-site precedence ───────────────────────────────────────
//
// The [precedence rule] the pipeline doc fixes: embedded EXIF wins over an exporter-side
// record for capture time and GPS. The adapter resolves it once at extraction; these two
// helpers are where the *write* path honours it, and they are the reason an exporter
// record can never overwrite what the file bytes themselves say.
//
// [precedence rule]: https://docs/design/import/pipeline/#third-party-importers

fn ts(secs: i64) -> Timestamp {
    Timestamp::from_second(secs).expect("in-range timestamp")
}

fn point(lat: f64, lon: f64, source: GpsSource) -> Gps {
    Gps {
        lat,
        lon,
        source,
        datum: capsule_core::domain::GpsDatum::Wgs84,
    }
}

/// Both sides present and disagreeing — the case that was unreachable while `extract_exif`
/// returned `None` for every well-formed EXIF file.
#[test]
fn embedded_exif_capture_wins_over_the_folded_record() {
    assert_eq!(
        folded_capture(Some(1_622_505_600), Some(ts(1_000_000_000))),
        (1_622_505_600, CaptureSource::Embedded)
    );
}

#[test]
fn the_folded_record_fills_capture_when_the_bytes_resolve_none() {
    assert_eq!(
        folded_capture(None, Some(ts(1_609_502_400))),
        (1_609_502_400, CaptureSource::Folded)
    );
}

#[test]
fn capture_falls_back_to_import_time_when_neither_side_has_one() {
    let before = Timestamp::now().as_second();
    let (secs, source) = folded_capture(None, None);
    assert_eq!(source, CaptureSource::ImportTime);
    assert!(secs >= before, "the import clock, not the epoch");
}

#[test]
fn embedded_exif_gps_wins_over_the_folded_record() {
    let embedded = point(48.8584, 2.2945, GpsSource::Exif);
    let exporter = point(40.0, -70.0, GpsSource::Manual);
    assert_eq!(
        folded_gps(Some(embedded.clone()), Some(&exporter)),
        Some(embedded)
    );
}

#[test]
fn the_folded_record_fills_gps_when_the_bytes_carry_none() {
    let exporter = point(21.3, -157.8, GpsSource::Manual);
    assert_eq!(folded_gps(None, Some(&exporter)), Some(exporter));
    assert_eq!(folded_gps(None, None), None);
}

/// `import_bytes` is the platform-app entry point (PhotoKit / MediaStore hand over a
/// buffer, not a path). It must land an asset indistinguishable from a file import — same
/// signed artifacts, same `verify_asset` verdict, same plaintext readable back — and must
/// leave no staged plaintext behind.
#[test]
fn import_bytes_lands_a_signed_asset_and_clears_its_staging_copy() {
    let lib = TempDir::new().unwrap();
    let bytes = b"\xFF\xD8\xFF\xE0 in-memory jpeg bytes \x00\x01\x02".to_vec();

    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Camera Roll").unwrap();
    let asset = ws.import_bytes(album, "IMG_0042.JPG", &bytes).unwrap();

    assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
    assert_eq!(ws.read_plaintext(&asset).unwrap(), bytes);
    // The extension came off the file name, lowercased, exactly as a file import derives it.
    assert_eq!(ws.asset(&asset).unwrap().ext, "jpg");
    assert!(ws.asset(&asset).unwrap().sidecar.signature.is_some());

    // Move mode released the staged copy: no plaintext is left in the scratch directory.
    let staging = lib.path().join(".library").join("staging");
    let leftovers = std::fs::read_dir(&staging).map_or(0, std::iter::Iterator::count);
    assert_eq!(leftovers, 0, "the staged plaintext copy must not survive");
}

// ── Move-source release (#671) ──────────────────────────────────────────

/// A canned server: a verdict that is or is not durable for exactly the blobs it is asked
/// about, a receipt that does or does not verify, and a count of how often it was asked.
struct CannedServer {
    durable: bool,
    receipt: bool,
    asked: std::cell::Cell<u32>,
}

impl CannedServer {
    fn new(durable: bool, receipt: bool) -> Self {
        Self {
            durable,
            receipt,
            asked: std::cell::Cell::new(0),
        }
    }
}

impl StorageVerifier for CannedServer {
    fn verify(
        &self,
        asset_id: Uuid,
        blob_hashes: &[Hash32],
    ) -> std::result::Result<crate::library::StorageVerdict, crate::library::VerifierError> {
        self.asked.set(self.asked.get() + 1);
        Ok(crate::library::StorageVerdict {
            asset_id,
            durable: self.durable,
            blobs: blob_hashes
                .iter()
                .map(|hash| crate::library::BlobVerdict {
                    hash: *hash,
                    role: crate::library::BlobRole::Original,
                    stored: self.durable,
                    indexed: self.durable,
                    retrievable: self.durable,
                })
                .collect(),
            checked_at: "2026-10-22T00:00:00Z".into(),
        })
    }

    fn receipt_verified(
        &self,
        _asset_id: Uuid,
        _blob_hashes: &[Hash32],
    ) -> std::result::Result<bool, crate::library::VerifierError> {
        Ok(self.receipt)
    }
}

/// A workspace holding one asset imported in Move mode from `src_dir/name`, its source
/// left in place as every user-file import now leaves it.
fn moved_asset(lib: &Path, src_dir: &Path, bytes: &[u8]) -> (Workspace, Uuid, PathBuf) {
    let mut ws = fast_workspace(lib);
    let album = ws.create_album("Moves").unwrap();
    let src = src_dir.join("moved.jpg");
    fs::write(&src, bytes).unwrap();
    let opts = SignedImportOptions {
        move_source: true,
        defer_source_release: true,
        stack: None,
        enrichment: None,
    };
    let asset = ws.import_asset_with(album, &src, &opts).unwrap().asset_id;
    assert!(src.exists(), "a deferred release leaves the source");
    (ws, asset, src)
}

#[test]
fn a_durable_verdict_with_a_verified_receipt_releases_the_source() {
    let (lib, src_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (ws, asset, src) = moved_asset(lib.path(), src_dir.path(), b"held by the server");

    let decision = ws
        .release_move_source(&CannedServer::new(true, true), asset, &src)
        .unwrap();

    assert_eq!(decision, ReleaseDecision::Release);
    assert!(!src.exists(), "the source is deleted on custody");
    assert_eq!(ws.read_plaintext(&asset).unwrap(), b"held by the server");
}

/// The issue's second case: a non-durable verdict keeps the source.
#[test]
fn a_non_durable_verdict_keeps_the_source() {
    let (lib, src_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (ws, asset, src) = moved_asset(lib.path(), src_dir.path(), b"not yet durable");

    let decision = ws
        .release_move_source(&CannedServer::new(false, true), asset, &src)
        .unwrap();

    assert_eq!(decision, ReleaseDecision::Retain(RetainReason::NotDurable));
    assert_eq!(fs::read(&src).unwrap(), b"not yet durable");
}

#[test]
fn a_missing_receipt_keeps_the_source() {
    let (lib, src_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (ws, asset, src) = moved_asset(lib.path(), src_dir.path(), b"no receipt yet");

    let decision = ws
        .release_move_source(&CannedServer::new(true, false), asset, &src)
        .unwrap();

    assert_eq!(
        decision,
        ReleaseDecision::Retain(RetainReason::ReceiptMissing)
    );
    assert!(src.exists());
}

/// A source edited after import no longer is what the server holds: deleting it would
/// lose the edit, so it is kept even on full custody, and the server is not asked.
#[test]
fn a_source_edited_since_import_is_kept_on_custody() {
    let (lib, src_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (ws, asset, src) = moved_asset(lib.path(), src_dir.path(), b"as imported");
    fs::write(&src, b"edited after").unwrap();
    let server = CannedServer::new(true, true);

    let decision = ws.release_move_source(&server, asset, &src).unwrap();

    assert_eq!(
        decision,
        ReleaseDecision::Retain(RetainReason::ReadBackMismatch)
    );
    assert_eq!(fs::read(&src).unwrap(), b"edited after");
    assert_eq!(
        server.asked.get(),
        0,
        "read-back fails before any server check"
    );
}

/// A library copy that no longer reads back (torn, truncated, deleted) means the source may
/// be the only good copy on this device: it is kept even on full custody.
#[test]
fn a_library_copy_that_does_not_read_back_keeps_the_source() {
    let (lib, src_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (ws, asset, src) = moved_asset(lib.path(), src_dir.path(), b"library copy torn");
    let media = ws.media_path(ws.asset(&asset).unwrap());
    fs::write(&media, b"library copy t").unwrap();

    let decision = ws
        .release_move_source(&CannedServer::new(true, true), asset, &src)
        .unwrap();

    assert_eq!(
        decision,
        ReleaseDecision::Retain(RetainReason::ReadBackMismatch)
    );
    assert_eq!(fs::read(&src).unwrap(), b"library copy torn");
}

#[test]
fn the_library_copy_itself_is_never_released_as_a_source() {
    let (lib, src_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (ws, asset, _src) = moved_asset(lib.path(), src_dir.path(), b"one file");
    let media = ws.media_path(ws.asset(&asset).unwrap());

    let decision = ws
        .release_move_source(&CannedServer::new(true, true), asset, &media)
        .unwrap();

    assert_eq!(
        decision,
        ReleaseDecision::Retain(RetainReason::ReadBackMismatch)
    );
    assert!(media.exists());
}

#[test]
fn an_unknown_asset_is_an_error_and_deletes_nothing() {
    let (lib, src_dir) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (ws, _asset, src) = moved_asset(lib.path(), src_dir.path(), b"kept");

    let result = ws.release_move_source(&CannedServer::new(true, true), Uuid::now_v7(), &src);

    assert!(matches!(result, Err(LifecycleError::NotFound(_))));
    assert!(src.exists());
}

/// A name with no extension still imports — the extension degrades to `bin` rather than
/// the import failing, because a backup tool never refuses bytes it can store.
#[test]
fn import_bytes_without_an_extension_degrades_to_bin() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Camera Roll").unwrap();
    let asset = ws
        .import_bytes(album, "no-extension", b"raw bytes")
        .unwrap();
    assert_eq!(ws.asset(&asset).unwrap().ext, "bin");
    assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
}

/// A video imports as a signed original whose derivatives are *deferred* — the tier table
/// commits to video derivatives and no build links their codec yet — while a file with
/// nothing to derive (an unknown suffix) stays `NotAKnownStill`. A zero-byte file is an
/// original like any other (issue #695).
#[test]
fn video_derivatives_are_deferred_and_unknown_files_have_none_to_defer() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Camera Roll").unwrap();
    let cases: [(&str, &[u8], DerivativeStatus); 4] = [
        (
            "clip.mp4",
            b"\0\0\0\x18ftypmp42 not really",
            DerivativeStatus::DeferredNoCodec,
        ),
        (
            "clip.mov",
            b"\0\0\0\x14ftypqt   not really",
            DerivativeStatus::DeferredNoCodec,
        ),
        (
            "scan.pdf",
            b"%PDF-1.4 not really",
            DerivativeStatus::NotAKnownStill,
        ),
        ("empty.dat", b"", DerivativeStatus::NotAKnownStill),
    ];
    for (name, bytes, expected) in cases {
        let path = src.path().join(name);
        fs::write(&path, bytes).unwrap();
        let import = ws
            .import_asset_with(album, &path, &SignedImportOptions::default())
            .unwrap();
        assert_eq!(import.derivatives, expected, "for {name}");
        assert_eq!(ws.verify(&import.asset_id).unwrap(), VerifyOutcome::Accept);
        let asset = ws.asset(&import.asset_id).unwrap();
        assert_eq!(fs::read(ws.media_path(asset)).unwrap(), bytes, "for {name}");
    }
}

#[test]
fn only_a_video_turns_not_a_known_still_into_a_deferral() {
    use DerivativeStatus::{DecodeFailed, Decoded, DeferredNoCodec, NotAKnownStill};
    assert_eq!(
        video_derivatives_deferred(NotAKnownStill, "video/mp4"),
        DeferredNoCodec
    );
    assert_eq!(
        video_derivatives_deferred(NotAKnownStill, "application/octet-stream"),
        NotAKnownStill
    );
    for status in [Decoded, DecodeFailed, DeferredNoCodec] {
        assert_eq!(video_derivatives_deferred(status, "video/mp4"), status);
    }
}

/// The rewrite guard, directly. A media path holding bytes of a *different length* than
/// the correct plaintext is overwritten; a media path already holding the correct bytes is
/// left alone, its mtime intact — the safety property the migration and every metadata
/// edit rely on.
#[test]
fn write_asset_files_overwrites_wrong_bytes_and_leaves_correct_bytes_alone() {
    use std::time::{Duration, SystemTime};

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    let correct = b"\xFF\xD8\xFF the signed bytes".to_vec();
    fs::write(&img, &correct).unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    let id = ws.import_asset(album, &img).unwrap();
    let media = ws.media_path(ws.asset(&id).unwrap());

    // Wrong bytes at the media path (truncated, then longer): overwritten.
    for wrong in [
        &correct[..correct.len() / 2],
        b"\xFF\xD8\xFF not the signed bytes at all",
    ] {
        fs::write(&media, wrong).unwrap();
        ws.write_asset_files(ws.asset(&id).unwrap(), &correct)
            .unwrap();
        assert_eq!(
            fs::read(&media).unwrap(),
            correct,
            "wrong bytes were rewritten"
        );
    }

    // Correct bytes already there: not touched, which the mtime proves.
    let long_ago = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    fs::File::options()
        .write(true)
        .open(&media)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();
    ws.write_asset_files(ws.asset(&id).unwrap(), &correct)
        .unwrap();
    assert_eq!(fs::metadata(&media).unwrap().modified().unwrap(), long_ago);
    assert_eq!(fs::read(&media).unwrap(), correct);
    assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept);
}

/// The guard's documented limit, pinned so it cannot change silently: the decision is
/// made from the buffer with no second read of the disk, so a *same-length* file whose
/// bytes differ from a correct buffer is not detected. No caller in the tree can produce
/// that case — `append_lifecycle` passes the bytes it just read from this very path, a
/// create's source either is this path or the path does not exist yet, and `import_backup`
/// restores into a workspace where the path does not exist — and a wrong original is
/// caught by `verify`, never silently accepted. A caller that could meet the case must
/// decide the overwrite itself (see the caller rule on `write_asset_files`).
#[test]
fn write_asset_files_trusts_a_same_length_buffer_over_the_disk() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    let correct = b"\xFF\xD8\xFF the signed bytes".to_vec();
    fs::write(&img, &correct).unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    let id = ws.import_asset(album, &img).unwrap();
    let media = ws.media_path(ws.asset(&id).unwrap());

    let same_length = b"\xFF\xD8\xFF THE SIGNED BYTES".to_vec();
    assert_eq!(same_length.len(), correct.len());
    fs::write(&media, &same_length).unwrap();
    ws.write_asset_files(ws.asset(&id).unwrap(), &correct)
        .unwrap();
    assert_eq!(
        fs::read(&media).unwrap(),
        same_length,
        "the buffer is trusted; no second read decides this"
    );
    // ...and the wrong original does not pass verification.
    assert_ne!(ws.verify(&id).unwrap(), VerifyOutcome::Accept);
}

/// A lifecycle write — caption, tag, soft-delete, restore — touches the signed artifacts
/// and nothing else: the original is neither read nor rewritten. The original is made
/// unreadable for the duration (so any read would fail the edit), and its mtime and bytes
/// are unchanged afterwards.
#[test]
fn metadata_edits_neither_read_nor_rewrite_the_original() {
    use std::time::{Duration, SystemTime};

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    let bytes = b"\xFF\xD8\xFF never touched by an edit".to_vec();
    fs::write(&img, &bytes).unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    let id = ws.import_asset(album, &img).unwrap();
    let media = ws.media_path(ws.asset(&id).unwrap());

    let long_ago = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    fs::File::options()
        .write(true)
        .open(&media)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();
    #[cfg(unix)]
    let readable = {
        use std::os::unix::fs::PermissionsExt;
        let readable = fs::metadata(&media).unwrap().permissions();
        fs::set_permissions(&media, fs::Permissions::from_mode(0o000)).unwrap();
        readable
    };

    ws.set_caption(&id, "edited without touching the bytes")
        .unwrap();
    ws.tag_add(&id, "untouched").unwrap();
    ws.soft_delete(&id, 30).unwrap();
    ws.restore(&id).unwrap();

    #[cfg(unix)]
    fs::set_permissions(&media, readable).unwrap();
    assert_eq!(fs::metadata(&media).unwrap().modified().unwrap(), long_ago);
    assert_eq!(fs::read(&media).unwrap(), bytes);
    assert_eq!(ws.asset(&id).unwrap().chain.records().len(), 5);
    assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept);
}

// ── Importer-formed stacks ──────────────────────────────────────────

/// Write `n` distinct fixture files into `dir` and return their paths.
fn fixture_files(dir: &Path, n: usize) -> Vec<std::path::PathBuf> {
    (0..n)
        .map(|i| {
            let p = dir.join(format!("stacked-{i}.jpg"));
            let mut bytes = vec![0xFF, 0xD8, 0xFF];
            bytes.extend_from_slice(format!("stack fixture asset {i}").as_bytes());
            fs::write(&p, &bytes).unwrap();
            p
        })
        .collect()
}

/// **The acceptance case.** A stack formed by the *importer* — never touched by
/// hand — is written into the signed sidecar, so it survives losing `index/library.sqlite`
/// entirely and rebuilding from the artifacts on disk.
///
/// Before this change the placement existed only as index columns, so this test's `rebuild`
/// came back with both members loose in the timeline and no `asset_stacks` row at all.
#[test]
fn importer_formed_stack_survives_index_loss_and_rebuild() {
    use capsule_core::sidecar::sidecar_v1::StackRole;

    use crate::library::{open_library, rebuild_index};

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let root = lib.path().to_path_buf();
    let files = fixture_files(src.path(), 2);
    let stack_id = Uuid::now_v7();

    let mut ws = fast_workspace(&root);
    let album = ws.create_album("Trip").unwrap();
    let mut ids = Vec::new();
    for (seq, path) in files.iter().enumerate() {
        let opts = SignedImportOptions {
            stack: Some(StackMembership {
                stack_id,
                stack_type: capsule_core::domain::StackType::RawJpeg,
                role: if seq == 0 {
                    StackRole::Primary
                } else {
                    StackRole::Member
                },
                member_index: Some(seq as u32),
            }),
            ..Default::default()
        };
        ids.push(ws.import_asset_with(album, path, &opts).unwrap().asset_id);
    }
    let (primary, member) = (ids[0], ids[1]);

    // The register is on disk, inside the signed sidecar, and the asset still verifies.
    let sidecar = &ws.asset(&member).unwrap().sidecar;
    let membership = sidecar
        .stack_membership
        .get()
        .and_then(Option::as_ref)
        .expect("the importer wrote the stack register");
    assert_eq!(membership.stack_id, stack_id);
    assert_eq!(membership.role, StackRole::Member);
    assert_eq!(membership.member_index, Some(1));
    assert_eq!(ws.verify(&member).unwrap(), VerifyOutcome::Accept);

    // ...and it is projected onto the index the same way a rebuild projects it.
    let timeline = ws.db().query_timeline(0, 100).unwrap();
    assert_eq!(timeline.len(), 1, "only the primary is in the timeline");
    assert_eq!(timeline[0].uuid, primary.to_string());

    // Recovery: delete the index outright and rebuild it from the artifacts on disk.
    drop(ws);
    fs::remove_file(root.join("index/library.sqlite")).unwrap();
    let library = open_library(&root).unwrap();
    rebuild_index(&library).unwrap();

    for (id, hidden) in [(primary, false), (member, true)] {
        let row = library
            .db
            .find_by_uuid(&id.to_string())
            .unwrap()
            .expect("asset is back in the rebuilt index");
        assert_eq!(row.stack_id.as_deref(), Some(stack_id.to_string().as_str()));
        assert_eq!(row.is_stack_hidden, hidden);
    }
    let rebuilt = library.db.query_timeline(0, 100).unwrap();
    assert_eq!(rebuilt.len(), 1, "the stack still collapses to its primary");
    assert_eq!(rebuilt[0].uuid, primary.to_string());
    assert_eq!(
        library
            .db
            .list_stack_members(&stack_id.to_string())
            .unwrap()
            .len(),
        2,
        "the `stack_members` rows are reconstructed from the registers"
    );

    // Reopening the workspace reads the placement off the sidecar, not the index.
    drop(library);
    let ws = Workspace::open(
        &root,
        b"passphrase",
        capsule_core::crypto::primitives::Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        },
    )
    .unwrap();
    let placement = ws.asset(&member).unwrap().stack.as_ref().unwrap();
    assert_eq!(placement.stack_id, stack_id.to_string());
    assert!(placement.hidden);
}

/// The absent-key discipline, at the import path. An asset imported **without** a stack
/// leaves `stack_membership` at its never-written default, which is wire-absent — so its
/// signed sidecar encodes byte-identically to one written before the register existed.
#[test]
fn import_without_a_stack_leaves_the_stack_register_wire_absent() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("standalone.jpg");
    fs::write(&img, b"\xFF\xD8\xFF unstacked photo").unwrap();

    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    let id = ws.import_asset(album, &img).unwrap();

    let bytes = fs::read(ws.sidecar_path(ws.asset(&id).unwrap())).unwrap();
    for needle in [
        b"stack_membership".as_slice(),
        b"cull".as_slice(),
        b"hidden".as_slice(),
    ] {
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "a never-written register must not reach the wire"
        );
    }

    let parsed = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
    assert_eq!(parsed.stack_membership, Lww::new());

    // The proof itself: substituting the literal never-written field value (`Lww::new()`)
    // re-encodes to the same bytes, so the register contributes nothing to an unstacked
    // asset's signed sidecar and no existing signature is invalidated.
    let mut pre_change = parsed.clone();
    pre_change.stack_membership = Lww::new();
    assert_eq!(pre_change.to_canonical_vec(), bytes);
    assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept);
}

/// The register is only *written* when there is a stack: `stack_membership_register` is the
/// single decision point, and its `None` arm is the wire-absent default.
#[test]
fn stack_membership_register_is_absent_without_a_stack() {
    let device = Uuid::from_u128(0xD1);
    assert_eq!(stack_membership_register(None, device), Lww::new());

    let membership = StackMembership {
        stack_id: Uuid::now_v7(),
        stack_type: capsule_core::domain::StackType::Burst,
        role: capsule_core::sidecar::sidecar_v1::StackRole::Primary,
        member_index: Some(0),
    };
    let written = stack_membership_register(Some(membership.clone()), device);
    assert_eq!(written.get(), Some(&Some(membership)));
}

#[test]
fn end_to_end_data_plane() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    fs::write(
        &img,
        b"\xFF\xD8\xFF\xE0 fake jpeg bytes for the e2e test \x00\x01\x02",
    )
    .unwrap();

    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();

    // Import → encrypt → manifest+provenance+signed sidecar → verify_asset(Accept).
    let asset = ws.import_asset(album, &img).unwrap();
    assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);

    // The signed sidecar + provenance + plaintext exist on disk.
    let st = ws.asset(&asset).unwrap();
    assert!(ws.media_path(st).exists());
    assert!(ws.sidecar_path(st).exists());
    assert!(ws.provenance_path(st).exists());
    assert!(st.sidecar.verify(&ws.account.user_ik.verifying_key()));

    // CRDT metadata edits advance the chain and re-sign the sidecar.
    ws.tag_add(&asset, "vacation").unwrap();
    ws.set_caption(&asset, "sunset over the bay").unwrap();
    let st = ws.asset(&asset).unwrap();
    assert!(st.sidecar.tags_user.value().contains("vacation"));
    assert_eq!(st.sidecar.caption.get().unwrap(), "sunset over the bay");
    assert_eq!(st.chain.records().len(), 3); // create + 2 metadata-update
    ProvenanceChain::verify_walk(st.chain.records()).unwrap();

    // Soft delete + restore append lifecycle records.
    ws.soft_delete(&asset, 30).unwrap();
    ws.restore(&asset).unwrap();
    let st = ws.asset(&asset).unwrap();
    assert_eq!(st.chain.records().len(), 5);
    // The delete record carries a retention window; it remains in the chain after restore.
    let actions: Vec<_> = st
        .chain
        .records()
        .iter()
        .map(|r| r.manifest.core.action)
        .collect();
    assert_eq!(
        actions,
        vec![
            Action::Create,
            Action::MetadataUpdate,
            Action::MetadataUpdate,
            Action::Delete,
            Action::TrashRestore
        ]
    );

    // Backup → restore into a FRESH library (new device, verifying against the
    // exporter's published key) → byte-equal plaintext.
    let backup_path = src.path().join("backup.tar");
    ws.export_backup_with_params(&backup_path, b"recovery-pass", FAST_PARAMS)
        .unwrap();
    let exporter_pub = ws.exporter_verifying_key();

    let fresh = TempDir::new().unwrap();
    let mut ws2 = fast_workspace(fresh.path());
    let added = ws2
        .import_backup(&backup_path, b"recovery-pass", &exporter_pub)
        .unwrap();
    assert_eq!(added, 1);
    assert_eq!(
        ws2.read_plaintext(&asset).unwrap(),
        ws.read_plaintext(&asset).unwrap(),
        "restored library must be byte-equal to the source"
    );

    // A wrong exporter key (untrusted device) is refused.
    let imposter = HybridSigningKey::generate().verifying_key();
    let mut ws3 = fast_workspace(TempDir::new().unwrap().path());
    assert!(
        ws3.import_backup(&backup_path, b"recovery-pass", &imposter)
            .is_err()
    );
}

#[test]
fn crypto_lifecycle_writes_through_to_the_index() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    fs::write(&img, b"\xFF\xD8\xFF indexed photo").unwrap();

    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    let id = ws.import_asset(album, &img).unwrap();
    let uuid = id.to_string();

    // The import is queryable in the timeline, tagged to its album.
    let timeline = ws.db().query_timeline(0, 100).unwrap();
    assert_eq!(timeline.len(), 1);
    assert_eq!(timeline[0].uuid, uuid);
    assert_eq!(
        timeline[0].album_id.as_deref(),
        Some(album.to_string().as_str())
    );

    // It recorded a device-owned `original` representation, exempt from eviction.
    let reps = ws.db().representations_for(&uuid).unwrap();
    assert_eq!(reps.len(), 1);
    assert_eq!(reps[0].tier, "original");
    assert!(reps[0].is_owned_original);
    assert!(
        ws.db().eviction_candidates(0).unwrap().is_empty(),
        "an owned original is never an eviction candidate"
    );

    // A tag edit re-syncs into the index.
    ws.tag_add(&id, "vacation").unwrap();
    assert_eq!(
        ws.db().tags_for(&uuid).unwrap(),
        vec!["vacation".to_string()]
    );

    // Soft-delete hides it from the timeline; restore brings it back (deletion state is
    // derived from the provenance chain).
    ws.soft_delete(&id, 30).unwrap();
    assert!(ws.db().query_timeline(0, 100).unwrap().is_empty());
    ws.restore(&id).unwrap();
    assert_eq!(ws.db().query_timeline(0, 100).unwrap().len(), 1);
}

/// Exporter tags reach `tags_user` through the label rule: padded and NFD titles land
/// normalized (and an NFD title collapses onto its NFC twin), while empty, whitespace-only
/// and over-long titles are dropped without failing the import.
#[test]
fn enrichment_tags_follow_the_label_rule() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    fs::write(&img, b"\xFF\xD8\xFF enriched tag photo").unwrap();

    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    let opts = SignedImportOptions {
        enrichment: Some(SidecarEnrichment {
            tags: vec![
                "  Vacation 2021 ".to_string(),
                "Cafe\u{301}".to_string(),
                "Caf\u{e9}".to_string(),
                String::new(),
                " \t ".to_string(),
                "x".repeat(257),
                "y".repeat(256),
            ],
            ..Default::default()
        }),
        ..Default::default()
    };
    let id = ws
        .import_asset_with(album, &img, &opts)
        .expect("an unusable tag must not fail the import")
        .asset_id;

    let tags = ws.asset(&id).unwrap().sidecar.tags_user.value();
    let expected: std::collections::BTreeSet<String> = [
        "Vacation 2021".to_string(),
        "Caf\u{e9}".to_string(),
        "y".repeat(256),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        tags.into_iter().collect::<std::collections::BTreeSet<_>>(),
        expected
    );
}

// ── The index projection of capture time ─────────────────────────────────────

/// The live `assets` row is projected from the **signed sidecar's** capture timestamp, as
/// a rebuild projects it — not from the `capture_utc` shard, which is fixed at import and
/// stays put through a capture-time correction. Equal at import; this test drives the two
/// apart the way `set_capture_timestamp` will and asserts the row follows the sidecar.
#[test]
fn the_live_index_row_projects_capture_time_from_the_sidecar() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("p.jpg");
    fs::write(&img, b"\xFF\xD8\xFF capture projection bytes").unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("A").unwrap();
    let id = ws.import_asset(album, &img).unwrap();

    // At import the shard and the sidecar name the same instant.
    let asset = ws.assets.get(&id).unwrap();
    let row = asset_row_from_state(asset);
    assert_eq!(row.capture_timestamp, asset.capture_utc);
    assert_eq!(row.capture_utc, Some(asset.capture_utc));
    assert_eq!(
        rfc3339_to_secs(&asset.sidecar.capture_timestamp),
        asset.capture_utc
    );

    // Drive them apart in memory: the sidecar says 2001, the shard still says now.
    let corrected = 1_000_000_000;
    ws.assets.get_mut(&id).unwrap().sidecar.capture_timestamp = capture_rfc3339(corrected);
    let asset = ws.assets.get(&id).unwrap();
    assert_ne!(asset.capture_utc, corrected, "the shard is untouched");
    let row = asset_row_from_state(asset);
    assert_eq!(
        row.capture_timestamp, corrected,
        "the row follows the sidecar"
    );
    assert_eq!(row.capture_utc, Some(corrected));

    // An unparseable sidecar timestamp indexes as the epoch, as `rebuild_index` does.
    ws.assets.get_mut(&id).unwrap().sidecar.capture_timestamp = "not a timestamp".into();
    let row = asset_row_from_state(ws.assets.get(&id).unwrap());
    assert_eq!(row.capture_timestamp, 0);
    assert_eq!(row.capture_utc, Some(0));
}
