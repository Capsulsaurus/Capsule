use std::fs;
use std::path::Path;

use capsule_core::crypto::verify_asset::VerifyOutcome;
use tempfile::TempDir;

use super::super::fast_workspace;
use super::*;

// ── Culling workflow ────────────────────────────────────────────────

/// Import three distinct assets into `ws`'s `album` and return their ids in import order.
fn import_trio(ws: &mut Workspace, album: Uuid, src: &Path) -> [Uuid; 3] {
    let mut ids = [Uuid::nil(); 3];
    for (i, id) in ids.iter_mut().enumerate() {
        let p = src.join(format!("cull-{i}.jpg"));
        let mut bytes = vec![0xFF, 0xD8, 0xFF];
        bytes.extend_from_slice(format!("cull fixture asset {i}").as_bytes());
        fs::write(&p, &bytes).unwrap();
        *id = ws.import_asset(album, &p).unwrap();
    }
    ids
}

/// Round-trip: the flag → filter → sweep loop round-trips on a fixture
/// library. Flag pick/reject/neutral, filter the view by each flag, sweep the rejects to
/// trash (soft, retention-carrying, the only destructive step), and restore to prove
/// reversibility.
#[test]
fn cull_flag_filter_sweep_loop_round_trips() {
    use capsule_core::sidecar::sidecar_v1::CullFlag;

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Shoot").unwrap();
    let [pick, reject, neutral] = import_trio(&mut ws, album, src.path());

    // Flag: single write per asset, never touches bytes, and the sidecar stays signed.
    ws.set_cull(&pick, CullFlag::Pick).unwrap();
    ws.set_cull(&reject, CullFlag::Reject).unwrap();
    // `neutral` is left never-flagged (the wire-absent default).
    assert_eq!(ws.cull_flag(&pick), CullFlag::Pick);
    assert_eq!(ws.cull_flag(&reject), CullFlag::Reject);
    assert_eq!(ws.cull_flag(&neutral), CullFlag::Neutral);
    let st = ws.asset(&pick).unwrap();
    assert!(st.sidecar.verify(&ws.account.user_ik.verifying_key()));
    assert_eq!(ws.verify(&pick).unwrap(), VerifyOutcome::Accept);

    // Filter: the cull-filtered views partition the library by flag.
    assert_eq!(ws.assets_by_cull(CullFlag::Pick), vec![pick]);
    assert_eq!(ws.assets_by_cull(CullFlag::Reject), vec![reject]);
    assert!(ws.assets_by_cull(CullFlag::Neutral).contains(&neutral));

    // Sweep: the reject moves to trash (soft), the pick/neutral are untouched.
    let swept = ws.reject_sweep(30).unwrap();
    assert_eq!(swept, vec![reject]);
    // The swept asset dropped out of the (non-trashed) reject view; pick/neutral remain.
    assert!(ws.assets_by_cull(CullFlag::Reject).is_empty());
    assert_eq!(ws.assets_by_cull(CullFlag::Pick), vec![pick]);
    assert!(ws.assets_by_cull(CullFlag::Neutral).contains(&neutral));

    // Sweep is a soft delete carrying a signed retention window (reversible until purge).
    let head = ws.asset(&reject).unwrap().chain.records().last().unwrap();
    assert_eq!(head.manifest.core.action, Action::Delete);
    assert!(
        head.manifest.core.retention_until.is_some(),
        "the sweep's delete must carry a retention window"
    );

    // Reversible: a restore brings the swept asset back into the reject view.
    ws.restore(&reject).unwrap();
    assert_eq!(ws.assets_by_cull(CullFlag::Reject), vec![reject]);

    // A second sweep with nothing flagged reject is a no-op.
    ws.set_cull(&reject, CullFlag::Neutral).unwrap();
    assert!(ws.reject_sweep(30).unwrap().is_empty());
}

/// Convergence: concurrent flags from two devices converge. Driven
/// exactly as the CRDT tests — two register writes with distinct actors/timestamps merged
/// in both orders — and through the engine's sync-apply path.
#[test]
fn concurrent_cull_flags_from_two_devices_converge() {
    use capsule_core::metadata::crdt::Lww;
    use capsule_core::sidecar::sidecar_v1::CullFlag;

    let dev_a = Uuid::from_u128(0xA);
    let dev_b = Uuid::from_u128(0xB);

    // Device A flags Pick; device B flags Reject as the strictly later write (a far-future
    // stamp, so it also outranks the workspace's real-now local flag below).
    let mut reg_a: Lww<CullFlag> = Lww::new();
    reg_a.set(CullFlag::Pick, "2026-05-31T10:00:00Z", dev_a);
    let mut reg_b: Lww<CullFlag> = Lww::new();
    reg_b.set(CullFlag::Reject, "2099-01-01T00:00:00Z", dev_b);

    // Both merge orders converge on the later reject — order-independent by construction.
    let mut ab = reg_a.clone();
    ab.merge(&reg_b);
    let mut ba = reg_b.clone();
    ba.merge(&reg_a);
    assert_eq!(ab.get(), ba.get());
    assert_eq!(ab.get(), Some(&CullFlag::Reject));

    // Through the engine: a workspace that locally flagged Pick, then applies device B's
    // register (the sync-apply path), converges on Reject.
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    fs::write(&img, b"\xFF\xD8\xFF two-device cull").unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Shoot").unwrap();
    let asset = ws.import_asset(album, &img).unwrap();

    ws.set_cull(&asset, CullFlag::Pick).unwrap();
    ws.apply_remote_cull(&asset, &reg_b).unwrap();
    assert_eq!(ws.cull_flag(&asset), CullFlag::Reject);
    // Re-applying the stale Pick register changes nothing (idempotent convergence).
    ws.apply_remote_cull(&asset, &reg_a).unwrap();
    assert_eq!(ws.cull_flag(&asset), CullFlag::Reject);
    // The merged sidecar is still signed and verifies through the chokepoint.
    assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
}

// ── Hidden assets ───────────────────────────────────────────────────

/// A [`LocalAuthGate`] that always grants — the platform adapter's decision is not what is
/// under test here; what the Hidden view *contains* is.
struct AllowGate;
impl crate::library::auth_gate::LocalAuthGate for AllowGate {
    fn authenticate(
        &self,
        _view: crate::library::auth_gate::GatedView,
    ) -> std::result::Result<(), crate::library::auth_gate::LocalAuthError> {
        Ok(())
    }
}

/// **The acceptance case.** Hiding an asset through the public API takes it out of
/// every default projection, puts it in the gated Hidden view, and — because the register
/// is written into the signed sidecar, not just the index — it is *still* hidden after the
/// index is deleted and rebuilt from disk.
///
/// Before this change there was no `set_hidden` at all: the column, the gate and the
/// projections all read a register nothing wrote.
#[test]
fn hidden_written_through_the_api_leaves_default_views_and_survives_rebuild() {
    use crate::library::auth_gate::{GateKeeper, GatedView};
    use crate::library::{open_library, rebuild_index};

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let root = lib.path().to_path_buf();

    let mut ws = fast_workspace(&root);
    let album = ws.create_album("Shoot").unwrap();
    let [secret, visible, _] = import_trio(&mut ws, album, src.path());

    // Both start visible: the wire-absent default.
    assert!(!ws.is_hidden(&secret));
    assert_eq!(ws.db().query_timeline(0, 100).unwrap().len(), 3);

    ws.set_hidden(&secret, true).unwrap();

    // The register is written into the signed sidecar, which still verifies.
    assert!(ws.is_hidden(&secret));
    assert!(!ws.is_hidden(&visible));
    assert_eq!(ws.verify(&secret).unwrap(), VerifyOutcome::Accept);
    assert_eq!(
        ws.asset(&secret)
            .unwrap()
            .chain
            .records()
            .last()
            .unwrap()
            .manifest
            .core
            .action,
        Action::MetadataUpdate
    );

    // Immediately — no rebuild — the default projection drops it and the gated Hidden view
    // serves it. `append_lifecycle` re-indexes the row as part of the same write.
    let timeline: Vec<String> = ws
        .db()
        .query_timeline(0, 100)
        .unwrap()
        .into_iter()
        .map(|r| r.uuid)
        .collect();
    assert_eq!(timeline.len(), 2);
    assert!(!timeline.contains(&secret.to_string()));

    let mut gk = GateKeeper::new();
    gk.open(GatedView::Hidden, &AllowGate).unwrap();
    let hidden = gk.query_hidden(ws.db(), 0, 100).unwrap();
    assert_eq!(hidden.len(), 1);
    assert_eq!(hidden[0].uuid, secret.to_string());

    // Recovery: lose the whole index and rebuild it from the artifacts on disk.
    drop(ws);
    fs::remove_file(root.join("index/library.sqlite")).unwrap();
    let library = open_library(&root).unwrap();
    rebuild_index(&library).unwrap();

    let row = library
        .db
        .find_by_uuid(&secret.to_string())
        .unwrap()
        .expect("the hidden asset is back in the index");
    assert!(row.is_hidden, "the sidecar `hidden` register survived");
    let timeline: Vec<String> = library
        .db
        .query_timeline(0, 100)
        .unwrap()
        .into_iter()
        .map(|r| r.uuid)
        .collect();
    assert_eq!(timeline.len(), 2);
    assert!(!timeline.contains(&secret.to_string()));

    let mut gk = GateKeeper::new();
    gk.open(GatedView::Hidden, &AllowGate).unwrap();
    assert_eq!(gk.query_hidden(&library.db, 0, 100).unwrap().len(), 1);
}

/// Hiding is fully reversible and never touches asset bytes: unhiding writes `false` into
/// the same register and the asset returns to the default projections.
#[test]
fn unhiding_returns_the_asset_to_the_default_views() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Shoot").unwrap();
    let [a, _, _] = import_trio(&mut ws, album, src.path());

    let bytes = ws.read_plaintext(&a).unwrap();
    ws.set_hidden(&a, true).unwrap();
    assert_eq!(ws.db().query_hidden(0, 100).unwrap().len(), 1);

    ws.set_hidden(&a, false).unwrap();
    assert!(!ws.is_hidden(&a));
    // An explicit un-hide is a *written* `false`, not the absent default — it must beat a
    // stale remote `true` on merge, which an absent register could not.
    assert_eq!(ws.hidden_register(&a).unwrap().get(), Some(&false));
    assert!(ws.db().query_hidden(0, 100).unwrap().is_empty());
    assert_eq!(ws.db().query_timeline(0, 100).unwrap().len(), 3);
    assert_eq!(ws.read_plaintext(&a).unwrap(), bytes, "bytes untouched");
    assert_eq!(ws.verify(&a).unwrap(), VerifyOutcome::Accept);
}

/// The sync-apply arm: a peer's `hidden` register merges under the same `(ts, device_id)`
/// rule as `cull`, so two devices converge regardless of arrival order.
#[test]
fn remote_hidden_register_merges_and_converges() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Shoot").unwrap();
    let [a, _, _] = import_trio(&mut ws, album, src.path());

    // A peer hid it at 11:00; the local device un-hid it at 10:00. Later write wins.
    let mut early: Lww<bool> = Lww::new();
    early.set(false, "2026-08-29T10:00:00Z", Uuid::from_u128(0xA));
    let mut late: Lww<bool> = Lww::new();
    late.set(true, "2026-08-29T11:00:00Z", Uuid::from_u128(0xB));

    ws.apply_remote_hidden(&a, &early).unwrap();
    ws.apply_remote_hidden(&a, &late).unwrap();
    assert!(ws.is_hidden(&a));
    // Re-applying the stale register changes nothing (idempotent convergence).
    ws.apply_remote_hidden(&a, &early).unwrap();
    assert!(ws.is_hidden(&a));
    assert_eq!(ws.db().query_hidden(0, 100).unwrap().len(), 1);
    assert_eq!(ws.verify(&a).unwrap(), VerifyOutcome::Accept);
}

/// The manual stacking path writes through to the index the same way the importer does: the
/// `assets` stack columns are a projection of the signed register, so
/// `set_stack_membership` immediately suppresses a non-primary member from the timeline,
/// and leaving the stack (a stamped `None`) puts it back.
#[test]
fn set_stack_membership_projects_onto_the_index_both_ways() {
    use capsule_core::domain::StackType;
    use capsule_core::sidecar::sidecar_v1::StackRole;

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Shoot").unwrap();
    let [a, b, _] = import_trio(&mut ws, album, src.path());

    let stack_id = Uuid::now_v7();
    for (id, role, index) in [(a, StackRole::Primary, 0), (b, StackRole::Member, 1)] {
        ws.set_stack_membership(
            &id,
            Some(StackMembership {
                stack_id,
                stack_type: StackType::Burst,
                role,
                member_index: Some(index),
            }),
        )
        .unwrap();
    }

    let row = ws.db().find_by_uuid(&b.to_string()).unwrap().unwrap();
    assert_eq!(row.stack_id.as_deref(), Some(stack_id.to_string().as_str()));
    assert!(
        row.is_stack_hidden,
        "a non-primary member leaves the timeline"
    );
    assert_eq!(ws.db().query_timeline(0, 100).unwrap().len(), 2);

    // Leaving the stack is a *stamped* `None`, and the register stays authoritative: the
    // columns clear rather than falling back to a stale placement.
    ws.set_stack_membership(&b, None).unwrap();
    let row = ws.db().find_by_uuid(&b.to_string()).unwrap().unwrap();
    assert_eq!(row.stack_id, None);
    assert!(!row.is_stack_hidden);
    assert_eq!(ws.db().query_timeline(0, 100).unwrap().len(), 3);
}

/// A stack/group's cull state is *derived* from its members (all-rejected → any-pick
/// → mixed), and flagging a collapsed stack applies the flag to each member.
#[test]
fn group_cull_state_derives_from_stack_members() {
    use capsule_core::culling::GroupCullState;
    use capsule_core::domain::StackType;
    use capsule_core::sidecar::sidecar_v1::{CullFlag, StackMembership, StackRole};

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Shoot").unwrap();
    let [a, b, standalone] = import_trio(&mut ws, album, src.path());

    // a + b form a burst; `standalone` stays out of the stack.
    let stack_id = Uuid::now_v7();
    ws.set_stack_membership(
        &a,
        Some(StackMembership {
            stack_id,
            stack_type: StackType::Burst,
            role: StackRole::Primary,
            member_index: Some(0),
        }),
    )
    .unwrap();
    ws.set_stack_membership(
        &b,
        Some(StackMembership {
            stack_id,
            stack_type: StackType::Burst,
            role: StackRole::Member,
            member_index: Some(1),
        }),
    )
    .unwrap();

    // All members neutral → Mixed. An empty/unknown stack has no derived state.
    assert_eq!(ws.group_cull_state(stack_id), Some(GroupCullState::Mixed));
    assert_eq!(ws.group_cull_state(Uuid::now_v7()), None);

    // Flagging the collapsed stack applies to each member (one update per member).
    let flagged = ws.flag_stack(stack_id, CullFlag::Reject).unwrap();
    assert_eq!(flagged, {
        let mut m = vec![a, b];
        m.sort();
        m
    });
    assert_eq!(ws.cull_flag(&a), CullFlag::Reject);
    assert_eq!(ws.cull_flag(&b), CullFlag::Reject);
    // The standalone asset is untouched by a stack flag.
    assert_eq!(ws.cull_flag(&standalone), CullFlag::Neutral);

    // Every member rejected → AllRejected.
    assert_eq!(
        ws.group_cull_state(stack_id),
        Some(GroupCullState::AllRejected)
    );
    // A single pick surfaces the whole group as a keeper → AnyPick.
    ws.set_cull(&a, CullFlag::Pick).unwrap();
    assert_eq!(ws.group_cull_state(stack_id), Some(GroupCullState::AnyPick));
}

// ── Labels on regions and stacks (#667) ─────────────────────────────────────────────

fn face() -> RegionShape {
    RegionShape::Rect {
        x: 0.2,
        y: 0.1,
        w: 0.3,
        h: 0.4,
    }
}

fn burst(stack_id: Uuid) -> Option<StackMembership> {
    use capsule_core::domain::StackType;
    use capsule_core::sidecar::sidecar_v1::StackRole;
    Some(StackMembership {
        stack_id,
        stack_type: StackType::Burst,
        role: StackRole::Member,
        member_index: None,
    })
}

/// Regions are signed `metadata-update`s: add, label, unlabel and delete each land in the
/// sidecar, the asset still verifies, and the regions survive a reopen.
#[test]
fn region_labels_round_trip_through_signed_metadata_updates() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Faces").unwrap();
    let [asset, _, _] = import_trio(&mut ws, album, src.path());

    let region = ws.add_region(&asset, face()).unwrap();
    ws.add_region_label(&asset, &region, "Alice").unwrap();
    ws.add_region_label(&asset, &region, "Bob").unwrap();
    assert_eq!(ws.remove_region_label(&asset, &region, "Bob").unwrap(), 1);
    assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);

    let other = ws.add_region(&asset, face()).unwrap();
    ws.delete_region(&asset, &other).unwrap();
    assert!(matches!(
        ws.add_region_label(&asset, &other, "late"),
        Err(LifecycleError::NotFound(_))
    ));
    assert!(matches!(
        ws.add_region(
            &asset,
            RegionShape::Circle {
                cx: 2.0,
                cy: 0.5,
                r: 0.1
            }
        ),
        Err(LifecycleError::InvalidRegion(_))
    ));

    drop(ws);
    let ws = Workspace::open(lib.path(), b"passphrase", super::super::FAST_PARAMS).unwrap();
    let regions = ws.regions(&asset);
    assert_eq!(regions.len(), 1, "the deleted region is not live");
    assert_eq!(regions[0].0, region);
    assert_eq!(
        regions[0].1.labels.value().into_iter().collect::<Vec<_>>(),
        vec!["Alice".to_string()]
    );
    assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
}

/// The open-time counter reseed counts region labels, so a reopened device never re-issues
/// an `add_id` a region already holds.
#[test]
fn counter_reseed_counts_region_labels() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Faces").unwrap();
    let [asset, _, _] = import_trio(&mut ws, album, src.path());
    let region = ws.add_region(&asset, face()).unwrap();
    ws.add_region_label(&asset, &region, "Alice").unwrap();
    let issued = ws
        .asset(&asset)
        .unwrap()
        .sidecar
        .max_add_counter_for(&ws.device_id());

    drop(ws);
    let ws = Workspace::open(lib.path(), b"passphrase", super::super::FAST_PARAMS).unwrap();
    assert!(ws.counter.peek() > issued.unwrap());
}

/// Two replicas of one asset's regions converge whichever merges first: concurrent labels
/// on one region both survive, and a delete on one side wins over a label on the other.
#[test]
fn remote_regions_converge_in_either_order() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Faces").unwrap();
    let [asset, _, _] = import_trio(&mut ws, album, src.path());
    let kept = ws.add_region(&asset, face()).unwrap();
    let doomed = ws.add_region(&asset, face()).unwrap();
    let base = ws.asset(&asset).unwrap().sidecar.regions.clone();

    // Replica A: label both regions. Replica B (built from the same base): label `kept`
    // differently and delete `doomed`.
    ws.add_region_label(&asset, &kept, "Alice").unwrap();
    ws.add_region_label(&asset, &doomed, "Carol").unwrap();
    let replica_a = ws.asset(&asset).unwrap().sidecar.regions.clone();

    let mut replica_b = base;
    let device_b = Uuid::from_u128(0xB);
    let mut kept_b = live_region(&replica_b[&kept]).unwrap().clone();
    kept_b.labels.add(
        "Bob".into(),
        capsule_core::metadata::crdt::AddId {
            device: device_b,
            counter: 0,
        },
    );
    write_region(
        replica_b.get_mut(&kept).unwrap(),
        Some(kept_b),
        now_rfc3339(),
        device_b,
    );
    write_region(
        replica_b.get_mut(&doomed).unwrap(),
        None,
        "2000-01-01T00:00:00Z".into(),
        device_b,
    );

    let mut ab = replica_a.clone();
    merge_regions(&mut ab, &replica_b);
    let mut ba = replica_b.clone();
    merge_regions(&mut ba, &replica_a);
    assert_eq!(ab, ba);

    ws.apply_remote_regions(&asset, &replica_b).unwrap();
    let live = ws.regions(&asset);
    assert_eq!(
        live.len(),
        1,
        "the delete wins even though it is stamped earlier"
    );
    assert_eq!(
        live[0].1.labels.value(),
        std::collections::BTreeSet::from(["Alice".to_string(), "Bob".to_string()])
    );
    assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
}

/// A stack's labels live in its signed record, sealed under its album's AMK; every member
/// shares that album, so a join from another album is refused.
#[test]
fn stack_labels_live_in_the_record_and_members_share_one_album() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Burst").unwrap();
    let elsewhere = ws.create_album("Elsewhere").unwrap();
    let [a, b, _] = import_trio(&mut ws, album, src.path());
    let p = src.path().join("outsider.jpg");
    fs::write(&p, b"\xFF\xD8\xFF an asset in another album").unwrap();
    let outsider = ws.import_asset(elsewhere, &p).unwrap();

    let stack = Uuid::now_v7();
    assert!(matches!(
        ws.add_stack_label(stack, "empty"),
        Err(LifecycleError::NotFound(_))
    ));
    ws.set_stack_membership(&a, burst(stack)).unwrap();
    ws.set_stack_membership(&b, burst(stack)).unwrap();
    assert!(matches!(
        ws.set_stack_membership(&outsider, burst(stack)),
        Err(LifecycleError::StackAlbumMismatch { stack_album, asset_album, .. })
            if stack_album == album && asset_album == elsewhere
    ));

    ws.add_stack_label(stack, "hdr").unwrap();
    ws.add_stack_label(stack, "best").unwrap();
    assert_eq!(ws.remove_stack_label(stack, "best").unwrap(), 1);
    assert_eq!(
        ws.stack_labels(stack).unwrap(),
        std::collections::BTreeSet::from(["hdr".to_string()])
    );
    let doc = ws.stack_doc(stack).unwrap();
    assert_eq!(doc.album_id, album);
    assert!(doc.verify(&ws.user_ik_public()));

    // The sealed record opens under the album's key and merges idempotently.
    let wire = ws.seal_stack_doc(stack).unwrap();
    ws.apply_remote_stack_doc(stack, album, &wire).unwrap();
    assert_eq!(
        ws.stack_labels(stack).unwrap(),
        std::collections::BTreeSet::from(["hdr".to_string()])
    );
    assert!(ws.apply_remote_stack_doc(stack, elsewhere, &wire).is_err());
}

/// A stack in `album` holding one member, labelled `hdr`.
fn labelled_stack(ws: &mut Workspace, album: Uuid, src: &Path) -> Uuid {
    let [member, _, _] = import_trio(ws, album, src);
    let stack = Uuid::now_v7();
    ws.set_stack_membership(&member, burst(stack)).unwrap();
    ws.add_stack_label(stack, "hdr").unwrap();
    stack
}

/// A remote stack record that opens under the album's AMK is still refused when another
/// account's identity key signed it, or when it places the stack in another album; nothing
/// is merged.
#[test]
fn a_remote_stack_record_another_account_signed_is_refused() {
    use capsule_core::crypto::keys::HybridSigningKey;
    use capsule_core::metadata::crdt::AddId;

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Burst").unwrap();
    let elsewhere = ws.create_album("Elsewhere").unwrap();
    let stack = labelled_stack(&mut ws, album, src.path());
    let keys = ws.album(&album).unwrap();
    let amk = Amk::from_bytes(keys.amks[&keys.current_epoch]);
    let forge = |album_id: Uuid, signer: &HybridSigningKey| {
        let mut doc = StackDocV1::new(stack, album_id);
        doc.labels.add(
            "forged".into(),
            AddId {
                device: Uuid::from_u128(0xC),
                counter: 0,
            },
        );
        doc.sign(signer);
        doc.seal(&amk).unwrap()
    };

    let foreign = forge(album, &HybridSigningKey::generate());
    assert!(matches!(
        ws.apply_remote_stack_doc(stack, album, &foreign),
        Err(LifecycleError::UnverifiedDocument(id)) if id == stack
    ));
    let misplaced = forge(elsewhere, &ws.account.user_ik);
    assert!(matches!(
        ws.apply_remote_stack_doc(stack, album, &misplaced),
        Err(LifecycleError::UnverifiedDocument(id)) if id == stack
    ));
    assert_eq!(
        ws.stack_labels(stack).unwrap(),
        std::collections::BTreeSet::from(["hdr".to_string()])
    );
}

/// A persisted stack record that was altered after signing, or filed under another stack's
/// id, is refused when read rather than trusted.
#[test]
fn a_tampered_persisted_stack_record_is_refused() {
    use capsule_core::metadata::crdt::AddId;

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Burst").unwrap();
    let stack = labelled_stack(&mut ws, album, src.path());
    let path = stack_doc_path(lib.path(), &stack);

    let mut doc = ws.stack_doc(stack).unwrap();
    doc.labels.add(
        "injected".into(),
        AddId {
            device: Uuid::from_u128(0xC),
            counter: 0,
        },
    );
    fs::write(&path, doc.to_canonical_vec()).unwrap();
    assert!(matches!(
        ws.stack_doc(stack),
        Err(LifecycleError::UnverifiedDocument(id)) if id == stack
    ));

    let mut other = StackDocV1::new(Uuid::now_v7(), album);
    other.sign(&ws.account.user_ik);
    fs::write(&path, other.to_canonical_vec()).unwrap();
    assert!(matches!(
        ws.stack_doc(stack),
        Err(LifecycleError::UnverifiedDocument(id)) if id == stack
    ));

    fs::write(&path, b"not a record").unwrap();
    assert!(matches!(
        ws.stack_doc(stack),
        Err(LifecycleError::Document(_))
    ));
}
