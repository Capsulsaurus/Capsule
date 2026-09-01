//! Recovery-first rebuild of the SQLite index from the artifacts on disk.
//!
//! Two sidecar shapes can be found under `media/`, and this module reads both:
//!
//! * [`SidecarV1`] — the **signed** record every current write path emits (`write_asset_files`,
//!   behind [`import_asset`](crate::lifecycle::Workspace::import_asset) and every later
//!   metadata write). It carries the CRDT registers (`hidden`, `cull`, `stack_membership`,
//!   rating, tags), so it is the shape a rebuild must prefer: it is the write path's own
//!   output.
//! * [`AssetSidecar`] — the unsigned pre-signed-path shape. Its *write* path was retired by
//!   `S-B2`/`S-G4`; the **read** stays as the compatibility case for libraries written before
//!   the signed path existed. It has no register fields at all.
//!
//! Reading only the unsigned shape (the pre-`S-D21` behaviour) meant a rebuilt library came
//! back with every asset visible and un-trashed — a gate bypass, because rebuild is the
//! recovery path and the state it cannot carry is state the user cannot re-assert.
//!
//! What each shape can restore:
//!
//! | state | signed `SidecarV1` | unsigned `AssetSidecar` |
//! |---|---|---|
//! | `hidden` (gated Hidden view) | the `hidden` LWW register | absent — no such field |
//! | trash (`is_deleted`/`deleted_at`) | the provenance chain's lifecycle actions | the `is_deleted`/`deleted_at` fields |
//! | `album_id` | the provenance chain head manifest | the `album_id` field |
//! | stacks | the `stack_membership` LWW register | the `stack_hint` field |
//! | `cull` | *not an index projection* — see below | absent — no such field |
//!
//! An **importer-formed** stack used to be the one thing neither shape carried: pre-`S-B15`,
//! `import_asset_with` recorded it as `assets.stack_id` / `is_stack_hidden` and wrote no
//! `stack_membership` register, so it lived only in the index and a lost index lost it.
//! `S-B15` closed that: the importer now writes the register, so an importer-formed stack is
//! reconstructed from disk like any other. The preservation branch in [`signed_asset_row`] is
//! therefore no longer load-bearing for anything this build writes — it stays as the
//! **compatibility path** for libraries imported before `S-B15`, whose sidecars carry no
//! register and whose placement is still index-only.
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

use uuid::Uuid;
use walkdir::WalkDir;

use crate::cbor;
use crate::crypto::provenance::ProvenanceRecord;
use crate::crypto::provenance::action::Action;
use crate::db::rows::{AssetRow, AssetStackRow, StackMemberRow};
use crate::domain::{CaptureTzSource, DetectionMethod, MemberRole, StackType};
use crate::library::error::LibraryError;
use crate::library::library::Library;
use crate::metadata::AssetType;
use crate::sidecar::AssetSidecar;
use crate::sidecar::io::read_sidecar;
use crate::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1, StackMembership, StackRole};

type StackGroupKey = (String, String);
type StackGroupMembers = Vec<(String, String, StackType)>;

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
/// Every `{uuid}.cbor` under `media/` is decoded — preferring the signed [`SidecarV1`] shape
/// and falling back to the unsigned [`AssetSidecar`] compatibility shape — and upserted as an
/// `assets` row. Stacks are then reconstructed: from the `stack_membership` register for
/// signed sidecars, from `stack_hint` for unsigned ones.
///
/// A sidecar that decodes as neither shape is warned about and skipped: one unreadable file
/// must not cost the whole library its index.
#[tracing::instrument(skip_all, fields(root = %library.root.display()))]
pub fn rebuild_index(library: &Library) -> Result<(), LibraryError> {
    let media_dir = library.root.join("media");
    if !media_dir.exists() {
        tracing::info!("rebuild_index: no media directory; nothing to rebuild");
        return Ok(());
    }

    let mut signed: Vec<SignedOnDisk> = Vec::new();
    let mut legacy: Vec<AssetSidecar> = Vec::new();
    let mut skipped = 0usize;

    for entry in WalkDir::new(&media_dir).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        // The sidecar is exactly `{uuid}.cbor`. The sibling `{uuid}.provenance.cbor` and
        // `{uuid}.receipts.cbor` logs are read through their own readers (or not at all) and
        // must never be parsed as sidecars — the same filter `Workspace::open` applies.
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !name.ends_with(".cbor")
            || name.ends_with(".provenance.cbor")
            || name.ends_with(".receipts.cbor")
        {
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

        // Signed shape first: it is what every current write path emits, and it is the only
        // shape that carries the `hidden` register the default projections gate on.
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
            Err(signed_err) => match read_sidecar(path) {
                // Compatibility case (`S-B2`/`S-G4`): a library written before the signed
                // path existed. Nothing writes this shape any more, so nothing here can
                // restore a register it never had.
                Ok(sidecar) => {
                    tracing::debug!(
                        sidecar = %path.display(),
                        asset_id = %sidecar.uuid,
                        shape = "asset-sidecar-unsigned",
                        "rebuild_index: decoded pre-signed-path sidecar"
                    );
                    legacy.push(sidecar);
                }
                Err(legacy_err) => {
                    skipped += 1;
                    tracing::warn!(
                        sidecar = %path.display(),
                        signed_error = %signed_err,
                        unsigned_error = %legacy_err,
                        "rebuild_index: sidecar decodes as neither the signed nor the \
                         pre-signed-path shape; skipping"
                    );
                }
            },
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

    for sidecar in &legacy {
        let row = legacy_asset_row(sidecar);
        trashed += usize::from(row.is_deleted);
        tracing::trace!(
            asset_id = %row.uuid,
            album_id = ?row.album_id,
            is_deleted = row.is_deleted,
            "rebuild_index: upserting row rebuilt from a pre-signed-path sidecar"
        );
        library.db.upsert_asset(&row)?;
    }

    let signed_stacks = rebuild_signed_stacks(library, &signed);
    let legacy_stacks = rebuild_legacy_stacks(library, &legacy);

    tracing::info!(
        signed = signed.len(),
        unsigned = legacy.len(),
        skipped,
        hidden,
        trashed,
        signed_stacks,
        unsigned_stacks = legacy_stacks,
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
    let capture_utc = rfc3339_to_secs(&s.capture_timestamp);
    if capture_utc.is_none() {
        tracing::warn!(
            asset_id = %s.uuid,
            capture_timestamp = %s.capture_timestamp,
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
        // No register on disk at all. Compatibility path only, since `S-B15`: every
        // importer-formed stack this build writes carries the register, so reaching here means a
        // **pre-`S-B15` library**, whose placement was written only to the index and lives nowhere
        // else. Whatever the row already carries is then the only copy in existence, and a rebuild
        // must not erase it.
        None => prior.map_or((None, false), |p| (p.stack_id.clone(), p.is_stack_hidden)),
    };
    if register.is_none() && stack_id.is_some() {
        tracing::debug!(
            asset_id = %s.uuid,
            stack_id = ?stack_id,
            "rebuild_index: no stack_membership register on disk (pre-S-B15 asset); keeping \
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
        // The register this whole slice exists for (`S-D19`/`S-D21`): a never-written
        // register means visible, the wire-absent default.
        is_hidden: s.hidden.get().copied().unwrap_or(false),
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

// ── the pre-signed-path compatibility shape ─────────────────────────────────

/// Project an unsigned pre-signed-path sidecar onto an `assets` row.
///
/// This shape predates every CRDT register, so `is_hidden` is necessarily `false` here: the
/// file carries no `hidden` field to read. That is a property of the old on-disk format, not
/// a projection choice — a library that was ever written by the signed path has a
/// [`SidecarV1`] instead, and takes the branch above.
fn legacy_asset_row(s: &AssetSidecar) -> AssetRow {
    AssetRow {
        uuid: s.uuid.clone(),
        asset_type: asset_type_str(s.asset_type).to_string(),
        capture_timestamp: s.capture_timestamp.unwrap_or(s.import_timestamp),
        capture_utc: s.capture_utc,
        capture_tz_source: s.capture_tz_source.map(|c| tz_source_str(c).to_string()),
        import_timestamp: s.import_timestamp,
        hash_sha256: s.hash_sha256.clone(),
        width: s.width.map(i64::from),
        height: s.height.map(i64::from),
        duration_ms: s.duration_ms.map(|d| d as i64),
        stack_id: None,
        is_stack_hidden: false,
        chromahash: None,
        dominant_color: None,
        album_id: s.album_id.clone(),
        rating: i64::from(s.rating),
        is_deleted: s.is_deleted,
        deleted_at: s.deleted_at,
        is_hidden: false,
    }
}

/// Reconstruct stacks from the unsigned shape's `stack_hint` fields, grouping by
/// `(detection_key, detection_method)`. Returns the number of stacks written.
fn rebuild_legacy_stacks(library: &Library, legacy: &[AssetSidecar]) -> usize {
    let mut groups: HashMap<StackGroupKey, StackGroupMembers> = HashMap::new();

    for sidecar in legacy {
        if let Some(hint) = &sidecar.stack_hint {
            let method_str = detection_method_str(hint.detection_method);
            groups
                .entry((hint.detection_key.clone(), method_str.to_string()))
                .or_default()
                .push((
                    sidecar.uuid.clone(),
                    member_role_str(hint.member_role).to_string(),
                    hint.stack_type,
                ));
        }
    }

    let now = now_secs();
    for ((detection_key, detection_method), members) in &groups {
        let stack_id = format!("{detection_method}:{detection_key}");
        let Some(primary_uuid) = members
            .iter()
            .find(|(_, role, _)| role == "primary")
            .or_else(|| members.first())
            .map(|(uuid, _, _)| uuid.clone())
        else {
            continue;
        };

        let stack_type_str = members
            .first()
            .map_or("custom", |(_, _, st)| stack_type_str(*st));

        let stack_row = AssetStackRow {
            id: stack_id.clone(),
            stack_type: stack_type_str.to_string(),
            primary_asset_id: primary_uuid.clone(),
            cover_asset_id: Some(primary_uuid.clone()),
            is_collapsed: true,
            is_auto_generated: true,
            created_at: now,
            modified_at: now,
        };
        // Ignore error if stack already exists (idempotent on rebuild).
        let _ = library.db.insert_stack(&stack_row);

        for (i, (uuid, role, _)) in members.iter().enumerate() {
            let member_row = StackMemberRow {
                id: format!("{stack_id}#{i}"),
                stack_id: stack_id.clone(),
                asset_id: uuid.clone(),
                sequence_order: i as i64,
                member_role: role.clone(),
                created_at: now,
            };
            let _ = library.db.insert_stack_member(&member_row);

            let is_primary = uuid == &primary_uuid;
            let _ = library.db.update_stack_hidden(uuid, !is_primary);
        }
        tracing::debug!(
            stack_id = %stack_id,
            members = members.len(),
            "rebuild_index: stack reconstructed from pre-signed-path stack hints"
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

fn asset_type_str(t: AssetType) -> &'static str {
    match t {
        AssetType::Photo => "photo",
        AssetType::Video => "video",
        AssetType::Sidecar => "sidecar",
    }
}

fn tz_source_str(s: CaptureTzSource) -> &'static str {
    match s {
        CaptureTzSource::OffsetExif => "offset_exif",
        CaptureTzSource::GpsLookup => "gps_lookup",
        CaptureTzSource::Floating => "floating",
    }
}

fn detection_method_str(m: DetectionMethod) -> &'static str {
    match m {
        DetectionMethod::FilenameStem => "filename_stem",
        DetectionMethod::ContentIdentifier => "content_identifier",
        DetectionMethod::Timecode => "timecode",
        DetectionMethod::Manual => "manual",
    }
}

fn member_role_str(r: MemberRole) -> &'static str {
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

/// The `stack_members.member_role` string for a signed membership's role. Shares the
/// vocabulary of [`member_role_str`] where the two enums overlap.
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

// The contract these tests hold `rebuild_index` to (slice `S-D21`):
//
// signed shape (`SidecarV1` — what every current write path emits)
//   1. a hidden asset comes back hidden, and stays out of the default projections
//   2. a visible asset comes back visible (the control: hiding is not the default)
//   3. `stack_membership` comes back as `stack_id` + `is_stack_hidden` + stack rows
//   3b. an index-only (importer-formed) placement is preserved, never overwritten with NULL
//   4. the trash state and the owning album come back from the provenance chain
//   5. a missing chain degrades to "album unknown, not trashed" rather than failing
//   6. rebuilding twice changes nothing
//   7. the `{uuid}.provenance.cbor` / `{uuid}.receipts.cbor` siblings are not sidecars
//   8. `cull` needs no rebuild support — it is not an index projection (audit finding)
//
// unsigned shape (`AssetSidecar` — the pre-signed-path compatibility read)
//   9-11. the pre-existing standalone / stacked / idempotent cases still pass
//   12. a library holding both shapes rebuilds both
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    use crate::crypto::hash::Hash32;
    use crate::crypto::primitives::{Argon2Params, CRYPTO_SUITE_ID};
    use crate::domain::{DetectionMethod, ImportMode, MemberRole, StackType};
    use crate::library::init::init_library;
    use crate::library::open::open_library;
    use crate::lifecycle::Workspace;
    use crate::metadata::AssetType;
    use crate::metadata::crdt::{Lww, OrSet};
    use crate::sidecar::io::write_sidecar;
    use crate::sidecar::sidecar_v1::{CullFlag, Dimensions};
    use crate::sidecar::{AssetSidecar, StackHint};

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

    // ── the unsigned, pre-signed-path shape ─────────────────────────────────

    fn make_sidecar(uuid: &str, hash: &str, hint: Option<StackHint>) -> AssetSidecar {
        AssetSidecar {
            version: 1,
            uuid: uuid.to_string(),
            asset_type: AssetType::Photo,
            original_filename: format!("{uuid}.jpg"),
            import_timestamp: 1720000000,
            modified_timestamp: 1720000000,
            hash_sha256: hash.to_string(),
            file_size: 1024,
            is_deleted: false,
            rating: 0,
            tags: vec![],
            import_mode: ImportMode::Copy,
            importer_version: "0.1.0".to_string(),
            rawshift_version: "0.1.0".to_string(),
            capture_timestamp: None,
            capture_utc: None,
            capture_tz: None,
            capture_tz_source: None,
            tz_db_version: None,
            width: None,
            height: None,
            duration_ms: None,
            stack_hint: hint,
            album_id: None,
            deleted_at: None,
            camera_make: None,
            camera_model: None,
            gps_lat: None,
            gps_lon: None,
            unknown_fields: BTreeMap::new(),
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
            cull: Lww::new(),
            hidden: Lww::new(),
            camera_id: None,
            device_id: Uuid::from_u128(0xD1),
            session_id: Uuid::from_u128(0x5E),
            gps: None,
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

    /// The two shapes are disjoint on the wire, so probing signed-then-unsigned cannot
    /// mis-route a file: a signed sidecar has integer field 0 and no `version` key, an
    /// unsigned one has `version` and no field 0. This is also *why* the pre-`S-D21` rebuild
    /// lost the register state silently — it did not mis-read signed sidecars, it skipped
    /// every one of them, so a signed library rebuilt to nothing at all.
    #[test]
    fn the_two_sidecar_shapes_do_not_decode_as_each_other() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        let signed_path = dir.join("signed.cbor");
        fs::write(
            &signed_path,
            signed_sidecar(Uuid::from_u128(0xD15), 0x66).to_canonical_vec(),
        )
        .unwrap();
        assert!(
            read_sidecar(&signed_path).is_err(),
            "the pre-signed-path reader must reject a signed sidecar"
        );

        let legacy_path = dir.join("legacy.cbor");
        write_sidecar(
            &legacy_path,
            &make_sidecar(
                "eeee0000-0000-0000-0000-000000000005",
                &"e".repeat(64),
                None,
            ),
        )
        .unwrap();
        let bytes = fs::read(&legacy_path).unwrap();
        assert!(
            SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).is_err(),
            "the signed reader must reject a pre-signed-path sidecar"
        );
    }

    /// **The `S-D21` acceptance case.** A hidden asset survives a rebuild still hidden — and
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

        // ...and the projections behave as `S-D19` requires.
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

    /// The pre-`S-B15` compatibility case. A stack formed by an importer *of that vintage* was
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
    /// asset on disk is one row, not three.
    #[test]
    fn signed_rebuild_ignores_provenance_and_receipt_siblings() {
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

        rebuild_index(&lib).unwrap();

        assert_eq!(lib.db.query_timeline(0, 100).unwrap().len(), 1);
    }

    /// An asset whose chain is missing is still worth indexing — but it is indexed as
    /// **not** trashed, which is the permissive answer, so the `warn` in `chain_facts` is the
    /// only record that the state was unknown rather than known-false.
    #[test]
    fn signed_rebuild_without_a_chain_indexes_the_asset_as_not_trashed() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("lib");
        let lib = init_library(&root, "T").unwrap();

        let id = Uuid::from_u128(0xC0FFEE);
        write_signed(&root.join(FIXTURE_MEDIA), &signed_sidecar(id, 0x33));

        rebuild_index(&lib).unwrap();

        let row = lib.db.find_by_uuid(&id.to_string()).unwrap().unwrap();
        assert!(!row.is_deleted);
        assert_eq!(row.album_id, None, "no chain, no album to claim");
    }

    /// Overwrite an asset's signed sidecar with `mutate` applied.
    ///
    /// This is how a test produces a hidden asset today: `hidden` has an index projection and
    /// a gated view (`S-D19`) but no `Workspace` setter yet, so the register can only be
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

    /// **The `S-D21` acceptance case on a real signed library**, rather than on a hand-built
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
                Workspace::create_with_params(lib_dir.path(), b"passphrase", fast_params())
                    .unwrap();
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

    /// **The trash half of the `S-D21` audit, end to end on a real signed library.** The
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
                Workspace::create_with_params(lib_dir.path(), b"passphrase", fast_params())
                    .unwrap();
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
                Workspace::create_with_params(lib_dir.path(), b"passphrase", fast_params())
                    .unwrap();
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

    // ── the pre-signed-path compatibility read ──────────────────────────────

    #[test]
    fn test_rebuild_standalone_asset() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("lib");
        let lib = init_library(&root, "T").unwrap();

        // Manually write a sidecar
        let media_dir = root.join(FIXTURE_MEDIA);
        std::fs::create_dir_all(&media_dir).unwrap();
        let sidecar = make_sidecar(
            "aabbccdd-0000-0000-0000-000000000001",
            &"a".repeat(64),
            None,
        );
        write_sidecar(
            &media_dir.join("aabbccdd00000000000000000000001.cbor"),
            &sidecar,
        )
        .unwrap();

        rebuild_index(&lib).unwrap();

        let found = lib.db.find_by_hash(&"a".repeat(64)).unwrap();
        assert!(found.is_some(), "asset should be in DB after rebuild");
    }

    #[test]
    fn test_rebuild_stacked_assets() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("lib");
        let lib = init_library(&root, "T").unwrap();

        let media_dir = root.join(FIXTURE_MEDIA);
        std::fs::create_dir_all(&media_dir).unwrap();

        let primary_hint = StackHint {
            detection_key: "img_0042".to_string(),
            detection_method: DetectionMethod::FilenameStem,
            member_role: MemberRole::Primary,
            stack_type: StackType::RawJpeg,
        };
        let raw_hint = StackHint {
            detection_key: "img_0042".to_string(),
            detection_method: DetectionMethod::FilenameStem,
            member_role: MemberRole::Raw,
            stack_type: StackType::RawJpeg,
        };

        let primary = make_sidecar(
            "aaaa0000-0000-0000-0000-000000000001",
            &"a".repeat(64),
            Some(primary_hint),
        );
        let raw = make_sidecar(
            "bbbb0000-0000-0000-0000-000000000002",
            &"b".repeat(64),
            Some(raw_hint),
        );

        write_sidecar(
            &media_dir.join("aaaa000000000000000000000000001.cbor"),
            &primary,
        )
        .unwrap();
        write_sidecar(
            &media_dir.join("bbbb000000000000000000000000002.cbor"),
            &raw,
        )
        .unwrap();

        rebuild_index(&lib).unwrap();

        // Both assets should be in the DB
        assert!(lib.db.find_by_hash(&"a".repeat(64)).unwrap().is_some());
        assert!(lib.db.find_by_hash(&"b".repeat(64)).unwrap().is_some());

        // Primary should be visible, raw hidden
        let timeline = lib.db.query_timeline(0, 100).unwrap();
        assert_eq!(
            timeline.len(),
            1,
            "only primary should be visible in timeline"
        );
    }

    #[test]
    fn test_rebuild_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("lib");
        let lib = init_library(&root, "T").unwrap();

        let media_dir = root.join(FIXTURE_MEDIA);
        std::fs::create_dir_all(&media_dir).unwrap();
        let sidecar = make_sidecar(
            "cccc0000-0000-0000-0000-000000000003",
            &"c".repeat(64),
            None,
        );
        write_sidecar(
            &media_dir.join("cccc000000000000000000000000003.cbor"),
            &sidecar,
        )
        .unwrap();

        rebuild_index(&lib).unwrap();
        rebuild_index(&lib).unwrap(); // second call should not fail

        let found = lib.db.find_by_hash(&"c".repeat(64)).unwrap();
        assert!(found.is_some());
    }

    /// The two shapes coexist: a library part-written before the signed path must rebuild
    /// both, with the signed asset keeping its register state and the unsigned one keeping
    /// what its shape can carry.
    #[test]
    fn mixed_library_rebuilds_both_sidecar_shapes() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("lib");
        let lib = init_library(&root, "T").unwrap();
        let media_dir = root.join(FIXTURE_MEDIA);
        std::fs::create_dir_all(&media_dir).unwrap();

        let legacy = make_sidecar(
            "dddd0000-0000-0000-0000-000000000004",
            &"d".repeat(64),
            None,
        );
        write_sidecar(
            &media_dir.join("dddd000000000000000000000000004.cbor"),
            &legacy,
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
            lib.db.find_by_hash(&"d".repeat(64)).unwrap().is_some(),
            "the pre-signed-path sidecar still rebuilds"
        );
        let signed_row = lib
            .db
            .find_by_uuid(&signed_id.to_string())
            .unwrap()
            .unwrap();
        assert!(signed_row.is_hidden);
        let timeline = lib.db.query_timeline(0, 100).unwrap();
        assert_eq!(timeline.len(), 1, "only the unsigned, visible asset shows");
    }
}
