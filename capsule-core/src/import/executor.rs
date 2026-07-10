//! Phase 4 — execute an import plan onto the **signed lifecycle path** (slice `S-B2`).
//!
//! Each `ImportDecision::Import` candidate is imported through
//! [`Workspace::import_asset_with`](crate::lifecycle::Workspace::import_asset_with): every member
//! becomes a signed [`SidecarV1`](crate::sidecar::sidecar_v1::SidecarV1) + signed manifest +
//! append-only provenance, self-verified through
//! [`verify_asset`](crate::crypto::verify_asset::verify_asset), and — behind the `media` feature,
//! when a [`StillEncoder`](crate::media::image::derivative::StillEncoder) is attached to the
//! workspace — with signed thumbnail/preview derivatives + an LQIP in the sidecar.
//!
//! This retires the legacy unsigned `AssetSidecar` write path from the executor (that write path
//! itself is deleted wholesale later, in `S-G4`). The pure planner (`import::planner`) is
//! unchanged: it still decides *what* to import; the executor decides *how* to commit it.

use std::path::PathBuf;

use uuid::Uuid;

use crate::db::rows::{AssetStackRow, StackMemberRow};
use crate::domain::{ImportMode, MemberRole};
use crate::import::executor_cancellation::CancellationToken;
use crate::import::planner::{ImportActionPlan, ImportConfig, ImportDecision};
use crate::import::progress::{ImportExecutionSummary, ImportOutcome, ImportProgressEvent};
use crate::import::scan::ImportCandidate;
use crate::lifecycle::{LifecycleError, SignedImportOptions, StackPlacement, Workspace};

type ExecError = Box<dyn std::error::Error + Send + Sync>;

/// Phase 4 — execute the import plan against `workspace`.
///
/// Every `ImportDecision::Import` candidate is committed through the signed lifecycle path; skip
/// decisions are reported verbatim. Assets are written into the album resolved from
/// `config.target_album_id` (a UUID string) or, when unset, the workspace's default album — which
/// must already exist in the workspace.
#[tracing::instrument(
    skip_all,
    fields(candidates = plan.actions.len(), mode = ?config.import_mode)
)]
pub fn execute(
    plan: &ImportActionPlan,
    workspace: &mut Workspace,
    config: &ImportConfig,
    on_event: impl Fn(ImportProgressEvent),
    cancel: &CancellationToken,
) -> Result<ImportExecutionSummary, ExecError> {
    let album_id = match &config.target_album_id {
        Some(s) => Uuid::parse_str(s).map_err(|e| format!("invalid target album id {s:?}: {e}"))?,
        None => workspace.default_album_id(),
    };

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
            ImportDecision::Import => execute_candidate(candidate, workspace, config, album_id)?,
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
        "import: execution complete"
    );
    Ok(summary)
}

// ── Per-candidate execution ──────────────────────────────────────────────────

/// Import every member of `candidate` through the signed path, then persist its stack grouping
/// (if any). Move-mode source release and stack placement are handled inside the signed import.
fn execute_candidate(
    candidate: &ImportCandidate,
    workspace: &mut Workspace,
    config: &ImportConfig,
    album_id: Uuid,
) -> Result<Vec<(PathBuf, ImportOutcome)>, ExecError> {
    let move_source = matches!(config.import_mode, ImportMode::Move);
    // One stack id per multi-file candidate; the primary member stays visible, the rest hidden.
    let stack_id = candidate
        .stack_type
        .map(|_| format!("stack-{}", Uuid::now_v7().simple()));

    let mut outcomes = Vec::new();
    let mut imported: Vec<(String, MemberRole)> = Vec::new();

    for (seq, (path, role)) in candidate.members.iter().enumerate() {
        let is_primary = *role == MemberRole::Primary || seq == 0;
        let stack = stack_id.as_ref().map(|sid| StackPlacement {
            stack_id: sid.clone(),
            hidden: !is_primary,
        });
        // Offline import: release the Move source on the local durable commit. The online /
        // streaming path (S-B3) sets `defer_source_release` and gates on the server verdict.
        let opts = SignedImportOptions {
            move_source,
            defer_source_release: false,
            stack,
        };

        match workspace.import_asset_with(album_id, path, &opts) {
            Ok(uuid) => {
                imported.push((uuid.to_string(), *role));
                outcomes.push((path.clone(), ImportOutcome::Imported));
            }
            Err(e) => outcomes.push((path.clone(), import_error_outcome(&e))),
        }
    }

    if let Some(sid) = &stack_id
        && !imported.is_empty()
    {
        persist_stack(workspace, candidate, sid, &imported)?;
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
    /// destination (the executor resolves `target_album_id: None` to the default album).
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
    }

    /// S-B2: an executor import produces `verify_asset`-accepting assets **with signed
    /// derivatives** when a `StillEncoder` is attached (behind the `media` feature).
    #[cfg(feature = "media")]
    #[test]
    fn import_generates_signed_derivatives() {
        use crate::media::image::buffer::{ComponentType, ImageBuffer, PixelFormat};
        use crate::media::image::derivative::{DerivativeFormat, DerivativeTier, StillEncoder};
        use crate::media::image::formats::jpeg::JpegImage;
        use crate::media::image::metadata::ImageMetadata;
        use crate::media::image::types::ImageFormat;
        use crate::media::image::{Image, ImageEncode};
        use crate::media::metadata::ColorSpace;

        /// Deterministic in-test byte encoder standing in for the SDK's per-platform codecs.
        struct TagEncoder;
        impl StillEncoder for TagEncoder {
            fn encode(
                &self,
                buffer: &ImageBuffer,
                format: DerivativeFormat,
                _tier: DerivativeTier,
            ) -> Result<Vec<u8>, crate::media::image::buffer::ImageBufferError> {
                let tag: u8 = match format {
                    DerivativeFormat::Jxl => 0x4A,
                    DerivativeFormat::Avif => 0xAF,
                    DerivativeFormat::WebP => 0x7B,
                    DerivativeFormat::Original => 0x00,
                };
                let mut v = Vec::with_capacity(buffer.data.len() + 1);
                v.push(tag);
                v.extend_from_slice(&buffer.data);
                Ok(v)
            }
        }

        // A real, decodable JPEG (512×384 gradient) so the still decode + derivative path runs.
        fn gradient_jpeg() -> Vec<u8> {
            let (w, h) = (512usize, 384usize);
            let mut data = Vec::with_capacity(w * h * 3);
            for y in 0..h {
                for x in 0..w {
                    data.push((x % 256) as u8);
                    data.push((y % 256) as u8);
                    data.push(((x + y) % 256) as u8);
                }
            }
            let buffer = ImageBuffer::new(
                data,
                w,
                h,
                PixelFormat::Rgb,
                ComponentType::U8,
                ColorSpace::Srgb,
            )
            .unwrap();
            let meta = ImageMetadata {
                format: Some(ImageFormat::Jpeg),
                width: w as u32,
                height: h as u32,
                bit_depth: 8,
                color_space: ColorSpace::Srgb,
                ..Default::default()
            };
            JpegImage::from_raw_parts(buffer, meta)
                .unwrap()
                .encode_to_bytes()
                .unwrap()
        }

        let src = TempDir::new().unwrap();
        let lib_dir = TempDir::new().unwrap();
        fs::write(src.path().join("photo.jpg"), gradient_jpeg()).unwrap();

        let mut ws = signed_workspace(lib_dir.path()).with_still_encoder(Box::new(TagEncoder));
        let scan_result = scan(&[src.path().to_path_buf()]).unwrap();
        let config = ImportConfig::default();
        let plan_result = plan(&scan_result, ws.db(), &config).unwrap();

        let token = CancellationToken::new();
        let summary = execute(&plan_result, &mut ws, &config, noop_event, &token).unwrap();
        assert_eq!(summary.imported_count(), 1);

        let ids = ws.asset_ids();
        assert_eq!(ws.verify(&ids[0]).unwrap(), VerifyOutcome::Accept);

        // The sidecar carries an LQIP computed from the decoded still.
        assert!(
            ws.asset(&ids[0]).unwrap().sidecar.lqip.is_some(),
            "LQIP should be populated for a decodable still"
        );

        // Signed derivatives are persisted: the manifest bundle plus the encoded tiers.
        let stem = ids[0].simple().to_string();
        let deriv_files: Vec<_> = walkdir::WalkDir::new(lib_dir.path().join("media"))
            .into_iter()
            .filter_map(std::result::Result::ok)
            .filter(|e| {
                e.path()
                    .to_string_lossy()
                    .contains(&format!("derivatives/{stem}"))
            })
            .filter(|e| e.path().is_file())
            .collect();
        assert!(
            deriv_files
                .iter()
                .any(|e| e.path().to_string_lossy().ends_with(".derivatives.cbor")),
            "the signed derivative manifest bundle should exist"
        );
        // Thumbnail (>256px → 3 formats) + preview (3 formats) tiers were written to disk.
        let tier_files = deriv_files
            .iter()
            .filter(|e| !e.path().to_string_lossy().ends_with(".derivatives.cbor"))
            .count();
        assert!(
            tier_files >= 2,
            "expected multiple derivative tier files, got {tier_files}"
        );
    }
}
