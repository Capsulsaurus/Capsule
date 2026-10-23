//! Edit tests: each edit through a real fast-cost workspace, the report it returns, and the
//! parsers the CLI and the TUI forms share.

use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::domain::StackType;
use capsule_core::sidecar::{StackMembership, StackRole};

use super::*;

const FAST_KDF: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// A fast-cost workspace with `count` imported assets, in a scratch directory removed on drop.
struct Fixture {
    dir: std::path::PathBuf,
    ws: Workspace,
    album: Uuid,
    ids: Vec<Uuid>,
}

impl Fixture {
    fn with_assets(count: usize) -> Self {
        let dir = std::env::temp_dir().join(format!("capsule-cli-edit-{}", nanoid::nanoid!()));
        let lib = dir.join("lib");
        std::fs::create_dir_all(&lib).expect("scratch library dir");
        let mut ws =
            Workspace::create_with_params(&lib, b"pw", FAST_KDF).expect("create workspace");
        let album = ws.default_album_id();
        ws.create_album_with_id(album, "Imports")
            .expect("create album");
        let ids = (0..count)
            .map(|n| {
                let src = dir.join(format!("photo-{n}.jpg"));
                let mut bytes = b"\xFF\xD8\xFF".to_vec();
                bytes.extend_from_slice(format!(" edit asset {n}").as_bytes());
                std::fs::write(&src, bytes).expect("fixture file");
                ws.import_asset(album, &src).expect("import")
            })
            .collect();
        Self {
            dir,
            ws,
            album,
            ids,
        }
    }

    fn records(&self, asset_id: Uuid) -> usize {
        self.ws
            .asset(&asset_id)
            .expect("asset")
            .chain
            .records()
            .len()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn add(target: LabelTarget, label: &str) -> Edit {
    Edit::AddLabel {
        target,
        label: label.to_owned(),
    }
}

fn remove(target: LabelTarget, label: &str) -> Edit {
    Edit::RemoveLabel {
        target,
        label: label.to_owned(),
    }
}

#[test]
fn an_asset_label_is_added_and_removed_as_signed_records() {
    let mut fx = Fixture::with_assets(1);
    let asset = fx.ids[0];
    let before = fx.records(asset);

    let added = apply(&mut fx.ws, &add(LabelTarget::Asset(asset), " beach ")).expect("add");
    assert_eq!(added.edit, "label-add");
    assert_eq!(added.asset_id, Some(asset));
    assert!(added.changed);
    assert_eq!(added.labels, Some(vec!["beach".to_owned()]));
    assert_eq!(fx.records(asset), before + 1);

    let removed = apply(&mut fx.ws, &remove(LabelTarget::Asset(asset), "beach")).expect("remove");
    assert!(removed.changed);
    assert_eq!(removed.labels, Some(Vec::new()));
    assert_eq!(fx.records(asset), before + 2);

    let again = apply(&mut fx.ws, &remove(LabelTarget::Asset(asset), "beach")).expect("remove");
    assert!(!again.changed, "nothing left to remove");
    assert_eq!(fx.records(asset), before + 2, "no record for a no-op");
}

#[test]
fn stack_and_album_labels_land_in_their_records() {
    let mut fx = Fixture::with_assets(1);
    let stack = Uuid::now_v7();
    fx.ws
        .set_stack_membership(
            &fx.ids[0],
            Some(StackMembership {
                stack_id: stack,
                stack_type: StackType::Burst,
                role: StackRole::Primary,
                member_index: None,
            }),
        )
        .expect("stack");

    let report = apply(&mut fx.ws, &add(LabelTarget::Stack(stack), "burst")).expect("stack add");
    assert_eq!(report.stack_id, Some(stack));
    assert_eq!(report.labels, Some(vec!["burst".to_owned()]));
    assert_eq!(
        labels(&fx.ws, LabelTarget::Stack(stack)).expect("stack labels"),
        vec!["burst".to_owned()]
    );

    let album = LabelTarget::Album(fx.album);
    apply(&mut fx.ws, &add(album, "trip")).expect("album add");
    apply(&mut fx.ws, &add(album, "family")).expect("album add");
    let report = apply(&mut fx.ws, &remove(album, "trip")).expect("album remove");
    assert_eq!(report.album_id, Some(fx.album));
    assert_eq!(report.labels, Some(vec!["family".to_owned()]));
}

#[test]
fn a_region_is_drawn_labelled_and_deleted() {
    let mut fx = Fixture::with_assets(1);
    let asset = fx.ids[0];
    let report = apply(
        &mut fx.ws,
        &Edit::AddRegion {
            asset_id: asset,
            shape: RegionShape::Circle {
                cx: 0.5,
                cy: 0.5,
                r: 0.25,
            },
            labels: vec!["Ada".to_owned()],
        },
    )
    .expect("add region");
    let region_id = report.region_id.expect("the new region's id");
    assert_eq!(report.asset_id, Some(asset));
    assert_eq!(report.labels, Some(vec!["Ada".to_owned()]));

    let region = LabelTarget::Region {
        asset_id: asset,
        region_id,
    };
    let report = apply(&mut fx.ws, &add(region, "face")).expect("region label");
    assert_eq!(
        report.labels,
        Some(vec!["Ada".to_owned(), "face".to_owned()])
    );
    let report = apply(&mut fx.ws, &remove(region, "Ada")).expect("region unlabel");
    assert_eq!(report.labels, Some(vec!["face".to_owned()]));

    let report = apply(
        &mut fx.ws,
        &Edit::DeleteRegion {
            asset_id: asset,
            region_id,
        },
    )
    .expect("delete region");
    assert_eq!(report.region_id, Some(region_id));
    assert!(fx.ws.regions(&asset).is_empty());
    assert!(matches!(
        labels(&fx.ws, region),
        Err(EditError::Lifecycle(LifecycleError::NotFound(_)))
    ));
}

#[test]
fn a_region_with_a_refused_label_is_not_written() {
    let mut fx = Fixture::with_assets(1);
    let asset = fx.ids[0];
    let before = fx.records(asset);
    let refused = apply(
        &mut fx.ws,
        &Edit::AddRegion {
            asset_id: asset,
            shape: RegionShape::Rect {
                x: 0.1,
                y: 0.1,
                w: 0.2,
                h: 0.2,
            },
            labels: vec!["ok".to_owned(), "   ".to_owned()],
        },
    );
    assert!(matches!(
        refused,
        Err(EditError::Lifecycle(LifecycleError::InvalidLabel(_)))
    ));
    assert!(fx.ws.regions(&asset).is_empty());
    assert_eq!(fx.records(asset), before);

    let past_edge = apply(
        &mut fx.ws,
        &Edit::AddRegion {
            asset_id: asset,
            shape: RegionShape::Rect {
                x: 0.9,
                y: 0.1,
                w: 0.2,
                h: 0.2,
            },
            labels: Vec::new(),
        },
    );
    let error = past_edge.expect_err("a rectangle past the edge is refused");
    assert!(describe_error(&error).contains("stays inside the image"));
}

#[test]
fn a_capture_time_correction_keeps_the_imported_value() {
    let mut fx = Fixture::with_assets(1);
    let asset = fx.ids[0];
    let imported = crate::show::collect(&fx.ws, &asset)
        .expect("view")
        .capture_timestamp;
    let capture: Timestamp = "2021-07-04T18:30:00Z".parse().expect("instant");
    let report = apply(
        &mut fx.ws,
        &Edit::SetCaptureTime {
            asset_id: asset,
            capture,
        },
    )
    .expect("capture time");
    assert_eq!(report.edit, "capture-time");
    assert_eq!(
        report.capture_timestamp.as_deref(),
        Some("2021-07-04T18:30:00Z")
    );
    assert_eq!(report.capture_timestamp_imported, Some(imported));
    assert!(summary(&report).starts_with("Capture time now 2021-07-04T18:30:00Z"));
}

#[test]
fn a_location_is_set_then_cleared_beside_the_imported_one() {
    let mut fx = Fixture::with_assets(1);
    let asset = fx.ids[0];
    let report = apply(
        &mut fx.ws,
        &Edit::SetLocation {
            asset_id: asset,
            lat: 48.8584,
            lon: 2.2945,
            datum: GpsDatum::Wgs84,
        },
    )
    .expect("set location");
    let fix = report.location.clone().flatten().expect("an effective fix");
    assert_eq!((fix.lat, fix.lon), (48.8584, 2.2945));
    assert_eq!((fix.datum, fix.source), ("WGS-84", "manual"));
    assert_eq!(report.location_imported, Some(None), "the fixture had none");

    let report = apply(&mut fx.ws, &Edit::ClearLocation { asset_id: asset }).expect("clear");
    assert_eq!(report.edit, "location-clear");
    assert_eq!(report.location, Some(None));
    let json = serde_json::to_value(&report).expect("serializes");
    assert!(json["location"].is_null(), "{json}");
    assert!(json.get("labels").is_none(), "{json}");

    let refused = apply(
        &mut fx.ws,
        &Edit::SetLocation {
            asset_id: asset,
            lat: 91.0,
            lon: 0.0,
            datum: GpsDatum::Wgs84,
        },
    )
    .expect_err("out of range");
    assert!(describe_error(&refused).contains("[-90, 90]"));
}

#[test]
fn an_edit_of_an_unknown_asset_is_refused_with_its_id() {
    let mut fx = Fixture::with_assets(0);
    let ghost = Uuid::now_v7();
    let error = apply(&mut fx.ws, &add(LabelTarget::Asset(ghost), "x")).expect_err("unknown asset");
    assert!(describe_error(&error).contains(&ghost.to_string()));
}

#[test]
fn the_summary_names_each_edit() {
    let base = EditReport::new(&add(LabelTarget::Asset(Uuid::nil()), "x"));
    let labelled = EditReport {
        labels: Some(vec!["a".to_owned(), "b".to_owned()]),
        ..base.clone()
    };
    assert_eq!(summary(&labelled), "Label added. Labels now: a, b.");
    let unchanged = EditReport {
        edit: "label-remove",
        changed: false,
        labels: Some(Vec::new()),
        ..base.clone()
    };
    assert_eq!(
        summary(&unchanged),
        "It did not carry that label. Labels: (none)."
    );
    let located = EditReport {
        edit: "location",
        location: Some(Some(FixReport {
            lat: 1.0,
            lon: 2.0,
            datum: "WGS-84",
            source: "manual",
        })),
        location_imported: Some(None),
        ..base
    };
    assert_eq!(
        summary(&located),
        "Location now 1.000000, 2.000000 (WGS-84, manual); the original's no location is kept."
    );
}

#[test]
fn the_parsers_accept_the_documented_forms_and_refuse_the_rest() {
    assert_eq!(
        parse_circle("0.5, 0.25,0.1"),
        Ok(RegionShape::Circle {
            cx: 0.5,
            cy: 0.25,
            r: 0.1
        })
    );
    assert_eq!(
        parse_rect("0,0,1,1"),
        Ok(RegionShape::Rect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0
        })
    );
    assert!(parse_circle("0.5,0.5").is_err());
    assert!(parse_rect("0,0,1,NaN").is_err());
    assert!(parse_rect("0,0,1,x").is_err());

    assert_eq!(parse_coordinate(" -33.86 "), Ok(-33.86));
    assert!(parse_coordinate("inf").is_err());
    assert!(parse_coordinate("north").is_err());

    assert_eq!(parse_datum("WGS-84"), Ok(GpsDatum::Wgs84));
    assert_eq!(parse_datum("gcj02"), Ok(GpsDatum::Gcj02));
    assert!(parse_datum("nad83").is_err());
}
