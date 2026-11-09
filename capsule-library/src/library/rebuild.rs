//! Recovery-first rebuild of the SQLite index from the artifacts on disk.
//!
//! One sidecar shape is read here: the signed [`SidecarV1`] every write path emits
//! (`write_asset_files`, behind [`import_asset`](crate::lifecycle::Workspace::import_asset)
//! and every later metadata write). It carries the CRDT registers (`hidden`, `cull`,
//! `stack_membership`, rating, tags), and it is the write path's own output, so a rebuild
//! reconstructs exactly what the write path indexed.
//!
//! The retired unsigned pre-signed-path shape is **not** read: no reader of it remains. A
//! rebuild holds no key material and cannot sign a sidecar, a manifest, or seal a blob, so a
//! file that does not decode as the signed shape — that record included — is
//! [probed](capsule_core::sidecar::shape), warned about, and skipped, rather than indexed as an
//! asset the workspace cannot verify, export, or upload.
//!
//! What the signed shape restores:
//!
//! | state | source |
//! |---|---|
//! | `hidden` (gated Hidden view) | the `hidden` LWW register |
//! | trash (`is_deleted`/`deleted_at`) | the provenance chain's lifecycle actions |
//! | `album_id` | the provenance chain head manifest |
//! | stacks | the `stack_membership` LWW register |
//! | `cull` | *not an index projection* — see below |
//!
//! An **importer-formed** stack used to be the one thing the sidecar did not carry: an
//! older `import_asset_with` recorded it as `assets.stack_id` / `is_stack_hidden` and wrote no
//! `stack_membership` register, so it lived only in the index and a lost index lost it.
//! The importer now writes the register, so an importer-formed stack is reconstructed from
//! disk like any other. The preservation branch in [`signed_asset_row`] is therefore no longer
//! load-bearing for anything this build writes — it stays as the **compatibility path** for
//! libraries imported by that older importer, whose sidecars carry no register and whose
//! placement is still index-only.
//!
//! `cull` needs nothing here: it has no column in `assets` and no query projects it. The
//! culling views ([`Workspace::assets_by_cull`](crate::lifecycle::Workspace::assets_by_cull))
//! read the register straight off the signed sidecars that `Workspace::open` restores, so a
//! lost index costs it nothing. It is listed above so the absence is a recorded finding rather
//! than an oversight.
//!
//! Nothing here verifies a signature: a rebuild holds no key material (a [`Library`] has no
//! account), and these are the device's own local plaintext files — the same rule
//! `Workspace::open`'s add-id sweep follows. Verification is `verify_asset`'s job, on open.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use capsule_core::cbor;
use capsule_core::crypto::provenance::{Action, ProvenanceRecord};
use capsule_core::domain::StackType;
use capsule_core::sidecar::shape;
use capsule_core::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1, StackMembership, StackRole};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::db::rows::{AssetRow, AssetStackRow, StackMemberRow};
use crate::library::error::LibraryError;
use crate::library::library::Library;

/// A signed sidecar together with the directory it was found in (its provenance chain,
/// which carries the album and the trash state, is that directory's sibling file).
struct SignedOnDisk {
    dir: PathBuf,
    sidecar: SidecarV1,
}

/// The per-asset state that lives in the provenance chain rather than in the signed sidecar:
/// the owning album, and whether the asset is currently in the trash.
///
/// [`Default`] is the honest answer for an asset whose chain could not be read — album
/// unknown, not trashed — and it is always accompanied by a `warn`, because "not trashed"
/// is the permissive answer and a reader after the fact must be able to see it was a guess.
#[derive(Debug, Default)]
struct ChainFacts {
    album_id: Option<String>,
    is_deleted: bool,
    deleted_at: Option<i64>,
}

/// Rebuild the SQLite index from the sidecars on disk.
///
/// Every `{uuid}.cbor` under `media/` is decoded as the signed [`SidecarV1`] shape and upserted
/// as an `assets` row; stacks are then reconstructed from the `stack_membership` registers.
///
/// A sidecar that does not decode as the signed shape — a torn or foreign file, or a retired
/// unsigned pre-signed-path record — is warned about and skipped: a rebuild holds no keys and
/// cannot admit it, and one unreadable file must not cost the whole library its index.
#[tracing::instrument(skip_all, fields(root = %library.root.display()))]
pub fn rebuild_index(library: &Library) -> Result<(), LibraryError> {
    let media_dir = library.root.join("media");
    if !media_dir.exists() {
        tracing::info!("rebuild_index: no media directory; nothing to rebuild");
        return Ok(());
    }

    let mut signed: Vec<SignedOnDisk> = Vec::new();
    let mut skipped = 0usize;

    for entry in WalkDir::new(&media_dir).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        // The sidecar is exactly `{uuid}.cbor`. The sibling `{uuid}.provenance.cbor`,
        // `{uuid}.receipts.cbor` and `{uuid}.attestations.cbor` logs are read through their own
        // readers (or not at all) and must never be parsed as sidecars — the same filter
        // `Workspace::open` applies.
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !crate::library::paths::is_sidecar_file_name(&name) {
            continue;
        }

        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                skipped += 1;
                tracing::warn!(
                    sidecar = %path.display(),
                    error = %e,
                    "rebuild_index: unreadable sidecar file; skipping"
                );
                continue;
            }
        };

        // The signed shape is the only one read: it is what every write path emits, and it is
        // the only shape that carries the `hidden` register the default projections gate on.
        match SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1) {
            Ok(sidecar) => {
                let dir = path.parent().unwrap_or(&media_dir).to_path_buf();
                tracing::debug!(
                    sidecar = %path.display(),
                    asset_id = %sidecar.uuid,
                    shape = "sidecar-v1-signed",
                    "rebuild_index: decoded sidecar"
                );
                signed.push(SignedOnDisk { dir, sidecar });
            }
            Err(signed_err) => {
                skipped += 1;
                tracing::warn!(
                    sidecar = %path.display(),
                    shape = ?shape::probe(&bytes),
                    error = %signed_err,
                    "rebuild_index: sidecar does not decode as the signed shape; skipping"
                );
            }
        }
    }

    let mut hidden = 0usize;
    let mut trashed = 0usize;

    for found in &signed {
        let facts = chain_facts(&found.dir, &found.sidecar.uuid);
        // The row already in the index, if any. A rebuild normally runs against a lost index
        // and finds nothing here; when it is run against a live one (`capsule rebuild`), this
        // is what keeps it from erasing the index-only stack placement described below.
        let prior = library
            .db
            .find_by_uuid(&found.sidecar.uuid.to_string())
            .ok()
            .flatten();
        let row = signed_asset_row(&found.sidecar, &facts, prior.as_ref());
        hidden += usize::from(row.is_hidden);
        trashed += usize::from(row.is_deleted);
        tracing::trace!(
            asset_id = %row.uuid,
            album_id = ?row.album_id,
            is_hidden = row.is_hidden,
            is_deleted = row.is_deleted,
            stack_id = ?row.stack_id,
            "rebuild_index: upserting row rebuilt from a signed sidecar"
        );
        library.db.upsert_asset(&row)?;
    }

    let signed_stacks = rebuild_signed_stacks(library, &signed);

    tracing::info!(
        signed = signed.len(),
        skipped,
        hidden,
        trashed,
        signed_stacks,
        "rebuild_index: index rebuilt from on-disk sidecars"
    );
    Ok(())
}

// ── the signed shape ────────────────────────────────────────────────────────

/// Read `{uuid}.provenance.cbor` and replay its lifecycle actions.
///
/// The chain — not the sidecar and not the old index — is the source of truth for the trash
/// state (a `delete` moves an asset to trash, a later `trash-restore` brings it back) and for
/// the owning album (the head manifest names it). Signatures are not checked here; see the
/// module docs.
fn chain_facts(dir: &Path, asset_id: &Uuid) -> ChainFacts {
    let path = dir.join(format!("{}.provenance.cbor", asset_id.simple()));
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                asset_id = %asset_id,
                provenance = %path.display(),
                error = %e,
                "rebuild_index: no readable provenance chain; rebuilding this asset with an \
                 unknown album and as NOT trashed"
            );
            return ChainFacts::default();
        }
    };
    let records: Vec<ProvenanceRecord> = match cbor::from_slice(&bytes) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                asset_id = %asset_id,
                provenance = %path.display(),
                error = %e,
                "rebuild_index: undecodable provenance chain; rebuilding this asset with an \
                 unknown album and as NOT trashed"
            );
            return ChainFacts::default();
        }
    };

    let mut facts = ChainFacts::default();
    for rec in &records {
        match rec.manifest.core.action {
            Action::Delete => {
                facts.is_deleted = true;
                facts.deleted_at = rfc3339_to_secs(&rec.manifest.core.timestamp);
            }
            Action::TrashRestore => {
                facts.is_deleted = false;
                facts.deleted_at = None;
            }
            _ => {}
        }
    }
    facts.album_id = records
        .last()
        .map(|rec| rec.manifest.core.album_id.to_string());
    tracing::debug!(
        asset_id = %asset_id,
        records = records.len(),
        album_id = ?facts.album_id,
        is_deleted = facts.is_deleted,
        "rebuild_index: replayed provenance chain"
    );
    facts
}

/// Project a signed sidecar (plus its chain facts) onto an `assets` row.
///
/// This mirrors `lifecycle::asset_row_from_state`, which is what the write path indexes: the
/// two must agree, or a rebuild would silently change what the views show. Media-derived
/// columns (`chromahash`, `dominant_color`, `duration_ms`) stay NULL for the same reason they
/// do there.
fn signed_asset_row(s: &SidecarV1, facts: &ChainFacts, prior: Option<&AssetRow>) -> AssetRow {
    // The timeline is dated by the user's correction when there is one, else the as-imported
    // instant — the same rule the write path applies.
    let capture = s.effective_capture_timestamp();
    let capture_utc = rfc3339_to_secs(capture);
    if capture_utc.is_none() {
        tracing::warn!(
            asset_id = %s.uuid,
            capture_timestamp = %capture,
            "rebuild_index: unparseable capture timestamp; indexing it as the epoch"
        );
    }
    // Three cases, and the middle one is why this matches on `get()` rather than flattening
    // through `and_then(Option::as_ref)`: a *stamped* `None` and a *never-written* register are
    // different facts, and collapsing them resurrects placement the user deliberately removed.
    // Mirrors `lifecycle::import::asset_row_from_state`, which is the write path's answer to the
    // same question.
    let register = s.stack_membership.get();
    let (stack_id, is_stack_hidden) = match register {
        // Stamped with a membership: project it. Every non-primary member is suppressed from the
        // timeline, exactly as the write path arranges it.
        Some(membership) => membership.as_ref().map_or((None, false), |m| {
            // Stamped `None` — the asset left the stack. The register is authoritative and says
            // so, so the columns are cleared rather than reconstructed from a stale index row.
            (Some(m.stack_id.to_string()), m.role != StackRole::Primary)
        }),
        // No register on disk at all. Compatibility path only: every
        // importer-formed stack this build writes carries the register, so reaching here means a
        // **library imported by the older importer**, whose placement was written only to the
        // index and lives nowhere else. Whatever the row already carries is then the only copy in existence, and a rebuild
        // must not erase it.
        None => prior.map_or((None, false), |p| (p.stack_id.clone(), p.is_stack_hidden)),
    };
    if register.is_none() && stack_id.is_some() {
        tracing::debug!(
            asset_id = %s.uuid,
            stack_id = ?stack_id,
            "rebuild_index: no stack_membership register on disk (asset written before the register existed); keeping \
             the index-only stack placement the existing row carries"
        );
    }
    AssetRow {
        uuid: s.uuid.to_string(),
        asset_type: if s.content_type.starts_with("video/") {
            "video".to_string()
        } else {
            "photo".to_string()
        },
        capture_timestamp: capture_utc.unwrap_or(0),
        capture_utc: Some(capture_utc.unwrap_or(0)),
        capture_tz_source: None,
        import_timestamp: rfc3339_to_secs(&s.import_timestamp).unwrap_or(0),
        hash_sha256: s.hash.to_hex(),
        width: s.dimensions.as_ref().map(|d| i64::from(d.width)),
        height: s.dimensions.as_ref().map(|d| i64::from(d.height)),
        duration_ms: None,
        stack_id,
        is_stack_hidden,
        chromahash: None,
        dominant_color: None,
        album_id: facts.album_id.clone(),
        rating: i64::from(s.rating.get().copied().unwrap_or(0)),
        is_deleted: facts.is_deleted,
        deleted_at: facts.deleted_at,
        // The register this rebuild exists for: a never-written
        // register means visible, the wire-absent default.
        is_hidden: s.hidden.get().copied().unwrap_or(false),
        // The caption register and the effective location (the user's edit over the
        // as-imported fix), projected as the write path projects them.
        caption: s.caption.get().filter(|c| !c.is_empty()).cloned(),
        latitude: s.effective_gps().map(|g| g.lat),
        longitude: s.effective_gps().map(|g| g.lon),
    }
}

/// Reconstruct `asset_stacks` / `stack_members` from the signed `stack_membership` registers.
/// Returns the number of stacks written.
fn rebuild_signed_stacks(library: &Library, signed: &[SignedOnDisk]) -> usize {
    // stack id → its members, as (asset uuid, membership).
    let mut groups: HashMap<Uuid, Vec<(Uuid, StackMembership)>> = HashMap::new();
    for found in signed {
        if let Some(Some(membership)) = found.sidecar.stack_membership.get() {
            groups
                .entry(membership.stack_id)
                .or_default()
                .push((found.sidecar.uuid, membership.clone()));
        }
    }

    let now = now_secs();
    for (stack_id, members) in &mut groups {
        // Deterministic order regardless of directory-walk order: declared index first,
        // then asset id.
        members.sort_by_key(|(uuid, m)| (m.member_index, *uuid));
        let primary = members
            .iter()
            .find(|(_, m)| m.role == StackRole::Primary)
            .or_else(|| members.first())
            .map(|(uuid, _)| uuid.to_string());
        let Some(primary) = primary else {
            continue;
        };
        let stack_type = members
            .first()
            .map_or("custom", |(_, m)| stack_type_str(m.stack_type));

        let stack_row = AssetStackRow {
            id: stack_id.to_string(),
            stack_type: stack_type.to_string(),
            primary_asset_id: primary.clone(),
            cover_asset_id: Some(primary.clone()),
            is_collapsed: true,
            // The register records the grouping, not how it came about, so a rebuilt stack
            // makes no claim to have been auto-detected.
            is_auto_generated: false,
            created_at: now,
            modified_at: now,
        };
        if let Err(e) = library.db.insert_stack(&stack_row) {
            // Idempotent on rebuild: the row is already there from an earlier pass.
            tracing::debug!(stack_id = %stack_id, error = %e, "rebuild_index: stack row already present");
        }

        for (i, (uuid, membership)) in members.iter().enumerate() {
            let member_row = StackMemberRow {
                id: format!("{stack_id}#{i}"),
                stack_id: stack_id.to_string(),
                asset_id: uuid.to_string(),
                sequence_order: membership.member_index.map_or(i as i64, i64::from),
                member_role: stack_role_str(membership.role).to_string(),
                created_at: now,
            };
            if let Err(e) = library.db.insert_stack_member(&member_row) {
                tracing::debug!(
                    stack_id = %stack_id,
                    asset_id = %uuid,
                    error = %e,
                    "rebuild_index: stack member row already present"
                );
            }
        }
        tracing::debug!(
            stack_id = %stack_id,
            members = members.len(),
            primary_asset_id = %primary,
            "rebuild_index: stack reconstructed from signed stack_membership registers"
        );
    }
    groups.len()
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn rfc3339_to_secs(s: &str) -> Option<i64> {
    s.parse::<jiff::Timestamp>()
        .ok()
        .map(|t: jiff::Timestamp| t.as_second())
}

/// The `stack_members.member_role` string for a signed membership's role.
fn stack_role_str(r: StackRole) -> &'static str {
    match r {
        StackRole::Primary => "primary",
        StackRole::Member => "member",
        StackRole::Proxy => "proxy",
    }
}

fn stack_type_str(st: StackType) -> &'static str {
    match st {
        StackType::RawJpeg => "raw_jpeg",
        StackType::Burst => "burst",
        StackType::LivePhoto => "live_photo",
        StackType::Portrait => "portrait",
        StackType::SmartSelection => "smart_selection",
        StackType::HdrBracket => "hdr_bracket",
        StackType::FocusStack => "focus_stack",
        StackType::PixelShift => "pixel_shift",
        StackType::Panorama => "panorama",
        StackType::Proxy => "proxy",
        StackType::Chaptered => "chaptered",
        StackType::DualAudio => "dual_audio",
        StackType::Custom => "custom",
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// The contract these tests hold `rebuild_index` to:
//
// signed shape (`SidecarV1` — what every current write path emits)
//   1. a hidden asset comes back hidden, and stays out of the default projections
//   2. a visible asset comes back visible (the control: hiding is not the default)
//   3. `stack_membership` comes back as `stack_id` + `is_stack_hidden` + stack rows
//   3b. an index-only (importer-formed) placement is preserved, never overwritten with NULL
//   4. the trash state and the owning album come back from the provenance chain
//   5. a missing chain degrades to "album unknown, not trashed" rather than failing
//   6. rebuilding twice changes nothing
//   7. the `{uuid}.provenance.cbor` / `{uuid}.receipts.cbor` / `{uuid}.attestations.cbor`
//      siblings are not sidecars, and a real attestation log survives the rebuild untouched
//   8. `cull` needs no rebuild support — it is not an index projection (audit finding)
//
// the retired unsigned shape
//   9. the two shapes are disjoint on the wire, so the probe cannot mis-route a file
//   10. an unsigned sidecar is reported and not indexed; the signed asset beside it is
#[cfg(test)]
mod tests;
