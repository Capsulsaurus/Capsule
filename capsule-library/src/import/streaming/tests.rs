use std::cell::Cell;
use std::fs;
use std::path::Path;

use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::domain::ImportMode;
use capsule_core::metadata::AssetType;
use jiff::Timestamp;
use tempfile::TempDir;
use uuid::Uuid;

use super::*;
use crate::import::importers::{
    ExtractedImport, ExtractedMetadata, FoldSource, GeoPoint, SourceEntry,
};
use crate::import::planner::{ImportConfig, plan};
use crate::import::scanner::scan;
use crate::library::{BlobRole, BlobVerdict, StorageVerdict, VerifierError};

fn noop_event(_: StreamingEvent) {}

/// A workspace with fast Argon2 params + its default album created (as the executor tests do).
fn signed_workspace(dir: &Path) -> Workspace {
    let mut ws = Workspace::create_with_params(
        dir,
        b"passphrase",
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        },
    )
    .unwrap();
    let default = ws.default_album_id();
    ws.create_album_with_id(default, "Imports").unwrap();
    ws
}

// ── Injected upload seam mocks ──────────────────────────────────────────────

/// Always accepts the upload — the connected steady state.
struct OkUploader;
impl AssetUploader for OkUploader {
    fn upload(&self, _: &StreamedImport) -> Result<(), UploadHalt> {
        Ok(())
    }
}

/// Drops the connection on the Nth upload (1-based), accepting every other call. Models a
/// mid-stream disconnect.
struct DisconnectOnNth {
    n: u32,
    seen: Cell<u32>,
}
impl AssetUploader for DisconnectOnNth {
    fn upload(&self, _: &StreamedImport) -> Result<(), UploadHalt> {
        let seen = self.seen.get() + 1;
        self.seen.set(seen);
        if seen == self.n {
            Err(UploadHalt::Disconnected("socket closed".into()))
        } else {
            Ok(())
        }
    }
}

// ── Injected verify seam mock (the StorageVerifier) ────────────────────

/// Returns a canned verdict + receipt fact, building one safely-stored (or not) blob verdict
/// per declared hash so `release_is_safe`'s per-blob re-check is exercised.
struct MockVerifier {
    durable: bool,
    receipt: bool,
}
impl StorageVerifier for MockVerifier {
    fn verify(
        &self,
        asset_id: Uuid,
        blob_hashes: &[Hash32],
    ) -> Result<StorageVerdict, VerifierError> {
        let blobs = blob_hashes
            .iter()
            .map(|h| BlobVerdict {
                hash: *h,
                role: BlobRole::Original,
                stored: self.durable,
                indexed: self.durable,
                retrievable: self.durable,
            })
            .collect();
        Ok(StorageVerdict {
            asset_id,
            durable: self.durable,
            blobs,
            checked_at: "2026-07-10T00:00:00Z".into(),
        })
    }

    fn receipt_verified(&self, _: Uuid, _: &[Hash32]) -> Result<bool, VerifierError> {
        Ok(self.receipt)
    }
}

fn write_sources(dir: &Path, n: usize) -> Vec<std::path::PathBuf> {
    (0..n)
        .map(|i| {
            let p = dir.join(format!("photo_{i}.jpg"));
            // Distinct contents so each is a distinct hash (no accidental dedup).
            fs::write(&p, format!("streamed jpeg content number {i}").into_bytes()).unwrap();
            p
        })
        .collect()
}

/// Streaming release gating (smoke): a `durable` verdict + verified receipt releases each
/// local original (its owned-original row dropped, its file deleted) while the asset row
/// survives; a non-`durable` verdict leaves every local copy in place.
#[test]
fn streaming_release_gates_on_durable_verdict() {
    // ── Durable: every original is released. ───────────────────────────────
    {
        let src = TempDir::new().unwrap();
        let lib = TempDir::new().unwrap();
        write_sources(src.path(), 3);
        let mut ws = signed_workspace(lib.path());
        let plan = plan(
            &scan(&[src.path().to_path_buf()]).unwrap(),
            ws.db(),
            &ImportConfig::default(),
        )
        .unwrap();
        assert_eq!(plan.counts.to_import, 3);

        let verifier = MockVerifier {
            durable: true,
            receipt: true,
        };
        let report = execute_streaming(
            &plan,
            &mut ws,
            StreamingOptions {
                config: &ImportConfig::default(),
                source: &SourceMetadataIndex::empty(),
                uploader: &OkUploader,
                verifier: &verifier,
                headroom_margin: 0,
                cancel: &CancellationToken::new(),
            },
            noop_event,
        )
        .unwrap();

        assert_eq!(report.released_count(), 3, "all three released on durable");
        assert_eq!(report.retained_count(), 0);
        assert!(!report.is_halted());
        // The asset rows survive release (server-only, re-fetchable); the owned-original
        // representation rows are gone (bytes released) and the media files deleted.
        assert_eq!(ws.asset_ids().len(), 3);
        for id in ws.asset_ids() {
            let reps = ws.db().representations_for(&id.to_string()).unwrap();
            assert!(
                reps.is_empty(),
                "owned-original row should be dropped after release"
            );
        }
    }

    // ── Not durable: nothing is released; every local copy is retained. ────
    {
        let src = TempDir::new().unwrap();
        let lib = TempDir::new().unwrap();
        write_sources(src.path(), 3);
        let mut ws = signed_workspace(lib.path());
        let plan = plan(
            &scan(&[src.path().to_path_buf()]).unwrap(),
            ws.db(),
            &ImportConfig::default(),
        )
        .unwrap();

        let verifier = MockVerifier {
            durable: false,
            receipt: true,
        };
        let report = execute_streaming(
            &plan,
            &mut ws,
            StreamingOptions {
                config: &ImportConfig::default(),
                source: &SourceMetadataIndex::empty(),
                uploader: &OkUploader,
                verifier: &verifier,
                headroom_margin: 0,
                cancel: &CancellationToken::new(),
            },
            noop_event,
        )
        .unwrap();

        assert_eq!(
            report.released_count(),
            0,
            "a non-durable verdict releases nothing"
        );
        assert_eq!(report.retained_count(), 3);
        for o in &report.outcomes {
            assert_eq!(o.state, StreamedState::Retained(RetainReason::NotDurable));
        }
        // Local originals remain: owned-original rows present and the files still on disk.
        for id in ws.asset_ids() {
            let reps = ws.db().representations_for(&id.to_string()).unwrap();
            assert_eq!(reps.len(), 1, "owned original retained when not durable");
            assert!(
                Path::new(&reps[0].path).exists(),
                "retained file must stay on disk"
            );
        }
    }
}

/// Move-mode streaming release: the external source is deleted only on a durable verdict, and
/// retained otherwise (the verify-before-destroy rule per asset).
#[test]
fn streaming_move_source_released_only_on_durable() {
    // Durable → external source deleted.
    let src = TempDir::new().unwrap();
    let lib = TempDir::new().unwrap();
    let sources = write_sources(src.path(), 1);
    let mut ws = signed_workspace(lib.path());
    let config = ImportConfig {
        import_mode: ImportMode::Move,
        ..Default::default()
    };
    let plan_a = plan(
        &scan(&[src.path().to_path_buf()]).unwrap(),
        ws.db(),
        &config,
    )
    .unwrap();
    let report = execute_streaming(
        &plan_a,
        &mut ws,
        StreamingOptions {
            config: &config,
            source: &SourceMetadataIndex::empty(),
            uploader: &OkUploader,
            verifier: &MockVerifier {
                durable: true,
                receipt: true,
            },
            headroom_margin: 0,
            cancel: &CancellationToken::new(),
        },
        noop_event,
    )
    .unwrap();
    assert_eq!(report.released_count(), 1);
    assert!(
        !sources[0].exists(),
        "Move source deleted on durable release"
    );

    // Not durable → external source retained.
    let src2 = TempDir::new().unwrap();
    let lib2 = TempDir::new().unwrap();
    let sources2 = write_sources(src2.path(), 1);
    let mut ws2 = signed_workspace(lib2.path());
    let plan2 = plan(
        &scan(&[src2.path().to_path_buf()]).unwrap(),
        ws2.db(),
        &config,
    )
    .unwrap();
    execute_streaming(
        &plan2,
        &mut ws2,
        StreamingOptions {
            config: &config,
            source: &SourceMetadataIndex::empty(),
            uploader: &OkUploader,
            verifier: &MockVerifier {
                durable: false,
                receipt: true,
            },
            headroom_margin: 0,
            cancel: &CancellationToken::new(),
        },
        noop_event,
    )
    .unwrap();
    assert!(
        sources2[0].exists(),
        "Move source retained when not durable"
    );
}

/// Accepts every upload, and overwrites the Move source while it is in flight — a source
/// edited between its import and its release.
struct EditsSourceDuringUpload;
impl AssetUploader for EditsSourceDuringUpload {
    fn upload(&self, imported: &StreamedImport) -> Result<(), UploadHalt> {
        if let Some(src) = &imported.move_source {
            fs::write(src, b"edited while uploading").unwrap();
        }
        Ok(())
    }
}

/// The read-back half of invariant 7 holds in streaming mode too: a source that no longer
/// hashes to the signed original is kept on full custody, and so is the local original —
/// the one copy of what was imported known to be intact.
#[test]
fn streaming_keeps_a_move_source_that_fails_the_read_back() {
    let src = TempDir::new().unwrap();
    let lib = TempDir::new().unwrap();
    let sources = write_sources(src.path(), 1);
    let mut ws = signed_workspace(lib.path());
    let config = ImportConfig {
        import_mode: ImportMode::Move,
        ..Default::default()
    };
    let plan_result = plan(
        &scan(&[src.path().to_path_buf()]).unwrap(),
        ws.db(),
        &config,
    )
    .unwrap();

    let report = execute_streaming(
        &plan_result,
        &mut ws,
        StreamingOptions {
            config: &config,
            source: &SourceMetadataIndex::empty(),
            uploader: &EditsSourceDuringUpload,
            verifier: &MockVerifier {
                durable: true,
                receipt: true,
            },
            headroom_margin: 0,
            cancel: &CancellationToken::new(),
        },
        noop_event,
    )
    .unwrap();

    assert_eq!(report.released_count(), 0);
    assert_eq!(
        report.outcomes[0].state,
        StreamedState::Retained(RetainReason::ReadBackMismatch)
    );
    assert_eq!(fs::read(&sources[0]).unwrap(), b"edited while uploading");
    let asset = report.outcomes[0].asset_id.unwrap();
    assert!(
        ws.read_plaintext(&asset).is_ok(),
        "the local original is kept too"
    );
}

/// Streaming halt-on-disconnect (smoke): dropping the connection mid-stream stops the window
/// from admitting new source files (bounded local growth), and a reconnect run re-derives the
/// remaining work without re-importing completed assets.
#[test]
fn streaming_halts_on_disconnect_then_resumes_without_reimport() {
    let src = TempDir::new().unwrap();
    let lib = TempDir::new().unwrap();
    write_sources(src.path(), 3);
    let mut ws = signed_workspace(lib.path());
    let config = ImportConfig::default();

    // Disconnect on the 2nd upload: asset #1 imports+releases, asset #2 imports then the
    // window pauses, asset #3 is never admitted.
    let uploader = DisconnectOnNth {
        n: 2,
        seen: Cell::new(0),
    };
    let plan1 = plan(
        &scan(&[src.path().to_path_buf()]).unwrap(),
        ws.db(),
        &config,
    )
    .unwrap();
    assert_eq!(plan1.counts.to_import, 3);

    let report = execute_streaming(
        &plan1,
        &mut ws,
        StreamingOptions {
            config: &config,
            source: &SourceMetadataIndex::empty(),
            uploader: &uploader,
            verifier: &MockVerifier {
                durable: true,
                receipt: true,
            },
            headroom_margin: 0,
            cancel: &CancellationToken::new(),
        },
        noop_event,
    )
    .unwrap();

    assert_eq!(
        report.halted,
        Some(StreamHalt::Disconnected("socket closed".into())),
        "the window halts on disconnect"
    );
    assert_eq!(
        report.released_count(),
        1,
        "the first asset uploaded+released before the drop"
    );
    assert_eq!(
        report.retained_count(),
        1,
        "the in-flight asset is retained locally"
    );
    // Bounded local growth: only the two admitted assets ever entered the library; the third
    // source file was never imported.
    assert_eq!(
        ws.asset_ids().len(),
        2,
        "no unbounded local growth past the window"
    );

    // ── Reconnect: re-derive the plan and resume. ───────────────────────────
    // The deterministic planner finds the two already-imported assets in the index (by hash)
    // and skips them; only the never-admitted third file is imported now.
    let plan2 = plan(
        &scan(&[src.path().to_path_buf()]).unwrap(),
        ws.db(),
        &config,
    )
    .unwrap();
    assert_eq!(
        plan2.counts.to_import, 1,
        "only the un-imported file remains to import"
    );
    assert_eq!(
        plan2.counts.duplicates, 2,
        "completed assets are skipped, never re-imported"
    );

    let report2 = execute_streaming(
        &plan2,
        &mut ws,
        StreamingOptions {
            config: &config,
            source: &SourceMetadataIndex::empty(),
            uploader: &OkUploader,
            verifier: &MockVerifier {
                durable: true,
                receipt: true,
            },
            headroom_margin: 0,
            cancel: &CancellationToken::new(),
        },
        noop_event,
    )
    .unwrap();
    assert!(!report2.is_halted());
    assert_eq!(report2.released_count(), 1, "the resumed file completes");
    // Exactly one new import happened (2 → 3): completed assets were not re-imported.
    assert_eq!(ws.asset_ids().len(), 3);
}

/// The minimum-headroom hard error: when the largest single asset cannot fit within available
/// space (here forced with an impossibly large headroom margin), the run fails *before* any
/// file is imported.
#[test]
fn insufficient_headroom_is_a_hard_error_before_import() {
    let src = TempDir::new().unwrap();
    let lib = TempDir::new().unwrap();
    write_sources(src.path(), 1);
    let mut ws = signed_workspace(lib.path());
    let config = ImportConfig::default();
    let plan = plan(
        &scan(&[src.path().to_path_buf()]).unwrap(),
        ws.db(),
        &config,
    )
    .unwrap();

    // A headroom margin larger than any real volume forces the largest-asset check to fail.
    let err = execute_streaming(
        &plan,
        &mut ws,
        StreamingOptions {
            config: &config,
            source: &SourceMetadataIndex::empty(),
            uploader: &OkUploader,
            verifier: &MockVerifier {
                durable: true,
                receipt: true,
            },
            headroom_margin: u64::MAX,
            cancel: &CancellationToken::new(),
        },
        noop_event,
    )
    .unwrap_err();

    assert!(
        matches!(err, StreamingError::InsufficientHeadroom { .. }),
        "expected a hard headroom error, got {err:?}"
    );
    // Nothing was imported — the failure surfaced at pre-flight.
    assert_eq!(
        ws.asset_ids().len(),
        0,
        "no file imported when headroom is insufficient"
    );
}

/// **Staged × streaming exclusion (by construction).** A run configured with the
/// [`UploadPolicy::Staged`](crate::import::UploadPolicy) policy can never
/// enter the streaming window: `execute_streaming` refuses it *before* any file
/// is imported, mirroring the planner's confirmation-time rejection. (SSoT:
/// transfer upload doc — staged uploads are mutually exclusive with streaming.)
#[test]
fn staged_policy_cannot_enter_the_streaming_window() {
    use capsule_core::import::upload::{StagedStreamingConflict, UploadPolicy};

    let src = TempDir::new().unwrap();
    let lib = TempDir::new().unwrap();
    write_sources(src.path(), 2);
    let mut ws = signed_workspace(lib.path());
    let config = ImportConfig {
        upload_policy: UploadPolicy::Staged,
        ..Default::default()
    };
    let plan = plan(
        &scan(&[src.path().to_path_buf()]).unwrap(),
        ws.db(),
        &config,
    )
    .unwrap();

    let err = execute_streaming(
        &plan,
        &mut ws,
        StreamingOptions {
            config: &config,
            source: &SourceMetadataIndex::empty(),
            uploader: &OkUploader,
            verifier: &MockVerifier {
                durable: true,
                receipt: true,
            },
            headroom_margin: 0,
            cancel: &CancellationToken::new(),
        },
        noop_event,
    )
    .unwrap_err();

    assert!(
        matches!(
            err,
            StreamingError::StagedPolicyConflict(StagedStreamingConflict)
        ),
        "a staged policy must be refused before the window, got {err:?}"
    );
    // The refusal is before any import — no local growth, no partial run.
    assert_eq!(
        ws.asset_ids().len(),
        0,
        "no file imported when the staged policy is refused"
    );
}

// ── Third-party enrichment through the window ───────────────────────
//
// The adapter's folded metadata reaches the signed sidecar through the streaming window
// exactly as through the *bulk* executor — a streamed Takeout import must not silently
// discard what the adapter recovered. These two tests are the contract for that:
//
// - `a_streamed_import_carries_the_exporter_metadata_into_the_signed_sidecar` — a file the
//   index covers lands its exporter caption, favorite rating, album tags, capture time and
//   GPS inside the signed sidecar, and still verifies.
// - `a_streamed_import_the_index_does_not_cover_is_unenriched` — the plain-filesystem
//   arm: the wrapper's empty index leaves every enriched register at its default.

/// One entry covering `path` with a full exporter record.
fn covering_index(path: &Path) -> SourceMetadataIndex {
    let entry = SourceEntry {
        candidate: ImportCandidate {
            source_paths: vec![path.to_path_buf()],
            detected_type: AssetType::Photo,
            stack_type: None,
            detection_method: None,
            detection_key: None,
            members: vec![(path.to_path_buf(), MemberRole::Primary)],
        },
        metadata: ExtractedMetadata {
            taken_time: Timestamp::from_second(1_609_502_400).ok(),
            taken_time_source: FoldSource::Exporter,
            gps: Some(GeoPoint {
                lat: 21.3,
                lon: -157.8,
            }),
            gps_source: FoldSource::Exporter,
            description: Some("Snowy morning".into()),
            favorite: true,
            albums: vec!["Vacation 2021".to_string()],
        },
    };
    SourceMetadataIndex::from_extracted(&ExtractedImport {
        entries: vec![entry],
    })
}

/// Stream-import everything under `src` with `index` attached. The verifier withholds the
/// `durable` verdict on purpose: release deletes the local original, and the enrichment
/// assertions below end with a full `verify_asset` pass, which needs those bytes. The window
/// itself — import → upload → verify → gate — runs in full either way.
fn stream_with(index: &SourceMetadataIndex, src: &Path, ws: &mut Workspace) -> StreamingReport {
    let config = ImportConfig::default();
    let plan = plan(&scan(&[src.to_path_buf()]).unwrap(), ws.db(), &config).unwrap();
    execute_streaming(
        &plan,
        ws,
        StreamingOptions {
            config: &config,
            source: index,
            uploader: &OkUploader,
            verifier: &MockVerifier {
                durable: false,
                receipt: true,
            },
            headroom_margin: 0,
            cancel: &CancellationToken::new(),
        },
        noop_event,
    )
    .unwrap()
}

#[test]
fn a_streamed_import_carries_the_exporter_metadata_into_the_signed_sidecar() {
    let src = TempDir::new().unwrap();
    let lib = TempDir::new().unwrap();
    let photo = src.path().join("photo_0.jpg");
    fs::write(&photo, b"streamed bytes with no exif of their own").unwrap();
    let mut ws = signed_workspace(lib.path());

    let report = stream_with(&covering_index(&photo), src.path(), &mut ws);
    assert_eq!(report.retained_count(), 1, "the window ran the whole step");

    let id = ws.asset_ids().into_iter().next().expect("one asset");
    let sidecar = &ws.asset(&id).expect("asset").sidecar;

    assert_eq!(
        sidecar.caption.get().map(String::as_str),
        Some("Snowy morning"),
        "the exporter description must survive the streaming window"
    );
    assert_eq!(
        sidecar.rating.get(),
        Some(&crate::import::enrichment::FAVORITE_RATING)
    );
    assert!(sidecar.tags_user.value().contains("Vacation 2021"));
    assert_eq!(
        sidecar
            .capture_timestamp
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_second(),
        1_609_502_400,
        "these bytes carry no EXIF, so the exporter's taken-time fills the gap"
    );
    let gps = sidecar.gps.as_ref().expect("the exporter's fix");
    assert_eq!((gps.lat, gps.lon), (21.3, -157.8));
    assert_eq!(
        gps.source,
        capsule_core::sidecar::sidecar_v1::GpsSource::Manual,
        "an exporter-sourced fix must not claim to be this file's EXIF"
    );

    // Enrichment happens inside the signed sidecar, so the asset must still verify.
    assert_eq!(
        ws.verify(&id).expect("verify"),
        capsule_core::crypto::verify_asset::VerifyOutcome::Accept
    );
}

#[test]
fn a_streamed_import_the_index_does_not_cover_is_unenriched() {
    let src = TempDir::new().unwrap();
    let lib = TempDir::new().unwrap();
    write_sources(src.path(), 1);
    let mut ws = signed_workspace(lib.path());

    // The `execute_streaming` wrapper — the plain filesystem drive — passes an empty index.
    let plan = plan(
        &scan(&[src.path().to_path_buf()]).unwrap(),
        ws.db(),
        &ImportConfig::default(),
    )
    .unwrap();
    execute_streaming(
        &plan,
        &mut ws,
        StreamingOptions {
            config: &ImportConfig::default(),
            source: &SourceMetadataIndex::empty(),
            uploader: &OkUploader,
            verifier: &MockVerifier {
                durable: true,
                receipt: true,
            },
            headroom_margin: 0,
            cancel: &CancellationToken::new(),
        },
        noop_event,
    )
    .unwrap();

    let id = ws.asset_ids().into_iter().next().expect("one asset");
    let sidecar = &ws.asset(&id).expect("asset").sidecar;
    assert_eq!(sidecar.caption.get(), None);
    assert_eq!(sidecar.rating.get(), None);
    assert!(sidecar.tags_user.value().is_empty());
    assert!(
        sidecar.gps.is_none(),
        "no exporter record and no EXIF means no fix at all"
    );
}
