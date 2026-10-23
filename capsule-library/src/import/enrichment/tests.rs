use std::path::PathBuf;

use capsule_core::domain::{MemberRole, StackType};
use capsule_core::import::scan::ImportCandidate;
use capsule_core::metadata::AssetType;
use jiff::Timestamp;

use super::*;
use crate::import::importers::{FoldSource, GeoPoint, SourceEntry};

fn folded() -> ExtractedMetadata {
    ExtractedMetadata::default()
}

#[test]
fn no_exporter_record_yields_no_enrichment() {
    assert_eq!(sidecar_enrichment(&folded()), None);
}

#[test]
fn taken_time_and_gps_carry_across_as_fallbacks() {
    let m = ExtractedMetadata {
        taken_time: Timestamp::from_second(1_622_505_600).ok(),
        taken_time_source: FoldSource::Exporter,
        gps: Some(GeoPoint {
            lat: 21.3,
            lon: -157.8,
        }),
        gps_source: FoldSource::Exporter,
        ..folded()
    };
    let e = sidecar_enrichment(&m).expect("a record with values enriches");
    assert_eq!(
        e.capture_time.map(Timestamp::as_second),
        Some(1_622_505_600)
    );
    let gps = e.gps.expect("gps carried");
    assert_eq!((gps.lat, gps.lon), (21.3, -157.8));
    assert_eq!(gps.source, GpsSource::Manual);
    assert_eq!(gps.datum, GpsDatum::Wgs84);
}

#[test]
fn a_favorite_maps_to_the_max_star_rating() {
    let m = ExtractedMetadata {
        favorite: true,
        ..folded()
    };
    assert_eq!(
        sidecar_enrichment(&m).and_then(|e| e.rating),
        Some(FAVORITE_RATING)
    );
}

#[test]
fn a_non_favorite_leaves_the_rating_unwritten() {
    let m = ExtractedMetadata {
        description: Some("has a caption but is not a favorite".into()),
        ..folded()
    };
    let e = sidecar_enrichment(&m).expect("the description enriches");
    assert_eq!(e.rating, None, "an unstarred photo writes no rating at all");
}

#[test]
fn a_description_maps_to_the_caption() {
    let m = ExtractedMetadata {
        description: Some("On the beach".into()),
        ..folded()
    };
    assert_eq!(
        sidecar_enrichment(&m).and_then(|e| e.caption),
        Some("On the beach".to_string())
    );
}

#[test]
fn an_oversize_description_is_bounded_on_a_char_boundary() {
    // Multi-byte characters straddling the bound: the cut must stay valid UTF-8.
    let m = ExtractedMetadata {
        description: Some("é".repeat(CAPTION_MAX_BYTES)),
        ..folded()
    };
    let caption = sidecar_enrichment(&m)
        .and_then(|e| e.caption)
        .expect("the description enriches");
    assert!(caption.len() <= CAPTION_MAX_BYTES);
    assert!(caption.len() > CAPTION_MAX_BYTES - 4, "cut at the bound");
    assert!(caption.chars().all(|c| c == 'é'), "no split character");
}

#[test]
fn album_membership_maps_to_user_tags() {
    let m = ExtractedMetadata {
        albums: vec!["Alps".into(), "Vacation 2021".into()],
        ..folded()
    };
    assert_eq!(
        sidecar_enrichment(&m).map(|e| e.tags),
        Some(vec!["Alps".to_string(), "Vacation 2021".to_string()])
    );
}

fn stacked_entry(primary: &str, alternate: &str, metadata: ExtractedMetadata) -> SourceEntry {
    SourceEntry {
        candidate: ImportCandidate {
            source_paths: vec![PathBuf::from(primary), PathBuf::from(alternate)],
            detected_type: AssetType::Photo,
            stack_type: Some(StackType::Custom),
            detection_method: None,
            detection_key: None,
            members: vec![
                (PathBuf::from(primary), MemberRole::Primary),
                (PathBuf::from(alternate), MemberRole::Alternate),
            ],
        },
        metadata,
    }
}

#[test]
fn the_index_resolves_every_member_of_a_stacked_entry() {
    let m = ExtractedMetadata {
        description: Some("Edited photo".into()),
        ..folded()
    };
    let extracted = ExtractedImport {
        entries: vec![stacked_entry(
            "/x/edit.jpg",
            "/x/edit-edited.jpg",
            m.clone(),
        )],
    };
    let index = SourceMetadataIndex::from_extracted(&extracted);
    assert_eq!(index.len(), 2);
    // Google's edited rendition is its own signed asset, and the exporter's record
    // describes the photograph — so both members resolve to it.
    assert_eq!(index.get(Path::new("/x/edit.jpg")), Some(&m));
    assert_eq!(index.get(Path::new("/x/edit-edited.jpg")), Some(&m));
}

#[test]
fn the_index_misses_an_unknown_path() {
    let index = SourceMetadataIndex::from_extracted(&ExtractedImport::default());
    assert!(index.is_empty());
    assert_eq!(index.get(Path::new("/x/never-seen.jpg")), None);
}
