//! Streaming import — the storage-constrained drive mode over the signed executor (slice
//! `S-B3` in the repo-root `SLICES.md`; SSoT:
//! [Import — Pipeline: Import-Upload Streaming Mode](https://docs/design/import/pipeline/#import-upload-streaming-mode)).
//!
//! The default [`execute`](crate::import::executor::execute) imports every file into the local
//! library *before* upload, so the device temporarily holds the whole import on disk — impossible
//! on a storage-constrained device. Streaming mode removes that requirement by running a bounded
//! **sliding window** of one asset at a time:
//!
//! 1. Import the next file onto the signed path — with source release *deferred*
//!    ([`Workspace::import_asset_streaming`](crate::lifecycle::Workspace::import_asset_streaming)).
//! 2. Upload its bundle via the injected [`AssetUploader`] seam.
//! 3. Confirm durability + custody through the `S-D4` [`ReleaseGate`](crate::library::ReleaseGate)
//!    over the injected [`StorageVerifier`](crate::library::StorageVerifier) seam.
//! 4. **Release** the local original (and delete the Move-mode source) *only* on the `durable`
//!    verdict, so the device never drops the only copy of bytes the server has not confirmed.
//! 5. Advance the window.
//!
//! Peak local disk is therefore bounded to the in-flight asset, not the whole import. The two
//! network touch-points — upload and verify — are **injected trait seams**, so this executor,
//! and its release gating, run with no network at all; `capsule-sdk`/CLI supply the real ones
//! (the SDK's upload client + `POST /storage/verify` coordinator), tests supply deterministic
//! mocks. `capsule-core` stays network-free.

use std::path::PathBuf;

use uuid::Uuid;

use crate::crypto::verify_asset::VerifyOutcome;
use crate::domain::{ImportMode, MemberRole};
use crate::import::executor_cancellation::CancellationToken;
use crate::import::planner::{ImportActionPlan, ImportConfig, ImportDecision};
use crate::import::scan::ImportCandidate;
use crate::library::{
    ReleaseDecision, ReleaseGate, RetainReason, StorageVerifier, available_bytes,
};
use crate::lifecycle::{StackPlacement, StreamedImport, Workspace};

// ── The injected network seam ─────────────────────────────────────────────────

/// The network seam the streaming window drives to hand one just-imported asset's bundle
/// (original + derivatives + metadata) to the [upload protocol](https://docs/design/import/upload-protocol/).
/// Implemented by `capsule-sdk`/CLI over the real upload client; tests supply a deterministic
/// mock. Kept in `capsule-core` as a trait so the streaming loop — and its release gating — is
/// exercised without any network. Synchronous by construction: the offline data plane calls it;
/// async callers resolve their futures before returning.
pub trait AssetUploader {
    /// Upload the imported asset identified by `imported.asset_id`, whose declared blob
    /// content-addresses are `imported.blob_hashes`. Returns `Ok` when the upload session(s)
    /// completed (or the server already held the bytes), or an [`UploadHalt`] when the window
    /// must **pause** rather than admit the next source file.
    fn upload(&self, imported: &StreamedImport) -> Result<(), UploadHalt>;
}

/// Why the streaming window must pause. A halt stops the loop from admitting further source files
/// into the library — continuing would refill the very disk streaming exists to spare. The
/// already-imported in-flight asset is retained locally; its upload resumes via the protocol's
/// `HEAD` resumption on reconnect, and the deterministic planner re-derives the not-yet-admitted
/// work on the next run.
#[derive(Debug, Clone)]
pub enum UploadHalt {
    /// The connection to the server dropped mid-stream.
    Disconnected(String),
    /// Server quota was exhausted at session creation; the next session would be refused.
    QuotaExhausted,
    /// A transport error the driver classifies as pause-worthy.
    Transport(String),
}

// ── Errors ────────────────────────────────────────────────────────────────────

/// A streaming import that cannot start or cannot make progress. Per-asset upload pauses and
/// per-asset retain decisions are *not* errors — they are outcomes in the [`StreamingReport`];
/// these variants are the hard stops.
#[derive(Debug, thiserror::Error)]
pub enum StreamingError {
    /// The minimum-headroom check failed: the largest single asset cannot fully materialize
    /// locally even within the one-asset window. Surfaced *before* any file is imported — the
    /// "cannot import *X* without freeing *Y*" confirmation-time error.
    #[error(
        "cannot stream-import {path}: its {asset_bytes} bytes exceed the {available} available \
         (free {need_to_free} more bytes)"
    )]
    InsufficientHeadroom {
        /// The offending asset's primary source path.
        path: PathBuf,
        /// The largest asset's size in bytes.
        asset_bytes: u64,
        /// Bytes available to an unprivileged process on the library volume.
        available: u64,
        /// Bytes that must be freed for the largest asset (plus headroom) to fit.
        need_to_free: u64,
    },

    /// The free-space probe failed.
    #[error("free-space probe: {0}")]
    Probe(#[from] crate::library::LibraryError),

    /// The run was configured with a [`UploadPolicy::Staged`](crate::import::upload::UploadPolicy)
    /// policy, which is mutually exclusive with streaming import (staged uploads,
    /// slice `S-B4`). Streaming releases local bytes quickly; staged defers exactly
    /// the T2 upload release depends on — so a staged policy can never enter the
    /// streaming window. Surfaced *before* any file is imported, mirroring the
    /// planner's confirmation-time rejection.
    #[error(transparent)]
    StagedPolicyConflict(#[from] crate::import::upload::StagedStreamingConflict),

    /// A signed-path import failed hard enough to abort the run (not a per-file skip).
    #[error("streamed import: {0}")]
    Import(String),

    /// A library-index write (dropping a released owned-original row) failed.
    #[error("index write: {0}")]
    Db(String),
}

// ── Report ────────────────────────────────────────────────────────────────────

/// The per-asset result of one window step, in window order.
#[derive(Debug)]
pub struct StreamedOutcome {
    /// The imported asset's id, when it reached the library (absent for skips / import failures).
    pub asset_id: Option<Uuid>,
    /// The source file this outcome is for.
    pub source_path: PathBuf,
    /// What became of it.
    pub state: StreamedState,
}

/// What became of one file in the streaming window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamedState {
    /// Imported, uploaded, verified `durable` + custody receipt, local original (and any
    /// Move-mode source) released — the asset is now a server-only, re-fetchable representation.
    Released,
    /// Imported + uploaded, but the release gate **retained** the local copy (verdict not
    /// `durable`, receipt missing/unverified, or a transport failure). The bytes stay on disk;
    /// a later attempt re-drives the gate.
    Retained(RetainReason),
    /// The upload paused the window on this asset. Its bytes were imported and are retained
    /// locally; its upload resumes via `HEAD` on reconnect.
    UploadHalted,
    /// A planner skip decision — never imported.
    Skipped,
    /// The signed-path import of this member failed.
    ImportFailed(String),
}

/// Why the window halted before draining the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamHalt {
    /// The server connection dropped mid-stream.
    Disconnected(String),
    /// Server quota was exhausted.
    QuotaExhausted,
    /// A pause-worthy transport error.
    Transport(String),
    /// The caller's cancel token fired at a window boundary.
    Cancelled,
}

impl From<UploadHalt> for StreamHalt {
    fn from(h: UploadHalt) -> Self {
        match h {
            UploadHalt::Disconnected(m) => StreamHalt::Disconnected(m),
            UploadHalt::QuotaExhausted => StreamHalt::QuotaExhausted,
            UploadHalt::Transport(m) => StreamHalt::Transport(m),
        }
    }
}

/// The outcome of a streaming import run.
#[derive(Debug, Default)]
pub struct StreamingReport {
    /// Per-file outcomes in window order.
    pub outcomes: Vec<StreamedOutcome>,
    /// Set when the window halted before draining the plan; `None` on a clean drain.
    pub halted: Option<StreamHalt>,
}

impl StreamingReport {
    /// How many local originals were released after a durable verdict.
    pub fn released_count(&self) -> usize {
        self.count(|s| *s == StreamedState::Released)
    }

    /// How many imports were kept local (gate retained + the in-flight halted asset).
    pub fn retained_count(&self) -> usize {
        self.count(|s| matches!(s, StreamedState::Retained(_) | StreamedState::UploadHalted))
    }

    /// Whether the window halted before completing the plan.
    pub fn is_halted(&self) -> bool {
        self.halted.is_some()
    }

    fn count(&self, pred: impl Fn(&StreamedState) -> bool) -> usize {
        self.outcomes.iter().filter(|o| pred(&o.state)).count()
    }
}

/// Progress events for traceability / UI. The window is per-asset, so every state transition is
/// observable.
#[derive(Debug, Clone)]
pub enum StreamingEvent {
    /// The pre-flight probe result, emitted once before the first import.
    Preflight {
        /// The plan's total import size.
        total_size: u64,
        /// Bytes available on the library volume.
        available: u64,
        /// The largest single asset's size (the headroom-bound).
        largest_asset: u64,
    },
    /// An asset landed in the library (import complete, upload not yet started).
    Imported { asset_id: Uuid, source: PathBuf },
    /// An asset's local original was released after a durable verdict.
    Released { asset_id: Uuid },
    /// An asset's local copy was retained (gate refused release).
    Retained {
        asset_id: Uuid,
        reason: RetainReason,
    },
    /// The window halted; no further source files are admitted.
    Halted { reason: StreamHalt },
}

// ── The window executor ────────────────────────────────────────────────────────

/// Drive a streaming import: the bounded per-asset import → upload → verify → release window over
/// `plan`. `headroom_margin` is the free-space cushion the minimum-headroom check and the
/// implicit recommendation apply. `uploader` and `verifier` are the injected network seams; the
/// executor itself performs no network I/O. Honors `cancel` at every window boundary.
///
/// Returns a [`StreamingReport`]; a hard failure to *start* (insufficient headroom, a failed
/// probe) is a [`StreamingError`]. Per-asset upload pauses and gate retentions are outcomes, not
/// errors: on a pause the window halts (`report.halted` set) with no further files admitted, and
/// the run can be re-invoked after reconnect — the deterministic planner re-derives the remaining
/// work and skips already-completed assets.
#[tracing::instrument(
    skip_all,
    fields(candidates = plan.actions.len(), mode = ?config.import_mode, headroom = headroom_margin)
)]
#[allow(clippy::too_many_arguments)]
pub fn execute_streaming<U, V>(
    plan: &ImportActionPlan,
    workspace: &mut Workspace,
    config: &ImportConfig,
    uploader: &U,
    verifier: &V,
    headroom_margin: u64,
    on_event: impl Fn(StreamingEvent),
    cancel: &CancellationToken,
) -> Result<StreamingReport, StreamingError>
where
    U: AssetUploader,
    V: StorageVerifier,
{
    // ── Exclusion: a staged upload policy can never enter the streaming window ───
    // Streaming exists to release local bytes as fast as possible; staged uploads
    // defer exactly the T2 (original) upload that release depends on. The planner
    // rejects the combination at confirmation; this is the by-construction backstop
    // so a staged policy cannot reach `execute_streaming` even if a caller skips
    // confirmation. Surfaces before any file is imported (staged uploads, S-B4).
    crate::import::upload::ensure_streaming_compatible(config.upload_policy, true)?;

    let album_id = match &config.target_album_id {
        Some(s) => Uuid::parse_str(s)
            .map_err(|e| StreamingError::Import(format!("invalid target album id {s:?}: {e}")))?,
        None => workspace.default_album_id(),
    };

    // ── Pre-flight: minimum-headroom hard error ─────────────────────────────────
    // Streaming bounds peak disk to the window, but the largest single asset must still fully
    // materialize locally before its upload and release. Probe once, up front, so the failure
    // surfaces at confirmation rather than mid-stream.
    let available = available_bytes(workspace.root())?;
    let largest = plan.largest_import_size();
    on_event(StreamingEvent::Preflight {
        total_size: plan.counts.total_size,
        available,
        largest_asset: largest,
    });
    if !crate::library::largest_asset_fits(largest, available, headroom_margin) {
        let (path, asset_bytes) = largest_candidate(plan);
        let need_to_free = asset_bytes
            .saturating_add(headroom_margin)
            .saturating_sub(available);
        tracing::warn!(
            %need_to_free, asset_bytes, available,
            "streaming import blocked: largest asset does not fit"
        );
        return Err(StreamingError::InsufficientHeadroom {
            path,
            asset_bytes,
            available,
            need_to_free,
        });
    }

    // ── The window ──────────────────────────────────────────────────────────────
    let mut report = StreamingReport::default();
    for (candidate, decision) in &plan.actions {
        if cancel.is_cancelled() {
            report.halted = Some(StreamHalt::Cancelled);
            on_event(StreamingEvent::Halted {
                reason: StreamHalt::Cancelled,
            });
            break;
        }
        match decision {
            ImportDecision::Import => {
                match stream_candidate(
                    workspace, config, album_id, candidate, uploader, verifier, &on_event,
                )? {
                    CandidateFlow::Done(outs) => report.outcomes.extend(outs),
                    CandidateFlow::Halt(reason, outs) => {
                        report.outcomes.extend(outs);
                        on_event(StreamingEvent::Halted {
                            reason: reason.clone(),
                        });
                        report.halted = Some(reason);
                        break;
                    }
                }
            }
            ImportDecision::SkipDuplicate { .. }
            | ImportDecision::SkipUnsupported
            | ImportDecision::SkipError(_) => {
                report.outcomes.push(StreamedOutcome {
                    asset_id: None,
                    source_path: candidate.primary_path().clone(),
                    state: StreamedState::Skipped,
                });
            }
        }
    }

    tracing::info!(
        released = report.released_count(),
        retained = report.retained_count(),
        halted = report.is_halted(),
        "streaming import: run complete"
    );
    Ok(report)
}

// ── Per-candidate window step ──────────────────────────────────────────────────

/// The control flow of one candidate's window step.
enum CandidateFlow {
    /// Every member handled; the outcomes to record.
    Done(Vec<StreamedOutcome>),
    /// The window must halt: the reason + the outcomes gathered so far (including the in-flight
    /// retained asset that triggered the pause).
    Halt(StreamHalt, Vec<StreamedOutcome>),
}

/// Import → upload → verify → release each member of `candidate` in turn. A multi-file candidate
/// (RAW+JPEG, Live Photo) mints one stack id and hides its non-primary members, exactly as the
/// bulk executor does; the stack row is persisted once its members exist in the index — released
/// originals keep their queryable asset rows, so grouping survives release.
#[allow(clippy::too_many_arguments)]
fn stream_candidate<U, V>(
    workspace: &mut Workspace,
    config: &ImportConfig,
    album_id: Uuid,
    candidate: &ImportCandidate,
    uploader: &U,
    verifier: &V,
    on_event: &impl Fn(StreamingEvent),
) -> Result<CandidateFlow, StreamingError>
where
    U: AssetUploader,
    V: StorageVerifier,
{
    let move_source = matches!(config.import_mode, ImportMode::Move);
    let stack_id = candidate
        .stack_type
        .map(|_| format!("stack-{}", Uuid::now_v7().simple()));

    let mut outcomes = Vec::new();
    let mut imported_members: Vec<(String, MemberRole)> = Vec::new();

    for (seq, (path, role)) in candidate.members.iter().enumerate() {
        let is_primary = *role == MemberRole::Primary || seq == 0;
        let stack = stack_id.as_ref().map(|sid| StackPlacement {
            stack_id: sid.clone(),
            hidden: !is_primary,
        });

        // 1. Import onto the signed path with deferred release.
        let imported = match workspace.import_asset_streaming(album_id, path, move_source, stack) {
            Ok(i) => i,
            Err(e) => {
                outcomes.push(StreamedOutcome {
                    asset_id: None,
                    source_path: path.clone(),
                    state: StreamedState::ImportFailed(e.to_string()),
                });
                continue;
            }
        };
        on_event(StreamingEvent::Imported {
            asset_id: imported.asset_id,
            source: path.clone(),
        });
        imported_members.push((imported.asset_id.to_string(), *role));

        // 2. Upload the bundle. A halt pauses the window: the imported bytes stay local and no
        //    further source files are admitted.
        if let Err(halt) = uploader.upload(&imported) {
            tracing::warn!(asset = %imported.asset_id, ?halt, "streaming upload paused the window");
            outcomes.push(StreamedOutcome {
                asset_id: Some(imported.asset_id),
                source_path: path.clone(),
                state: StreamedState::UploadHalted,
            });
            return Ok(CandidateFlow::Halt(halt.into(), outcomes));
        }

        // 3–4. Gate on durability + custody, then release the local original (and Move source)
        //       only on a Release decision — the S-D4 verify-before-destroy rule per asset.
        let state = release_after_verify(workspace, verifier, &imported, on_event)?;
        outcomes.push(StreamedOutcome {
            asset_id: Some(imported.asset_id),
            source_path: path.clone(),
            state,
        });
    }

    // Persist the stack grouping once its members exist in the index.
    if let Some(sid) = &stack_id
        && !imported_members.is_empty()
    {
        crate::import::executor::persist_stack(workspace, candidate, sid, &imported_members)
            .map_err(|e| StreamingError::Db(e.to_string()))?;
    }

    Ok(CandidateFlow::Done(outcomes))
}

/// Run the release gate for a just-uploaded asset and act on it: on `Release`, delete the local
/// original + drop its owned-original representation row (it becomes a server-only asset) and
/// delete the Move-mode source; on `Retain`, leave every local byte in place.
fn release_after_verify<V: StorageVerifier>(
    workspace: &mut Workspace,
    verifier: &V,
    imported: &StreamedImport,
    on_event: &impl Fn(StreamingEvent),
) -> Result<StreamedState, StreamingError> {
    // The crypto half of the gate: the asset self-verified on import, but re-check freshly.
    let verify_asset_accepted = workspace
        .verify(&imported.asset_id)
        .is_ok_and(|o| o == VerifyOutcome::Accept);

    let decision = ReleaseGate::new(verifier).may_release(
        imported.asset_id,
        &imported.blob_hashes,
        verify_asset_accepted,
    );

    match decision {
        ReleaseDecision::Release => {
            let _ = std::fs::remove_file(&imported.local_original);
            // The owned-original representation row is keyed by the hyphenated UUID, matching
            // `Workspace::index_original_representation` (the writer).
            workspace
                .db()
                .remove_representation(&imported.asset_id.to_string(), "original")
                .map_err(|e| StreamingError::Db(e.to_string()))?;
            if let Some(src) = &imported.move_source {
                let _ = std::fs::remove_file(src);
            }
            on_event(StreamingEvent::Released {
                asset_id: imported.asset_id,
            });
            Ok(StreamedState::Released)
        }
        ReleaseDecision::Retain(reason) => {
            on_event(StreamingEvent::Retained {
                asset_id: imported.asset_id,
                reason: reason.clone(),
            });
            Ok(StreamedState::Retained(reason))
        }
    }
}

/// The largest `Import` candidate's `(primary_path, size)` — the offending asset for the
/// insufficient-headroom error.
fn largest_candidate(plan: &ImportActionPlan) -> (PathBuf, u64) {
    plan.actions
        .iter()
        .filter(|(_, d)| matches!(d, ImportDecision::Import))
        .map(|(c, _)| {
            (
                c.primary_path().clone(),
                crate::import::planner::candidate_size(c),
            )
        })
        .max_by_key(|(_, b)| *b)
        .unwrap_or_default()
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    use crate::crypto::hash::Hash32;
    use crate::crypto::primitives::Argon2Params;
    use crate::domain::ImportMode;
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
        ws.create_album_with_id(default, "Imports");
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

    // ── Injected verify seam mock (the S-D4 StorageVerifier) ────────────────────

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
                &ImportConfig::default(),
                &OkUploader,
                &verifier,
                0,
                noop_event,
                &CancellationToken::new(),
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
                &ImportConfig::default(),
                &OkUploader,
                &verifier,
                0,
                noop_event,
                &CancellationToken::new(),
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
    /// retained otherwise (the S-D4 verify-before-destroy rule per asset).
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
            &config,
            &OkUploader,
            &MockVerifier {
                durable: true,
                receipt: true,
            },
            0,
            noop_event,
            &CancellationToken::new(),
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
            &config,
            &OkUploader,
            &MockVerifier {
                durable: false,
                receipt: true,
            },
            0,
            noop_event,
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(
            sources2[0].exists(),
            "Move source retained when not durable"
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
            &config,
            &uploader,
            &MockVerifier {
                durable: true,
                receipt: true,
            },
            0,
            noop_event,
            &CancellationToken::new(),
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
            &config,
            &OkUploader,
            &MockVerifier {
                durable: true,
                receipt: true,
            },
            0,
            noop_event,
            &CancellationToken::new(),
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
            &config,
            &OkUploader,
            &MockVerifier {
                durable: true,
                receipt: true,
            },
            u64::MAX,
            noop_event,
            &CancellationToken::new(),
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
    /// [`UploadPolicy::Staged`](crate::import::upload::UploadPolicy) policy can never
    /// enter the streaming window: `execute_streaming` refuses it *before* any file
    /// is imported, mirroring the planner's confirmation-time rejection. (SSoT:
    /// download-sync doc — staged uploads are mutually exclusive with streaming.)
    #[test]
    fn staged_policy_cannot_enter_the_streaming_window() {
        use crate::import::upload::{StagedStreamingConflict, UploadPolicy};

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
            &config,
            &OkUploader,
            &MockVerifier {
                durable: true,
                receipt: true,
            },
            0,
            noop_event,
            &CancellationToken::new(),
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
}
