use super::*;
use crate::metadata::crdt::AddId;

const DEV_A: Uuid = Uuid::from_u128(0xA);
const DEV_B: Uuid = Uuid::from_u128(0xB);

fn rect() -> RegionShape {
    RegionShape::Rect {
        x: 0.25,
        y: 0.25,
        w: 0.5,
        h: 0.5,
    }
}

fn labelled(shape: RegionShape, labels: &[(&str, AddId)]) -> Region {
    let mut region = Region::new(shape);
    for (label, id) in labels {
        region.labels.add((*label).to_string(), *id);
    }
    region
}

fn register(value: Option<Region>, ts: &str, by: Uuid) -> Lww<Option<Region>> {
    let mut reg = Lww::new();
    write_region(&mut reg, value, ts.to_string(), by);
    reg
}

fn add(device: Uuid, counter: u64) -> AddId {
    AddId { device, counter }
}

#[test]
fn shape_validation_enforces_the_normalized_contract() {
    assert_eq!(rect().validate(), Ok(()));
    assert_eq!(
        RegionShape::Circle {
            cx: 0.5,
            cy: 0.5,
            r: 0.1
        }
        .validate(),
        Ok(())
    );
    assert_eq!(
        RegionShape::Circle {
            cx: 1.5,
            cy: 0.5,
            r: 0.1
        }
        .validate(),
        Err(RegionError::OutOfRange)
    );
    assert_eq!(
        RegionShape::Circle {
            cx: f32::NAN,
            cy: 0.5,
            r: 0.1
        }
        .validate(),
        Err(RegionError::OutOfRange)
    );
    assert_eq!(
        RegionShape::Rect {
            x: 0.1,
            y: 0.1,
            w: 0.0,
            h: 0.2
        }
        .validate(),
        Err(RegionError::Empty)
    );
    assert_eq!(
        RegionShape::Rect {
            x: 0.8,
            y: 0.1,
            w: 0.3,
            h: 0.2
        }
        .validate(),
        Err(RegionError::PastEdge)
    );
}

#[test]
fn concurrent_label_adds_on_one_region_both_survive() {
    let a = register(
        Some(labelled(rect(), &[("Alice", add(DEV_A, 0))])),
        "2026-05-31T10:00:00Z",
        DEV_A,
    );
    let b = register(
        Some(labelled(rect(), &[("Bob", add(DEV_B, 0))])),
        "2026-05-31T10:00:01Z",
        DEV_B,
    );
    let ab = merge_region_register(&a, &b);
    let ba = merge_region_register(&b, &a);
    assert_eq!(ab, ba);
    let labels = live_region(&ab).unwrap().labels.value();
    assert!(labels.contains("Alice") && labels.contains("Bob"));
}

#[test]
fn concurrent_add_and_remove_converge() {
    // Both devices observed Alice's add; A removes it while B adds Bob.
    let base = labelled(rect(), &[("Alice", add(DEV_A, 0))]);
    let mut removed = base.clone();
    removed.labels.remove(add(DEV_A, 0)).unwrap();
    let mut added = base;
    added.labels.add("Bob".into(), add(DEV_B, 0));

    let a = register(Some(removed), "2026-05-31T10:00:00Z", DEV_A);
    let b = register(Some(added), "2026-05-31T09:00:00Z", DEV_B);
    let ab = merge_region_register(&a, &b);
    assert_eq!(ab, merge_region_register(&b, &a));
    let labels = live_region(&ab).unwrap().labels.value();
    assert_eq!(
        labels.into_iter().collect::<Vec<_>>(),
        vec!["Bob".to_string()]
    );
}

#[test]
fn a_delete_absorbs_an_edit_stamped_after_it() {
    let edit = register(
        Some(labelled(rect(), &[("Alice", add(DEV_A, 0))])),
        "2026-05-31T12:00:00Z",
        DEV_A,
    );
    let delete = register(None, "2026-05-31T11:00:00Z", DEV_B);
    let merged = merge_region_register(&edit, &delete);
    assert_eq!(merged, merge_region_register(&delete, &edit));
    assert!(live_region(&merged).is_none());
    assert_eq!(merged.get(), Some(&None));
}

#[test]
fn the_later_shape_wins_between_live_writes() {
    let moved = RegionShape::Circle {
        cx: 0.1,
        cy: 0.1,
        r: 0.05,
    };
    let a = register(Some(Region::new(rect())), "2026-05-31T10:00:00Z", DEV_A);
    let b = register(Some(Region::new(moved)), "2026-05-31T10:05:00Z", DEV_B);
    assert_eq!(
        live_region(&merge_region_register(&a, &b)).unwrap().shape,
        moved
    );
    assert_eq!(
        live_region(&merge_region_register(&b, &a)).unwrap().shape,
        moved
    );
}

#[test]
fn map_merge_unions_region_ids() {
    let r1 = Uuid::from_u128(1);
    let r2 = Uuid::from_u128(2);
    let mut local: Regions = BTreeMap::new();
    local.insert(
        r1,
        register(Some(Region::new(rect())), "2026-05-31T10:00:00Z", DEV_A),
    );
    let mut remote: Regions = BTreeMap::new();
    remote.insert(
        r2,
        register(Some(Region::new(rect())), "2026-05-31T10:00:00Z", DEV_B),
    );
    merge_regions(&mut local, &remote);
    assert_eq!(local.keys().copied().collect::<Vec<_>>(), vec![r1, r2]);
}
