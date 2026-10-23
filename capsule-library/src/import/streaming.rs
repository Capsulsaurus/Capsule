//! Streaming import — the storage-constrained drive mode over the signed executor (SSoT:
//! [Import — Pipeline: Import-Upload Streaming Mode](https://docs/design/import/pipeline/#import-upload-streaming-mode)).
//!
//! The default [`execute`](crate::import::execute) imports every file into the local
//! library *before* upload, so the device temporarily holds the whole import on disk — impossible
//! on a storage-constrained device. Streaming mode removes that requirement by running a bounded
//! **sliding window** of one asset at a time:
//!
//! 1. Import the next file onto the signed path — with source release *deferred*
//!    ([`Workspace::import_asset_streaming`](crate::lifecycle::Workspace::import_asset_streaming)).
//! 2. Upload its bundle via the injected [`AssetUploader`] seam.
//! 3. Confirm durability + custody through the [`ReleaseGate`] over the injected
//!    [`StorageVerifier`] seam.
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

use capsule_core::crypto::verify_asset::VerifyOutcome;
use capsule_core::domain::{ImportMode, MemberRole};
use capsule_core::import::scan::ImportCandidate;
use uuid::Uuid;

use crate::import::enrichment::SourceMetadataIndex;
use crate::import::executor_cancellation::CancellationToken;
use crate::import::planner::{ImportActionPlan, ImportConfig, ImportDecision};
use crate::library::{
    ReleaseDecision, ReleaseGate, RetainReason, StorageVerifier, available_bytes,
};
use crate::lifecycle::{StreamedImport, Workspace};

// ── The injected network seam ─────────────────────────────────────────────────

/// The network seam the streaming window drives to hand one just-imported asset's bundle
/// (original + derivatives + metadata) to the [upload protocol](https://docs/design/transfer/upload/).
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

    /// The run was configured with a [`UploadPolicy::Staged`](crate::import::UploadPolicy)
    /// policy, which is mutually exclusive with streaming import. Streaming releases local
    /// bytes quickly; staged defers exactly the T2 upload release depends on — so a staged
    /// policy can never enter the streaming window. Surfaced *before* any file is imported, mirroring the
    /// planner's confirmation-time rejection.
    #[error(transparent)]
    StagedPolicyConflict(#[from] capsule_core::import::upload::StagedStreamingConflict),

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

/// Everything the streaming window needs beyond the plan, the workspace, and the event sink.
///
/// One struct rather than six positionals: the two entry points this replaced differed only in
/// `source` and carried eight and nine arguments respectively, both behind a
/// `#[allow(clippy::too_many_arguments)]`. A caller that wants no source-adapter enrichment
/// passes `&SourceMetadataIndex::empty()`, which is exactly what the thinner of the two
/// entry points did for it.
pub struct StreamingOptions<'a, U: AssetUploader, V: StorageVerifier> {
    /// The planner configuration the run was confirmed against.
    pub config: &'a ImportConfig,
    /// Folded metadata a third-party [source adapter](crate::import::SourceAdapter) extracted,
    /// attached to each file it covers. [`SourceMetadataIndex::empty`] for a plain filesystem
    /// drive; files the index does not cover import identically either way.
    pub source: &'a SourceMetadataIndex,
    /// The injected upload seam. `capsule-core` performs no network I/O of its own.
    pub uploader: &'a U,
    /// The injected durability/custody seam behind the release gate.
    pub verifier: &'a V,
    /// The free-space cushion the minimum-headroom check and the implicit recommendation apply.
    pub headroom_margin: u64,
    /// Honored at every window boundary.
    pub cancel: &'a CancellationToken,
}

/// Drive a streaming import: the bounded per-asset import → upload → verify → release window over
/// `plan`, under `opts`.
///
/// The enrichment in [`StreamingOptions::source`] is written *inside the signed sidecar*, at the
/// same single write path the bulk executor drives, so the drive mode a user picks never changes
/// what a migrated library keeps: a storage-constrained device streaming a Takeout export lands
/// the exporter's capture time, GPS, caption, favorite, and album membership exactly as an
/// unconstrained one does. This is the streaming twin of
/// [`execute_with_source_metadata`](crate::import::execute_with_source_metadata).
///
/// Returns a [`StreamingReport`]; a hard failure to *start* (insufficient headroom, a failed
/// probe) is a [`StreamingError`]. Per-asset upload pauses and gate retentions are outcomes, not
/// errors: on a pause the window halts (`report.halted` set) with no further files admitted, and
/// the run can be re-invoked after reconnect — the deterministic planner re-derives the remaining
/// work and skips already-completed assets.
#[tracing::instrument(
    skip_all,
    fields(
        candidates = plan.actions.len(),
        mode = ?opts.config.import_mode,
        headroom = opts.headroom_margin,
        source_metadata = opts.source.len(),
    )
)]
pub fn execute_streaming<U, V>(
    plan: &ImportActionPlan,
    workspace: &mut Workspace,
    opts: StreamingOptions<'_, U, V>,
    on_event: impl Fn(StreamingEvent),
) -> Result<StreamingReport, StreamingError>
where
    U: AssetUploader,
    V: StorageVerifier,
{
    // Every field is a shared reference or a `u64`, so this copies out of `opts` rather than
    // moving it; the struct is handed to `stream_candidate` whole below.
    let StreamingOptions {
        config,
        headroom_margin,
        cancel,
        ..
    } = opts;
    // ── Exclusion: a staged upload policy can never enter the streaming window ───
    // Streaming exists to release local bytes as fast as possible; staged uploads
    // defer exactly the T2 (original) upload that release depends on. The planner
    // rejects the combination at confirmation; this is the by-construction backstop
    // so a staged policy cannot reach `execute_streaming` even if a caller skips
    // confirmation. Surfaces before any file is imported (staged uploads).
    crate::import::ensure_streaming_compatible(config.upload_policy, true)?;

    // Same single resolution the executor runs: bind the library's derived de facto album
    // (rule 3), then apply the order (SSoT: organization § The Default Album).
    let resolved = capsule_core::import::default_album::resolve_default_album(
        &config.album.with_derived(workspace.default_album_id()),
    )
    .map_err(|e| StreamingError::Import(format!("cannot resolve a destination album: {e}")))?;
    tracing::info!(
        album_id = %resolved.album_id,
        rule = resolved.rule.as_str(),
        "streaming import destination resolved"
    );
    let album_id = resolved.album_id;

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
                match stream_candidate(workspace, album_id, candidate, &opts, &on_event)? {
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
            ImportDecision::SkipDuplicate { .. } | ImportDecision::SkipError(_) => {
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
fn stream_candidate<U, V>(
    workspace: &mut Workspace,
    album_id: Uuid,
    candidate: &ImportCandidate,
    opts: &StreamingOptions<'_, U, V>,
    on_event: &impl Fn(StreamingEvent),
) -> Result<CandidateFlow, StreamingError>
where
    U: AssetUploader,
    V: StorageVerifier,
{
    let StreamingOptions {
        config,
        source,
        uploader,
        verifier,
        ..
    } = *opts;
    let move_source = matches!(config.import_mode, ImportMode::Move);
    let stack = candidate.stack_type.map(|st| (Uuid::now_v7(), st));

    let mut outcomes = Vec::new();
    let mut imported_members: Vec<(String, MemberRole)> = Vec::new();

    for (seq, (path, role)) in candidate.members.iter().enumerate() {
        let is_primary = *role == MemberRole::Primary || seq == 0;
        let membership = crate::import::executor::stack_membership(stack, *role, is_primary, seq);

        // 1. Import onto the signed path with deferred release, carrying whatever the source
        //    adapter folded for this member. Resolved through the *same* helper the
        //    bulk executor uses, so the two drive modes cannot drift apart.
        let enrichment = crate::import::executor::member_enrichment(source, path);
        let imported = match workspace.import_asset_streaming(
            album_id,
            path,
            move_source,
            membership,
            enrichment,
        ) {
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
        //       only on a Release decision — the verify-before-destroy rule per asset.
        let state = release_after_verify(workspace, verifier, &imported, on_event)?;
        outcomes.push(StreamedOutcome {
            asset_id: Some(imported.asset_id),
            source_path: path.clone(),
            state,
        });
    }

    // Persist the stack grouping once its members exist in the index.
    if let Some((stack_id, _)) = stack
        && !imported_members.is_empty()
    {
        crate::import::executor::persist_stack(
            workspace,
            candidate,
            &stack_id.to_string(),
            &imported_members,
        )
        .map_err(|e| StreamingError::Db(e.to_string()))?;
    }

    Ok(CandidateFlow::Done(outcomes))
}

/// Run the release gate for a just-uploaded asset and act on it: on `Release`, delete the local
/// original + drop its owned-original representation row (it becomes a server-only asset); on
/// `Retain`, leave every local byte in place. A Move-mode source is released through
/// [`Workspace::release_move_source`], which reads it and the local original back against the
/// signed hash before the gate, so its decision is the one acted on here.
fn release_after_verify<V: StorageVerifier>(
    workspace: &mut Workspace,
    verifier: &V,
    imported: &StreamedImport,
    on_event: &impl Fn(StreamingEvent),
) -> Result<StreamedState, StreamingError> {
    // A Move source goes through the one Move-release path, before the local original is
    // touched: both copies are read back against the signed hash, then the gate decides, and
    // the source is deleted only on `Release`. A source that fails the read-back keeps the
    // local original too — it is then the one copy known to be intact.
    let decision = if let Some(src) = &imported.move_source {
        workspace
            .release_move_source(verifier, imported.asset_id, src)
            .map_err(|e| StreamingError::Import(e.to_string()))?
    } else {
        // The crypto half of the gate: the asset self-verified on import, but re-check freshly.
        let verify_asset_accepted = workspace
            .verify(&imported.asset_id)
            .is_ok_and(|o| o == VerifyOutcome::Accept);
        ReleaseGate::new(verifier).may_release(
            imported.asset_id,
            &imported.blob_hashes,
            verify_asset_accepted,
        )
    };

    match decision {
        ReleaseDecision::Release => {
            let _ = std::fs::remove_file(&imported.local_original);
            // The owned-original representation row is keyed by the hyphenated UUID, matching
            // `Workspace::index_original_representation` (the writer).
            workspace
                .db()
                .remove_representation(&imported.asset_id.to_string(), "original")
                .map_err(|e| StreamingError::Db(e.to_string()))?;
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
mod tests;
