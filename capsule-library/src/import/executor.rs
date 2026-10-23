//! Phase 4 — execute an import plan onto the **signed lifecycle path**.
//!
//! Each `ImportDecision::Import` candidate is imported through
//! [`Workspace::import_asset_with`](crate::lifecycle::Workspace::import_asset_with): every member
//! becomes a signed [`SidecarV1`](capsule_core::sidecar::SidecarV1) + signed manifest +
//! append-only provenance, self-verified through
//! [`verify_asset`](capsule_core::crypto::verify_asset::verify_asset), and — when the still decodes —
//! with a chromahash `lqip` in the sidecar and signed thumbnail derivatives on disk
//! ([`capsule_core::media`]). A format with no
//! codec in this build still imports, as a signed original with the gap recorded rather than
//! hidden.
//!
//! This retired the legacy unsigned `AssetSidecar` write path from the executor; the production
//! write path itself is now gone — no code writes unsigned sidecars anymore. Only the
//! *read* path survives, for the recovery-first index rebuild
//! ([`rebuild_index`](crate::library::rebuild_index)) that still ingests unsigned `.cbor`
//! sidecars left by pre-signed-path libraries. The pure planner (`import::planner`) is unchanged:
//! it still decides *what* to import; the executor decides *how* to commit it.

use std::path::{Path, PathBuf};

use capsule_core::domain::{ImportMode, MemberRole, StackType};
use capsule_core::import::default_album::resolve_default_album;
use capsule_core::import::scan::ImportCandidate;
use capsule_core::sidecar::sidecar_v1::{StackMembership, StackRole};
use uuid::Uuid;

use crate::db::rows::{AssetStackRow, StackMemberRow};
use crate::import::enrichment::{SourceMetadataIndex, sidecar_enrichment};
use crate::import::executor_cancellation::CancellationToken;
use crate::import::planner::{ImportActionPlan, ImportConfig, ImportDecision};
use crate::import::progress::{ImportExecutionSummary, ImportOutcome, ImportProgressEvent};
use crate::lifecycle::{LifecycleError, SidecarEnrichment, SignedImportOptions, Workspace};

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
/// each file it covers.
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
        // Imported, but with no thumbnail/preview: the first is the expected no-codec gap,
        // the second a genuine decode failure of a format we do support.
        derivatives_deferred = summary.deferred_derivative_count(),
        derivatives_decode_failed = summary.decode_failed_count(),
        // How much of this run carried third-party exporter metadata into the signed sidecars; zero
        // for a plain filesystem import.
        enriched_candidates,
        "import: execution complete"
    );
    Ok(summary)
}

// ── Per-member enrichment ────────────────────────────────────────────────────

/// The signed-sidecar enrichment for one member path, with the fold decision logged.
///
/// The single place a [`SourceMetadataIndex`] is turned into a [`SidecarEnrichment`], shared by
/// this executor and the [streaming window](crate::import::streaming) so a *streamed*
/// third-party import writes exactly what a bulk one does. A path the index does not cover, or a record that folds to nothing, yields
/// [`None`] — the untouched write path.
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
        // A Move source is never deleted on the local commit: until the server holds the asset
        // it may be the only other copy (invariant 7). The caller releases it through
        // `Workspace::release_move_source` once the server's custody clears the gate, using the
        // asset id each `Imported` outcome carries.
        let opts = SignedImportOptions {
            move_source,
            defer_source_release: true,
            stack: membership,
            enrichment,
        };

        match workspace.import_asset_with(album_id, path, &opts) {
            Ok(receipt) => {
                imported.push((receipt.asset_id.to_string(), *role));
                outcomes.push((
                    path.clone(),
                    ImportOutcome::Imported {
                        asset_id: receipt.asset_id,
                        derivatives: receipt.derivatives,
                        deferred_formats: receipt.deferred_formats,
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

/// The signed [`StackMembership`] one member of a multi-file candidate carries, or
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
mod tests;
