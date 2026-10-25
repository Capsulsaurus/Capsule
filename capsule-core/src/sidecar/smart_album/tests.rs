use std::collections::BTreeMap;

use uuid::Uuid;

use super::*;
use crate::crypto::hash::Hash32;
use crate::crypto::primitives::CRYPTO_SUITE_ID;
use crate::domain::StackType;
use crate::metadata::crdt::{AddId, Lww};
use crate::sidecar::region::{Region, RegionShape, Regions, write_region};
use crate::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, StackMembership, StackRole};

const DEV: Uuid = Uuid::from_u128(0xD1);
const STACK: Uuid = Uuid::from_u128(0x51);
const ALBUM: Uuid = Uuid::from_u128(0xA1);
const TS: &str = "2026-05-31T12:00:00Z";

fn add(counter: u64) -> AddId {
    AddId {
        device: DEV,
        counter,
    }
}

fn sidecar() -> SidecarV1 {
    SidecarV1 {
        sidecar_schema: SIDECAR_SCHEMA_V1,
        crypto_suite_id: CRYPTO_SUITE_ID,
        uuid: Uuid::from_u128(0x7777),
        hash: Hash32([0xAB; 32]),
        capture_timestamp: "2026-05-31T10:00:00Z".into(),
        capture_timestamp_user: Lww::new(),
        import_timestamp: "2026-05-31T11:00:00Z".into(),
        content_type: "image/jpeg".into(),
        dimensions: None,
        lqip: None,
        tags_user: OrSet::new(),
        tags_ai: OrSet::new(),
        caption: Lww::new(),
        rating: Lww::new(),
        stack_membership: Lww::new(),
        regions: Regions::new(),
        cull: Lww::new(),
        hidden: Lww::new(),
        camera_id: None,
        device_id: DEV,
        session_id: Uuid::from_u128(0x5E),
        gps: None,
        gps_user: Lww::new(),
        provenance_chain_hash: None,
        unknown: BTreeMap::new(),
        signature: None,
    }
}

fn join_stack(s: &mut SidecarV1, stack_id: Uuid) {
    s.stack_membership.set(
        Some(StackMembership {
            stack_id,
            stack_type: StackType::Burst,
            role: StackRole::Member,
            member_index: None,
        }),
        TS,
        DEV,
    );
}

fn with_region(s: &mut SidecarV1, label: &str, deleted: bool) {
    let mut region = Region::new(RegionShape::Rect {
        x: 0.25,
        y: 0.25,
        w: 0.5,
        h: 0.5,
    });
    region.labels.add(label.into(), add(7));
    let mut register = Lww::new();
    write_region(&mut register, Some(region), TS.into(), DEV);
    if deleted {
        write_region(&mut register, None, "2026-05-31T13:00:00Z".into(), DEV);
    }
    s.regions.insert(Uuid::from_u128(0x7E61), register);
}

fn stack_labelled(label: &str) -> StackDocV1 {
    let mut stack = StackDocV1::new(STACK, ALBUM);
    stack.labels.add(label.into(), add(8));
    stack
}

fn album_labelled(label: &str) -> AlbumDocV1 {
    let mut album = AlbumDocV1::new(ALBUM);
    album.labels.add(label.into(), add(9));
    album
}

fn ctx(sidecar: &SidecarV1) -> LabelContext<'_> {
    LabelContext {
        sidecar,
        stack: None,
        album: None,
    }
}

#[test]
fn matches_an_asset_label() {
    let mut s = sidecar();
    s.tags_user.add("Paris".into(), add(1));
    assert_eq!(ctx(&s).level_of("Paris"), Some(LabelLevel::Asset));
    assert!(ctx(&s).tags_user_contains("Paris"));
}

#[test]
fn matches_a_live_region_label() {
    let mut s = sidecar();
    with_region(&mut s, "Alice", false);
    assert_eq!(ctx(&s).level_of("Alice"), Some(LabelLevel::Region));
}

#[test]
fn never_matches_a_deleted_region_label() {
    let mut s = sidecar();
    with_region(&mut s, "Alice", true);
    assert!(!ctx(&s).tags_user_contains("Alice"));
}

#[test]
fn matches_the_stack_label_of_a_current_member() {
    let mut s = sidecar();
    join_stack(&mut s, STACK);
    let stack = stack_labelled("Sunset");
    let c = LabelContext {
        stack: Some(&stack),
        ..ctx(&s)
    };
    assert_eq!(c.level_of("Sunset"), Some(LabelLevel::Stack));
}

#[test]
fn ignores_a_stack_the_asset_does_not_belong_to() {
    let stack = stack_labelled("Sunset");

    let never_joined = sidecar();
    let c = LabelContext {
        stack: Some(&stack),
        ..ctx(&never_joined)
    };
    assert!(!c.tags_user_contains("Sunset"));

    let mut other = sidecar();
    join_stack(&mut other, Uuid::from_u128(0x52));
    let c = LabelContext {
        stack: Some(&stack),
        ..ctx(&other)
    };
    assert!(!c.tags_user_contains("Sunset"));

    let mut left = sidecar();
    join_stack(&mut left, STACK);
    left.stack_membership.set(None, "2026-05-31T13:00:00Z", DEV);
    let c = LabelContext {
        stack: Some(&stack),
        ..ctx(&left)
    };
    assert!(!c.tags_user_contains("Sunset"));
}

#[test]
fn matches_an_album_label() {
    let s = sidecar();
    let album = album_labelled("Holiday");
    let c = LabelContext {
        album: Some(&album),
        ..ctx(&s)
    };
    assert_eq!(c.level_of("Holiday"), Some(LabelLevel::Album));
}

#[test]
fn a_removed_label_does_not_match() {
    let mut s = sidecar();
    s.tags_user.add("Paris".into(), add(1));
    s.tags_user.remove(add(1)).unwrap();
    assert!(!ctx(&s).tags_user_contains("Paris"));
}

#[test]
fn matching_is_exact_and_case_sensitive() {
    let mut s = sidecar();
    s.tags_user.add("Paris".into(), add(1));
    assert!(!ctx(&s).tags_user_contains("paris"));
    assert!(!ctx(&s).tags_user_contains("Par"));
}

#[test]
fn ai_tags_never_match_a_user_label_term() {
    use crate::sidecar::sidecar_v1::AiTag;
    let mut s = sidecar();
    s.tags_ai.add(
        AiTag {
            tag: "Paris".into(),
            model_id: "m".into(),
            model_version: "1".into(),
        },
        add(2),
    );
    assert!(!ctx(&s).tags_user_contains("Paris"));
}

#[test]
fn reports_the_first_level_when_several_carry_the_label() {
    let mut s = sidecar();
    s.tags_user.add("Paris".into(), add(1));
    let album = album_labelled("Paris");
    let c = LabelContext {
        album: Some(&album),
        ..ctx(&s)
    };
    assert_eq!(c.level_of("Paris"), Some(LabelLevel::Asset));
    assert_eq!(ctx(&sidecar()).level_of("Paris"), None);
}
