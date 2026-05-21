use std::collections::HashMap;

type StackGroupKey = (String, String);
type StackGroupMembers = Vec<(String, String, StackType)>;
use walkdir::WalkDir;

use crate::db::rows::{AssetRow, AssetStackRow, StackMemberRow};
use crate::domain::{CaptureTzSource, DetectionMethod, MemberRole, StackType};
use crate::library::error::LibraryError;
use crate::library::library::Library;
use crate::metadata::AssetType;
use crate::sidecar::io::read_sidecar;

/// Rebuild the SQLite index from the CBOR sidecar files on disk.
///
/// For each `*.cbor` file under `media/`, an `assets` row is upserted.
/// Then stacks are reconstructed from `stack_hint` fields, inserting
/// `asset_stacks` and `stack_members` rows.
pub fn rebuild_index(library: &Library) -> Result<(), LibraryError> {
    let media_dir = library.root.join("media");
    if !media_dir.exists() {
        return Ok(());
    }

    let mut sidecars = Vec::new();

    for entry in WalkDir::new(&media_dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !name.ends_with(".cbor") {
            continue;
        }
        match read_sidecar(path) {
            Ok(sidecar) => sidecars.push(sidecar),
            Err(e) => {
                log::warn!(
                    "rebuild_index: skipping unreadable sidecar {}: {e}",
                    path.display()
                );
            }
        }
    }

    // Upsert all asset rows.
    for sidecar in &sidecars {
        let row = asset_row_from_sidecar(sidecar);
        library.db.upsert_asset(&row)?;
    }

    // Reconstruct stacks: group by (detection_key, detection_method) from stack_hint.
    // key: (detection_key, detection_method_str) → Vec<(uuid, member_role_str, StackType)>
    let mut groups: HashMap<StackGroupKey, StackGroupMembers> = HashMap::new();

    for sidecar in &sidecars {
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
        let primary_uuid = members
            .iter()
            .find(|(_, role, _)| role == "primary")
            .map(|(uuid, _, _)| uuid.clone())
            .unwrap_or_else(|| members[0].0.clone());

        let stack_type_str = members
            .first()
            .map(|(_, _, st)| stack_type_str(*st))
            .unwrap_or("custom");

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
    }

    Ok(())
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn asset_row_from_sidecar(s: &crate::sidecar::AssetSidecar) -> AssetRow {
    AssetRow {
        uuid: s.uuid.clone(),
        asset_type: asset_type_str(s.asset_type).to_string(),
        capture_timestamp: s.capture_timestamp.unwrap_or(s.import_timestamp),
        capture_utc: s.capture_utc,
        capture_tz_source: s.capture_tz_source.map(|c| tz_source_str(c).to_string()),
        import_timestamp: s.import_timestamp,
        hash_blake3: s.hash_blake3.clone(),
        width: s.width.map(|w| w as i64),
        height: s.height.map(|h| h as i64),
        duration_ms: s.duration_ms.map(|d| d as i64),
        stack_id: None,
        is_stack_hidden: false,
        chromahash: None,
        dominant_color: None,
        album_id: s.album_id.clone(),
        rating: s.rating as i64,
        is_deleted: s.is_deleted,
        deleted_at: s.deleted_at,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DetectionMethod, ImportMode, MemberRole, StackType};
    use crate::library::init::init_library;
    use crate::metadata::AssetType;
    use crate::sidecar::io::write_sidecar;
    use crate::sidecar::{AssetSidecar, StackHint};
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn make_sidecar(uuid: &str, hash: &str, hint: Option<StackHint>) -> AssetSidecar {
        AssetSidecar {
            version: 1,
            uuid: uuid.to_string(),
            asset_type: AssetType::Photo,
            original_filename: format!("{uuid}.jpg"),
            import_timestamp: 1720000000,
            modified_timestamp: 1720000000,
            hash_blake3: hash.to_string(),
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

    #[test]
    fn test_rebuild_standalone_asset() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("lib");
        let lib = init_library(&root, "T").unwrap();

        // Manually write a sidecar
        let media_dir = root.join("media/1970/1970-01");
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

        let media_dir = root.join("media/1970/1970-01");
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

        let media_dir = root.join("media/1970/1970-01");
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
}
