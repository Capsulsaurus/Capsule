//! The CRDT merge laws, as properties over generated replicas (SSoT: [Metadata — Grouping
//! Convergence]; testing plan, `capsule-core` row): every label-bearing merge is commutative,
//! associative and idempotent, so concurrent adds and removes from any number of devices
//! converge whatever order they arrive in.
//!
//! Generators keep one invariant real replicas keep: an `add_id` names one element everywhere
//! (a device never issues the same `add_id` twice), and a device never writes two different
//! values under one stamp.
//!
//! [Metadata — Grouping Convergence]: https://docs/design/metadata/#grouping-convergence-requirement

use std::collections::BTreeMap;

use capsule_core::metadata::crdt::{AddId, Lww, OrSet};
use capsule_core::sidecar::{
    AlbumDocV1, Region, RegionShape, Regions, StackDocV1, merge_regions, write_region,
};
use ciborium::value::Value;
use proptest::prelude::*;
use uuid::Uuid;

const LABELS: [&str; 5] = ["Alice", "Bob", "beach", "family", "hdr"];
const ALBUM: Uuid = Uuid::from_u128(0xA1B);
const STACK: Uuid = Uuid::from_u128(0x51);

fn device(i: u8) -> Uuid {
    Uuid::from_u128(0xD0 + u128::from(i))
}

fn add_id(dev: u8, counter: u64) -> AddId {
    AddId {
        device: device(dev),
        counter,
    }
}

/// The one element an `add_id` names, the same on every replica.
fn element(id: AddId) -> String {
    let n = usize::try_from(id.counter).expect("small counter") + id.device.as_u128() as usize;
    LABELS[n % LABELS.len()].to_string()
}

fn stamp(second: u8) -> String {
    format!("2026-05-31T10:00:{second:02}Z")
}

/// A replica's label set: some adds, some of them later removed.
fn or_set() -> impl Strategy<Value = OrSet<String>> {
    proptest::collection::vec((0u8..3, 0u64..6, any::<bool>()), 0..8).prop_map(|ops| {
        let mut set = OrSet::new();
        for (dev, counter, removed) in ops {
            let id = add_id(dev, counter);
            set.add(element(id), id);
            if removed {
                set.remove(id).expect("just added");
            }
        }
        set
    })
}

/// A name register after a few writes; the value is a function of its stamp.
fn name_register() -> impl Strategy<Value = Lww<String>> {
    proptest::collection::vec((0u8..6, 0u8..3), 0..4).prop_map(|writes| {
        let mut reg = Lww::new();
        for (second, dev) in writes {
            reg.set(format!("name-{second}-{dev}"), stamp(second), device(dev));
        }
        reg
    })
}

fn shape(n: u8) -> RegionShape {
    let f = f32::from(n) / 10.0;
    RegionShape::Rect {
        x: f,
        y: f,
        w: 0.25,
        h: 0.25,
    }
}

/// One region register: unwritten, deleted, or live with a labels set.
fn region_register() -> impl Strategy<Value = Lww<Option<Region>>> {
    (0u8..3, 0u8..6, 0u8..3, 0u8..4, or_set()).prop_map(|(kind, second, dev, s, labels)| {
        let mut reg = Lww::new();
        match kind {
            0 => {}
            1 => write_region(&mut reg, None, stamp(second), device(dev)),
            _ => write_region(
                &mut reg,
                Some(Region {
                    shape: shape(s),
                    labels,
                }),
                stamp(second),
                device(dev),
            ),
        }
        reg
    })
}

fn regions() -> impl Strategy<Value = Regions> {
    proptest::collection::btree_map(
        (1u128..4).prop_map(Uuid::from_u128),
        region_register(),
        0..4,
    )
}

fn unknown() -> impl Strategy<Value = BTreeMap<String, Value>> {
    proptest::collection::btree_map(
        prop_oneof![Just("x".to_string()), Just("y".to_string())],
        (0u8..4).prop_map(|n| Value::Integer(n.into())),
        0..2,
    )
}

fn album_doc() -> impl Strategy<Value = AlbumDocV1> {
    (or_set(), name_register(), unknown()).prop_map(|(labels, name, unknown)| {
        let mut doc = AlbumDocV1::new(ALBUM);
        doc.labels = labels;
        doc.name = name;
        doc.unknown = unknown;
        doc
    })
}

fn stack_doc() -> impl Strategy<Value = StackDocV1> {
    (or_set(), unknown()).prop_map(|(labels, unknown)| {
        let mut doc = StackDocV1::new(STACK, ALBUM);
        doc.labels = labels;
        doc.unknown = unknown;
        doc
    })
}

fn or_merge(a: &OrSet<String>, b: &OrSet<String>) -> OrSet<String> {
    let mut out = a.clone();
    out.merge(b);
    out
}

fn regions_merge(a: &Regions, b: &Regions) -> Regions {
    let mut out = a.clone();
    merge_regions(&mut out, b);
    out
}

fn album_merge(a: &AlbumDocV1, b: &AlbumDocV1) -> AlbumDocV1 {
    let mut out = a.clone();
    out.merge(b).expect("same album");
    out
}

fn stack_merge(a: &StackDocV1, b: &StackDocV1) -> StackDocV1 {
    let mut out = a.clone();
    out.merge(b).expect("same stack");
    out
}

/// What an album record converges on: its labels, its name and its preserved unknown keys.
/// (The name register's superseded log is a recovery surface, not converged state.)
fn album_state(doc: &AlbumDocV1) -> (OrSet<String>, Option<String>, BTreeMap<String, Value>) {
    (
        doc.labels.clone(),
        doc.current_name().map(str::to_string),
        doc.unknown.clone(),
    )
}

fn stack_state(doc: &StackDocV1) -> (OrSet<String>, BTreeMap<String, Value>) {
    (doc.labels.clone(), doc.unknown.clone())
}

proptest! {
    #[test]
    fn or_set_merge_is_commutative_associative_idempotent(
        a in or_set(), b in or_set(), c in or_set()
    ) {
        prop_assert_eq!(or_merge(&a, &b), or_merge(&b, &a));
        prop_assert_eq!(or_merge(&or_merge(&a, &b), &c), or_merge(&a, &or_merge(&b, &c)));
        prop_assert_eq!(or_merge(&a, &a), a);
    }

    #[test]
    fn region_merge_is_commutative_associative_idempotent(
        a in regions(), b in regions(), c in regions()
    ) {
        prop_assert_eq!(regions_merge(&a, &b), regions_merge(&b, &a));
        prop_assert_eq!(
            regions_merge(&regions_merge(&a, &b), &c),
            regions_merge(&a, &regions_merge(&b, &c))
        );
        prop_assert_eq!(regions_merge(&a, &a), a);
    }

    #[test]
    fn album_doc_merge_is_commutative_associative_idempotent(
        a in album_doc(), b in album_doc(), c in album_doc()
    ) {
        prop_assert_eq!(album_state(&album_merge(&a, &b)), album_state(&album_merge(&b, &a)));
        prop_assert_eq!(
            album_state(&album_merge(&album_merge(&a, &b), &c)),
            album_state(&album_merge(&a, &album_merge(&b, &c)))
        );
        prop_assert_eq!(album_state(&album_merge(&a, &a)), album_state(&a));
    }

    #[test]
    fn stack_doc_merge_is_commutative_associative_idempotent(
        a in stack_doc(), b in stack_doc(), c in stack_doc()
    ) {
        prop_assert_eq!(stack_state(&stack_merge(&a, &b)), stack_state(&stack_merge(&b, &a)));
        prop_assert_eq!(
            stack_state(&stack_merge(&stack_merge(&a, &b), &c)),
            stack_state(&stack_merge(&a, &stack_merge(&b, &c)))
        );
        prop_assert_eq!(stack_state(&stack_merge(&a, &a)), stack_state(&a));
    }
}
