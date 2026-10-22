use std::path::PathBuf;

use uuid::Uuid;

use super::*;

fn imported(
    name: &str,
    derivatives: DerivativeStatus,
    deferred_formats: u32,
) -> (PathBuf, ImportOutcome) {
    (
        PathBuf::from(name),
        ImportOutcome::Imported {
            asset_id: Uuid::from_u128(name.len() as u128),
            derivatives,
            deferred_formats,
        },
    )
}

fn summary() -> ImportExecutionSummary {
    ImportExecutionSummary {
        outcomes: vec![
            imported("/src/photo.jpg", DerivativeStatus::Decoded, 2),
            imported("/src/iphone.heic", DerivativeStatus::DeferredNoCodec, 0),
            imported("/src/clip.mp4", DerivativeStatus::DeferredNoCodec, 0),
            imported("/src/broken.jpg", DerivativeStatus::DecodeFailed, 0),
            imported("/src/scan.pdf", DerivativeStatus::NotAKnownStill, 0),
            (
                PathBuf::from("/src/again.jpg"),
                ImportOutcome::DuplicateSkipped {
                    existing_uuid: Uuid::from_u128(7).to_string(),
                },
            ),
            (
                PathBuf::from("/src/locked"),
                ImportOutcome::PermissionDenied("permission denied".to_owned()),
            ),
        ],
    }
}

fn counts() -> PlanCounts {
    PlanCounts {
        to_import: 5,
        duplicates: 1,
        errors: 0,
        total_size: 1234,
    }
}

#[test]
fn the_report_counts_every_reason_an_original_has_no_derivatives() {
    let report = ImportReport::new(7, 7, &counts(), &summary());

    assert_eq!(report.imported, 5);
    assert_eq!(report.duplicates, 1);
    assert_eq!(report.errors, 1);
    assert_eq!(report.derivatives_deferred, 2, "the HEIC and the video");
    assert_eq!(report.derivatives_decode_failed, 1);
    assert_eq!(report.formats_deferred, 2);
    assert_eq!(report.planned, 5);
    assert_eq!(report.planned_bytes, 1234);
    assert_eq!(report.outcomes.len(), 7);

    // Sorted by path, whatever order the run produced them in.
    let lines: Vec<(&str, FileImportStatus, Option<DerivativeReport>)> = report
        .outcomes
        .iter()
        .map(|o| (o.path.as_str(), o.status, o.derivatives))
        .collect();
    assert_eq!(
        lines,
        [
            ("/src/again.jpg", FileImportStatus::Duplicate, None),
            (
                "/src/broken.jpg",
                FileImportStatus::Imported,
                Some(DerivativeReport::DecodeFailed)
            ),
            (
                "/src/clip.mp4",
                FileImportStatus::Imported,
                Some(DerivativeReport::Deferred)
            ),
            (
                "/src/iphone.heic",
                FileImportStatus::Imported,
                Some(DerivativeReport::Deferred)
            ),
            ("/src/locked", FileImportStatus::Failed, None),
            (
                "/src/photo.jpg",
                FileImportStatus::Imported,
                Some(DerivativeReport::Generated)
            ),
            (
                "/src/scan.pdf",
                FileImportStatus::Imported,
                Some(DerivativeReport::NotApplicable)
            ),
        ]
    );
    assert_eq!(
        report.outcomes[0].asset_id.as_deref(),
        Some(Uuid::from_u128(7).to_string().as_str()),
        "a duplicate names the asset that already holds it"
    );
    assert_eq!(
        report.outcomes[4].error.as_deref(),
        Some("permission denied")
    );
    assert_eq!(report.outcomes[5].deferred_formats, 2);
}

#[test]
fn the_json_shape_names_each_status_in_snake_case() {
    let report = ImportReport::new(7, 7, &counts(), &summary());
    let json = serde_json::to_value(&report).expect("the report serializes");

    assert_eq!(json["derivatives_deferred"], 2);
    assert_eq!(json["outcomes"][0]["status"], "duplicate");
    assert_eq!(json["outcomes"][1]["derivatives"], "decode_failed");
    assert_eq!(json["outcomes"][2]["status"], "imported");
    assert_eq!(json["outcomes"][2]["derivatives"], "deferred");
    assert_eq!(json["outcomes"][4]["status"], "failed");
    assert_eq!(json["outcomes"][5]["derivatives"], "generated");
    assert_eq!(json["outcomes"][6]["derivatives"], "not_applicable");
}

#[test]
fn the_summary_line_names_deferred_and_failed_derivatives() {
    let report = ImportReport::new(7, 7, &counts(), &summary());
    assert_eq!(
        summary_line(&report),
        "Done: 5 imported, 1 duplicate(s), 1 error(s). 2 without previews until a build has \
         their codec. 1 in a supported format did not decode; their originals are kept."
    );

    let clean = ImportReport::new(
        1,
        1,
        &counts(),
        &ImportExecutionSummary {
            outcomes: vec![imported("/src/photo.jpg", DerivativeStatus::Decoded, 2)],
        },
    );
    assert_eq!(
        summary_line(&clean),
        "Done: 1 imported, 0 duplicate(s), 0 error(s)."
    );
}
