//! Phase 4 — execute an import plan onto the **signed lifecycle path** (slice `S-B2`).
//!
//! Each `ImportDecision::Import` candidate is imported through
//! [`Workspace::import_asset_with`](crate::lifecycle::Workspace::import_asset_with): every member
//! becomes a signed [`SidecarV1`](crate::sidecar::SidecarV1) + signed manifest +
//! append-only provenance, self-verified through
//! [`verify_asset`](crate::crypto::verify_asset::verify_asset), and — when a still encoder is
//! attached to the workspace — with signed thumbnail/preview derivatives + an LQIP in the
//! sidecar. No still encoder exists in this build: the media stack is retired to
//! `legacy-review/` and restoring it is `S-B1`.
//!
//! This retired the legacy unsigned `AssetSidecar` write path from the executor; the production
//! write path itself is now gone (`S-G4`) — no code writes unsigned sidecars anymore. Only the
//! *read* path survives, for the recovery-first index rebuild
//! ([`rebuild_index`](crate::library::rebuild_index)) that still ingests unsigned `.cbor`
//! sidecars left by pre-signed-path libraries. The pure planner (`import::planner`) is unchanged:
//! it still decides *what* to import; the executor decides *how* to commit it.

use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::db::rows::{AssetStackRow, StackMemberRow};
use crate::domain::{ImportMode, MemberRole, StackType};
use crate::import::default_album::resolve_default_album;
use crate::import::enrichment::{SourceMetadataIndex, sidecar_enrichment};
use crate::import::executor_cancellation::CancellationToken;
use crate::import::planner::{ImportActionPlan, ImportConfig, ImportDecision};
use crate::import::progress::{ImportExecutionSummary, ImportOutcome, ImportProgressEvent};
use crate::import::scan::ImportCandidate;
use crate::lifecycle::{LifecycleError, SidecarEnrichment, SignedImportOptions, Workspace};
use crate::sidecar::sidecar_v1::{StackMembership, StackRole};

type ExecError = Box<dyn std::error::Error + Send + Sync>;

/// Phase 4 — execute the import plan against `workspace`.
///
/// Every `ImportDecision::Import` candidate is committed through the signed lifecycle path; skip
/// decisions are reported verbatim. Assets are written into the album resolved from
/// `config.album` through [`resolve_default_album`] — the explicit pick, else the owner's
/// `default_album_id` pointer, else the workspace's derived de facto album — which must already
/// exist in the workspace.
///
/// This is a plain filesystem import: no exporter metadata is attached. A third-party import
/// calls [`execute_with_source_metadata`] instead.
pub fn execute(
    plan: &ImportActionPlan,
    workspace: &mut Workspace,
    config: &ImportConfig,
    on_event: impl Fn(ImportProgressEvent),
    cancel: &CancellationToken,
) -> Result<ImportExecutionSummary, ExecError> {
    execute_with_source_metadata(
        plan,
        workspace,
        config,
        &SourceMetadataIndex::empty(),
        on_event,
        cancel,
    )
}

/// [`execute`], with the folded metadata a third-party [source adapter] extracted attached to
/// each file it covers (slice `S-B10`).
///
/// The adapter resolved the [precedence rule] at extraction; the executor is what makes the
/// result *durable* — each member's record is mapped onto sidecar fields by
/// [`sidecar_enrichment`] and written inside the signed sidecar at import, rather than being
/// discarded once the plan was built. A file the index does not cover imports exactly as it
/// does through [`execute`].
///
/// [source adapter]: crate::import::SourceAdapter
/// [precedence rule]: https://docs/design/import/pipeline/#third-party-importers
#[tracing::instrument(
    skip_all,
    fields(
        candidates = plan.actions.len(),
        mode = ?config.import_mode,
        source_metadata = source.len(),
    )
)]
pub fn execute_with_source_metadata(
    plan: &ImportActionPlan,
    workspace: &mut Workspace,
    config: &ImportConfig,
    source: &SourceMetadataIndex,
    on_event: impl Fn(ImportProgressEvent),
    cancel: &CancellationToken,
) -> Result<ImportExecutionSummary, ExecError> {
    // The library is the only authority on the derived de facto album (rule 3), so bind it
    // here and run the *same* resolution the planner ran — one policy, never two. The plan's
    // recorded `album` is the explainability trail; this is the authoritative destination.
    let resolved = resolve_default_album(&config.album.with_derived(workspace.default_album_id()))
        .map_err(|e| format!("cannot resolve a destination album: {e}"))?;
    tracing::info!(
        album_id = %resolved.album_id,
        rule = resolved.rule.as_str(),
        "import destination resolved"
    );
    let album_id = resolved.album_id;

    let total = plan.actions.len() as u64;
    let total_files: u64 = plan
        .actions
        .iter()
        .filter(|(_, d)| matches!(d, ImportDecision::Import))
        .map(|(c, _)| c.source_paths.len() as u64)
        .sum();

    on_event(ImportProgressEvent::ImportStarted {
        total_candidates: total,
        total_files,
    });

    let mut summary = ImportExecutionSummary::default();
    let mut enriched_candidates = 0u64;

    for (i, (candidate, decision)) in plan.actions.iter().enumerate() {
        if cancel.is_cancelled() {
            break;
        }

        let primary_path = candidate.primary_path().clone();
        on_event(ImportProgressEvent::CandidateStarted {
            index: i as u64,
            total,
            primary_path: primary_path.clone(),
        });

        let outcomes = match decision {
            ImportDecision::Import => {
                enriched_candidates +=
                    u64::from(source.get(candidate.primary_path().as_path()).is_some());
                execute_candidate(candidate, workspace, config, album_id, source)?
            }
            ImportDecision::SkipDuplicate { existing_uuid } => {
                vec![(
                    primary_path,
                    ImportOutcome::DuplicateSkipped {
                        existing_uuid: existing_uuid.clone(),
                    },
                )]
            }
            ImportDecision::SkipUnsupported => {
                vec![(primary_path, ImportOutcome::Unsupported)]
            }
            ImportDecision::SkipError(msg) => {
                vec![(primary_path, ImportOutcome::CorruptUnreadable(msg.clone()))]
            }
        };

        on_event(ImportProgressEvent::CandidateCompleted {
            index: i as u64,
            outcomes: outcomes.clone(),
        });
        summary.outcomes.extend(outcomes);
    }

    on_event(ImportProgressEvent::ImportCompleted {
        summary: ImportExecutionSummary {
            outcomes: summary.outcomes.clone(),
        },
    });

    tracing::info!(
        imported = summary.imported_count(),
        duplicates = summary.duplicate_count(),
        errors = summary.error_count(),
        // Imported, but with no thumbnail/preview: the first is the expected S-B13 codec gap,
        // the second a genuine decode failure of a format we do support.
        derivatives_deferred = summary.deferred_derivative_count(),
        derivatives_decode_failed = summary.decode_failed_count(),
        // How much of this run carried third-party exporter metadata into the signed sidecars
        // (`S-B10`); zero for a plain filesystem import.
        enriched_candidates,
        "import: execution complete"
    );
    Ok(summary)
}

// ── Per-member enrichment ────────────────────────────────────────────────────

/// The signed-sidecar enrichment for one member path, with the fold decision logged.
///
/// The single place a [`SourceMetadataIndex`] is turned into a
/// [`SidecarEnrichment`](crate::lifecycle::SidecarEnrichment), shared by this executor and the
/// [streaming window](crate::import::streaming) so a *streamed* third-party import writes exactly
/// what a bulk one does (`S-B11` closed the gap `S-B10` left open). A path the index does not
/// cover, or a record that folds to nothing, yields [`None`] — the untouched write path.
pub(crate) fn member_enrichment(
    source: &SourceMetadataIndex,
    path: &Path,
) -> Option<SidecarEnrichment> {
    source.get(path).and_then(|folded| {
        // The fold decision itself, per member, so a surprising capture time or location in
        // a sidecar can be traced back to the side it came from. User content (the
        // description text, the album titles) stays out: sizes and counts are enough.
        tracing::debug!(
            path = %path.display(),
            taken_time = ?folded.taken_time,
            taken_time_source = ?folded.taken_time_source,
            gps_source = ?folded.gps_source,
            description_bytes = folded.description.as_ref().map_or(0, String::len),
            favorite = folded.favorite,
            albums = folded.albums.len(),
            "import: exporter metadata folded for member"
        );
        sidecar_enrichment(folded)
    })
}

// ── Per-candidate execution ──────────────────────────────────────────────────

/// Import every member of `candidate` through the signed path, then persist its stack grouping
/// (if any). Move-mode source release and stack placement are handled inside the signed import.
fn execute_candidate(
    candidate: &ImportCandidate,
    workspace: &mut Workspace,
    config: &ImportConfig,
    album_id: Uuid,
    source: &SourceMetadataIndex,
) -> Result<Vec<(PathBuf, ImportOutcome)>, ExecError> {
    let move_source = matches!(config.import_mode, ImportMode::Move);
    // One stack id per multi-file candidate; the primary member stays visible, the rest hidden.
    let stack = candidate.stack_type.map(|st| (Uuid::now_v7(), st));

    let mut outcomes = Vec::new();
    let mut imported: Vec<(String, MemberRole)> = Vec::new();

    for (seq, (path, role)) in candidate.members.iter().enumerate() {
        let is_primary = *role == MemberRole::Primary || seq == 0;
        let membership = stack_membership(stack, *role, is_primary, seq);
        let enrichment = member_enrichment(source, path);
        // Offline import: release the Move source on the local durable commit. The online /
        // streaming path (S-B3) sets `defer_source_release` and gates on the server verdict.
        let opts = SignedImportOptions {
            move_source,
            defer_source_release: false,
            stack: membership,
            enrichment,
        };

        match workspace.import_asset_with(album_id, path, &opts) {
            Ok(receipt) => {
                imported.push((receipt.asset_id.to_string(), *role));
                outcomes.push((
                    path.clone(),
                    ImportOutcome::Imported {
                        derivatives: receipt.derivatives,
                    },
                ));
            }
            Err(e) => outcomes.push((path.clone(), import_error_outcome(&e))),
        }
    }

    if let Some((stack_id, _)) = stack
        && !imported.is_empty()
    {
        persist_stack(workspace, candidate, &stack_id.to_string(), &imported)?;
    }

    Ok(outcomes)
}

/// Persist the `asset_stacks` row + its members once the member assets exist in the index.
/// Shared with the [streaming executor](crate::import::streaming), which groups multi-file
/// candidates the same way.
pub(crate) fn persist_stack(
    workspace: &Workspace,
    candidate: &ImportCandidate,
    stack_id: &str,
    imported: &[(String, MemberRole)],
) -> Result<(), ExecError> {
    let now = now_secs();
    let primary_uuid = imported
        .iter()
        .find(|(_, r)| *r == MemberRole::Primary)
        .or_else(|| imported.first())
        .map(|(u, _)| u.clone())
        .unwrap_or_default();

    let stack_row = AssetStackRow {
        id: stack_id.to_string(),
        stack_type: candidate.stack_type.map_or_else(
            || "custom".to_string(),
            |st| format!("{st:?}").to_lowercase(),
        ),
        primary_asset_id: primary_uuid.clone(),
        cover_asset_id: Some(primary_uuid),
        is_collapsed: true,
        is_auto_generated: true,
        created_at: now,
        modified_at: now,
    };
    workspace.db().insert_stack(&stack_row)?;

    for (seq, (uuid, role)) in imported.iter().enumerate() {
        let member_row = StackMemberRow {
            id: format!("{stack_id}#{seq}"),
            stack_id: stack_id.to_string(),
            asset_id: uuid.clone(),
            sequence_order: seq as i64,
            member_role: role_str(*role).to_string(),
            created_at: now,
        };
        workspace.db().insert_stack_member(&member_row)?;
    }
    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Map a signed-import failure to a per-file outcome. Permission errors surface distinctly; the
/// rest — including the "should never happen" self-verify failures — report as unreadable.
fn import_error_outcome(e: &LifecycleError) -> ImportOutcome {
    let msg = e.to_string();
    if msg.contains("permission") {
        ImportOutcome::PermissionDenied(msg)
    } else {
        ImportOutcome::CorruptUnreadable(msg)
    }
}

/// The signed [`StackMembership`] one member of a multi-file candidate carries (`S-B15`), or
/// `None` for a standalone candidate.
///
/// `is_primary` is the executor's own rule (the declared primary, or the first member when none
/// is declared), and it is what the index projection reads as "not stack-hidden" — so it wins
/// over the declared [`MemberRole`], which is why the role mapping is applied only to the rest.
pub(crate) fn stack_membership(
    stack: Option<(Uuid, StackType)>,
    role: MemberRole,
    is_primary: bool,
    seq: usize,
) -> Option<StackMembership> {
    let (stack_id, stack_type) = stack?;
    Some(StackMembership {
        stack_id,
        stack_type,
        role: if is_primary {
            StackRole::Primary
        } else {
            stack_role(role)
        },
        member_index: Some(seq as u32),
    })
}

/// Narrow the importer's [`MemberRole`] onto the sidecar's closed [`StackRole`]. The register
/// records only what a *view* needs — which member represents the stack, and which are proxies
/// — so every other role collapses to an ordinary member. The full role stays in the
/// `stack_members` index row.
fn stack_role(r: MemberRole) -> StackRole {
    match r {
        MemberRole::Primary => StackRole::Primary,
        MemberRole::Proxy => StackRole::Proxy,
        _ => StackRole::Member,
    }
}

fn role_str(r: MemberRole) -> &'static str {
    match r {
        MemberRole::Primary => "primary",
        MemberRole::Raw => "raw",
        MemberRole::Video => "video",
        MemberRole::Audio => "audio",
        MemberRole::DepthMap => "depth_map",
        MemberRole::Processed => "processed",
        MemberRole::Source => "source",
        MemberRole::Alternate => "alternate",
        MemberRole::Sidecar => "sidecar",
        MemberRole::Proxy => "proxy",
        MemberRole::Master => "master",
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::*;
    use crate::crypto::primitives::Argon2Params;
    use crate::crypto::verify_asset::VerifyOutcome;
    use crate::domain::ImportMode;
    use crate::import::planner::{ImportConfig, plan};
    use crate::import::scanner::scan;

    fn noop_event(_: ImportProgressEvent) {}

    /// A workspace with fast Argon2 params + its default album created, so imports have a signed
    /// destination (the executor resolves an unbound context to the derived de facto album).
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

    /// **The S-B13 contract (slice `S-B13`).** An original whose format has no codec in this
    /// build is imported as a signed, encrypted, verifiable asset — it simply arrives without a
    /// thumbnail/preview, and the run summary says so.
    ///
    /// **`S-C59` narrowed what this can assert.** It used to pin the distinction the logs must
    /// preserve: `iphone.heic` an *expected* deferral, `snap.jpg` a *genuine* decode failure of a
    /// format we do support. With `capsule_core::media` retired there is no decoder for any
    /// format, so both are deferrals and the distinction is unobservable — it comes back with
    /// Rawshift. What survives is the half that matters most and would be the worst to lose
    /// silently: **an undecodable original is still a signed, encrypted, self-verifying backup**,
    /// and both files land.
    #[test]
    fn originals_with_no_codec_are_still_imported_and_signed() {
        use crate::lifecycle::DerivativeStatus;

        let src = TempDir::new().unwrap();
        let lib_dir = TempDir::new().unwrap();
        fs::write(src.path().join("iphone.heic"), b"fake heic bytes").unwrap();
        fs::write(src.path().join("snap.jpg"), b"not really a jpeg").unwrap();

        let mut ws = signed_workspace(lib_dir.path());
        let scan_result = scan(&[src.path().to_path_buf()]).unwrap();
        let config = ImportConfig::default();
        let plan_result = plan(&scan_result, ws.db(), &config).unwrap();

        // Admission is untouched by codec coverage.
        assert_eq!(plan_result.counts.to_import, 2);
        assert_eq!(plan_result.counts.unsupported, 0);

        let token = CancellationToken::new();
        let summary = execute(&plan_result, &mut ws, &config, noop_event, &token).unwrap();

        assert_eq!(summary.imported_count(), 2, "both originals are backed up");
        assert_eq!(
            summary.deferred_derivative_count(),
            2,
            "with no decoder in the build, every still is a codec deferral"
        );
        assert_eq!(
            summary.decode_failed_count(),
            0,
            "nothing is *attempted*, so nothing can fail to decode — the distinction returns \
             with Rawshift"
        );

        // Reported per file rather than only in aggregate, so the shape a caller reads is
        // pinned even while there is one reason rather than two.
        for (path, outcome) in &summary.outcomes {
            let ImportOutcome::Imported { derivatives } = outcome else {
                panic!("{} should have imported, got {outcome:?}", path.display());
            };
            assert_eq!(
                *derivatives,
                DerivativeStatus::DeferredNoCodec,
                "for {}",
                path.display()
            );
        }

        // Both land on the signed path and self-verify — a missing thumbnail is not a missing
        // backup.
        let ids = ws.asset_ids();
        assert_eq!(ids.len(), 2);
        for id in &ids {
            assert_eq!(ws.verify(id).unwrap(), VerifyOutcome::Accept);
        }
    }

    /// A RAW-only candidate — no same-stem JPEG to fall back on — still lands as a signed,
    /// self-verifying original. RAW has no decoder in this build, which is exactly why this
    /// needs pinning: the archive is the whole point, the derivative is a bonus (slice `S-B13`).
    #[test]
    fn raw_only_candidate_lands_as_a_signed_original() {
        use crate::lifecycle::DerivativeStatus;

        let src = TempDir::new().unwrap();
        let lib_dir = TempDir::new().unwrap();
        fs::write(src.path().join("shot.ARW"), b"fake sony raw bytes").unwrap();

        let mut ws = signed_workspace(lib_dir.path());
        let scan_result = scan(&[src.path().to_path_buf()]).unwrap();
        let config = ImportConfig::default();
        let plan_result = plan(&scan_result, ws.db(), &config).unwrap();
        assert_eq!(plan_result.counts.to_import, 1);
        assert_eq!(plan_result.counts.unsupported, 0);

        let token = CancellationToken::new();
        let summary = execute(&plan_result, &mut ws, &config, noop_event, &token).unwrap();

        assert_eq!(summary.imported_count(), 1);
        assert_eq!(summary.deferred_derivative_count(), 1);
        assert_eq!(summary.decode_failed_count(), 0);
        assert!(matches!(
            summary.outcomes[0].1,
            ImportOutcome::Imported {
                derivatives: DerivativeStatus::DeferredNoCodec
            }
        ));

        let ids = ws.asset_ids();
        assert_eq!(ids.len(), 1);
        assert_eq!(ws.verify(&ids[0]).unwrap(), VerifyOutcome::Accept);
    }

    #[test]
    fn single_file_import_is_verify_asset_accepting() {
        let src = TempDir::new().unwrap();
        let lib_dir = TempDir::new().unwrap();
        fs::write(src.path().join("test.jpg"), b"fake jpeg content for test").unwrap();

        let mut ws = signed_workspace(lib_dir.path());
        let scan_result = scan(&[src.path().to_path_buf()]).unwrap();
        let config = ImportConfig::default();
        let plan_result = plan(&scan_result, ws.db(), &config).unwrap();
        assert_eq!(plan_result.counts.to_import, 1);

        let token = CancellationToken::new();
        let summary = execute(&plan_result, &mut ws, &config, noop_event, &token).unwrap();
        assert_eq!(summary.imported_count(), 1);

        // The imported asset lands on the signed path and self-verifies through the chokepoint.
        let ids = ws.asset_ids();
        assert_eq!(ids.len(), 1);
        assert_eq!(ws.verify(&ids[0]).unwrap(), VerifyOutcome::Accept);

        // The queryable index has exactly one visible row.
        assert_eq!(ws.db().query_timeline(0, 100).unwrap().len(), 1);
    }

    #[test]
    fn move_mode_deletes_source() {
        let src = TempDir::new().unwrap();
        let lib_dir = TempDir::new().unwrap();
        let photo = src.path().join("move_me.jpg");
        fs::write(&photo, b"jpeg to move").unwrap();

        let mut ws = signed_workspace(lib_dir.path());
        let scan_result = scan(&[src.path().to_path_buf()]).unwrap();
        let config = ImportConfig {
            import_mode: ImportMode::Move,
            ..Default::default()
        };
        let plan_result = plan(&scan_result, ws.db(), &config).unwrap();
        let token = CancellationToken::new();
        execute(&plan_result, &mut ws, &config, noop_event, &token).unwrap();

        assert!(
            !photo.exists(),
            "source file should be deleted in move mode"
        );
        let ids = ws.asset_ids();
        assert_eq!(ws.verify(&ids[0]).unwrap(), VerifyOutcome::Accept);
    }

    #[test]
    fn cancellation_stops_execution() {
        let src = TempDir::new().unwrap();
        let lib_dir = TempDir::new().unwrap();
        for i in 0..3 {
            fs::write(
                src.path().join(format!("photo_{i}.jpg")),
                format!("content_{i}").as_bytes(),
            )
            .unwrap();
        }

        let mut ws = signed_workspace(lib_dir.path());
        let scan_result = scan(&[src.path().to_path_buf()]).unwrap();
        let config = ImportConfig::default();
        let plan_result = plan(&scan_result, ws.db(), &config).unwrap();

        let token = CancellationToken::new();
        token.cancel(); // Cancel before starting.

        let summary = execute(&plan_result, &mut ws, &config, noop_event, &token).unwrap();
        assert_eq!(
            summary.outcomes.len(),
            0,
            "no files imported after immediate cancellation"
        );
        assert_eq!(ws.asset_ids().len(), 0);
    }

    #[test]
    fn raw_jpeg_stack_import_hides_secondary() {
        let src = TempDir::new().unwrap();
        let lib_dir = TempDir::new().unwrap();
        fs::write(src.path().join("img_0001.jpg"), b"jpeg content").unwrap();
        fs::write(src.path().join("img_0001.ARW"), b"raw content").unwrap();

        let mut ws = signed_workspace(lib_dir.path());
        let scan_result = scan(&[src.path().to_path_buf()]).unwrap();
        assert_eq!(
            scan_result.candidates.len(),
            1,
            "should form a single stack candidate"
        );

        let config = ImportConfig::default();
        let plan_result = plan(&scan_result, ws.db(), &config).unwrap();
        let token = CancellationToken::new();
        let summary = execute(&plan_result, &mut ws, &config, noop_event, &token).unwrap();

        assert_eq!(summary.imported_count(), 2, "both RAW and JPEG imported");
        // Both members verify on the signed path.
        for id in ws.asset_ids() {
            assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept);
        }
        // Only the primary is visible in the timeline (the RAW is stack-hidden).
        assert_eq!(
            ws.db().query_timeline(0, 100).unwrap().len(),
            1,
            "only the primary member is visible"
        );

        // S-B15: the grouping is *durable* — every member carries the signed
        // `stack_membership` register, sharing one stack id, with exactly one primary. Before
        // that slice the executor's placement was index columns and nothing else.
        let mut stack_ids = Vec::new();
        let mut primaries = 0;
        for id in ws.asset_ids() {
            let membership = ws
                .asset(&id)
                .unwrap()
                .sidecar
                .stack_membership
                .get()
                .and_then(Option::as_ref)
                .expect("the executor wrote the stack register")
                .clone();
            assert_eq!(membership.stack_type, StackType::RawJpeg);
            primaries += usize::from(membership.role == StackRole::Primary);
            stack_ids.push(membership.stack_id);
        }
        assert_eq!(primaries, 1, "exactly one member represents the stack");
        stack_ids.dedup();
        assert_eq!(stack_ids.len(), 1, "both members share one stack id");
    }
}
