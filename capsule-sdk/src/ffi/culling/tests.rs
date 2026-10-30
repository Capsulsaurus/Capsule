use std::sync::Arc;

use capsule_core::domain::StackType;
use capsule_core::sidecar::sidecar_v1::{StackMembership, StackRole};
use uuid::Uuid;

use super::*;
use crate::ffi::{FfiClientBuild, FfiDeviceTier};

/// A workspace at a fresh root holding `count` sealed assets, and their ids in seal order.
fn workspace_with(root: &std::path::Path, count: usize) -> (Arc<FfiWorkspace>, Vec<String>) {
    let workspace = FfiWorkspace::create(
        root.to_string_lossy().into_owned(),
        b"test-passphrase".to_vec(),
        FfiDeviceTier::LowRam,
        FfiClientBuild {
            client_id: "capsule-ffi-test".into(),
            semver: "0.0.1".into(),
        },
    )
    .expect("enrollment succeeds");
    let album = workspace
        .ensure_album(workspace.default_album_id().unwrap(), "Camera Roll".into())
        .unwrap();
    let assets = (0..count)
        .map(|i| {
            workspace
                .seal_asset(
                    album.clone(),
                    format!("IMG_{i:04}.JPG"),
                    vec![0xFF, 0xD8, i as u8],
                )
                .unwrap()
        })
        .collect();
    (workspace, assets)
}

/// Join `assets` into one stack by writing their membership registers directly: the FFI has no
/// stack-editing verb yet, and the cull verbs only read membership.
fn stack(workspace: &FfiWorkspace, assets: &[String]) -> String {
    let stack_id = Uuid::now_v7();
    workspace
        .with(|ws| {
            for (i, asset) in assets.iter().enumerate() {
                let membership = StackMembership {
                    stack_id,
                    stack_type: StackType::Burst,
                    role: if i == 0 {
                        StackRole::Primary
                    } else {
                        StackRole::Member
                    },
                    member_index: Some(i as u32),
                };
                ws.set_stack_membership(&Uuid::parse_str(asset).unwrap(), Some(membership))?;
            }
            Ok(())
        })
        .unwrap();
    stack_id.to_string()
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

#[test]
fn cull_flags_convert_both_ways() {
    for flag in [FfiCullFlag::Pick, FfiCullFlag::Neutral, FfiCullFlag::Reject] {
        assert_eq!(FfiCullFlag::from(CullFlag::from(flag)), flag);
    }
    assert_eq!(
        FfiStackCullState::from(GroupCullState::AllRejected),
        FfiStackCullState::AllRejected
    );
    assert_eq!(
        FfiStackCullState::from(GroupCullState::AnyPick),
        FfiStackCullState::AnyPick
    );
    assert_eq!(
        FfiStackCullState::from(GroupCullState::Mixed),
        FfiStackCullState::Mixed
    );
}

/// A flag is written, read back, filtered on, and cleared by `Neutral`.
#[test]
fn an_asset_flag_round_trips_and_is_reversible() {
    let lib = tempfile::TempDir::new().unwrap();
    let (workspace, assets) = workspace_with(lib.path(), 2);
    let (a, b) = (assets[0].clone(), assets[1].clone());

    assert_eq!(
        workspace.cull_flag(a.clone()).unwrap(),
        FfiCullFlag::Neutral
    );
    assert_eq!(
        sorted(workspace.assets_by_cull(FfiCullFlag::Neutral).unwrap()),
        sorted(assets.clone()),
        "never-flagged assets read as neutral"
    );

    workspace.set_cull(a.clone(), FfiCullFlag::Pick).unwrap();
    workspace.set_cull(b.clone(), FfiCullFlag::Reject).unwrap();
    assert_eq!(workspace.cull_flag(a.clone()).unwrap(), FfiCullFlag::Pick);
    assert_eq!(
        workspace.assets_by_cull(FfiCullFlag::Pick).unwrap(),
        vec![a.clone()]
    );
    assert_eq!(
        workspace.assets_by_cull(FfiCullFlag::Reject).unwrap(),
        vec![b.clone()]
    );

    workspace.set_cull(b.clone(), FfiCullFlag::Neutral).unwrap();
    assert_eq!(
        workspace.cull_flag(b.clone()).unwrap(),
        FfiCullFlag::Neutral
    );
    assert!(
        workspace
            .assets_by_cull(FfiCullFlag::Reject)
            .unwrap()
            .is_empty()
    );
}

/// Flagging a stack flags every member, and the stack's state is derived from them each read.
#[test]
fn a_stack_flag_reaches_every_member_and_its_state_is_derived() {
    let lib = tempfile::TempDir::new().unwrap();
    let (workspace, assets) = workspace_with(lib.path(), 3);
    let stack_id = stack(&workspace, &assets[..2]);

    assert_eq!(
        workspace.stack_cull_state(stack_id.clone()).unwrap(),
        Some(FfiStackCullState::Mixed)
    );
    let flagged = workspace
        .flag_stack(stack_id.clone(), FfiCullFlag::Reject)
        .unwrap();
    assert_eq!(flagged, sorted(assets[..2].to_vec()));
    assert_eq!(
        workspace.stack_cull_state(stack_id.clone()).unwrap(),
        Some(FfiStackCullState::AllRejected)
    );
    assert_eq!(
        workspace.cull_flag(assets[2].clone()).unwrap(),
        FfiCullFlag::Neutral,
        "an asset outside the stack is untouched"
    );

    workspace
        .set_cull(assets[0].clone(), FfiCullFlag::Pick)
        .unwrap();
    assert_eq!(
        workspace.stack_cull_state(stack_id).unwrap(),
        Some(FfiStackCullState::AnyPick)
    );

    let empty = Uuid::now_v7().to_string();
    assert_eq!(workspace.stack_cull_state(empty.clone()).unwrap(), None);
    assert!(
        workspace
            .flag_stack(empty, FfiCullFlag::Pick)
            .unwrap()
            .is_empty()
    );
}

/// The sweep trashes exactly the rejects, restorably, and a second sweep finds nothing.
#[test]
fn the_reject_sweep_trashes_only_rejects_and_they_stay_restorable() {
    let lib = tempfile::TempDir::new().unwrap();
    let (workspace, assets) = workspace_with(lib.path(), 3);
    workspace
        .set_cull(assets[0].clone(), FfiCullFlag::Reject)
        .unwrap();
    workspace
        .set_cull(assets[1].clone(), FfiCullFlag::Reject)
        .unwrap();
    workspace
        .set_cull(assets[2].clone(), FfiCullFlag::Pick)
        .unwrap();

    let swept = workspace.reject_sweep(30).unwrap();
    assert_eq!(swept, sorted(assets[..2].to_vec()));
    let trashed: Vec<String> = workspace
        .trash()
        .unwrap()
        .into_iter()
        .map(|entry| {
            assert!(entry.restorable);
            entry.asset_id
        })
        .collect();
    assert_eq!(sorted(trashed), swept);
    assert!(
        workspace
            .assets_by_cull(FfiCullFlag::Reject)
            .unwrap()
            .is_empty(),
        "a trashed reject leaves the filtered view"
    );
    assert!(
        workspace.reject_sweep(30).unwrap().is_empty(),
        "nothing left to sweep"
    );

    workspace.restore_asset(assets[0].clone()).unwrap();
    assert_eq!(
        workspace.cull_flag(assets[0].clone()).unwrap(),
        FfiCullFlag::Reject,
        "a restore brings the asset back with its flag"
    );
}

/// Bad ids are typed argument errors; flagging an asset the workspace does not hold is refused.
#[test]
fn bad_arguments_are_typed_errors() {
    let lib = tempfile::TempDir::new().unwrap();
    let (workspace, _) = workspace_with(lib.path(), 1);

    assert!(matches!(
        workspace.set_cull("not-a-uuid".into(), FfiCullFlag::Pick),
        Err(FfiError::InvalidArgument { .. })
    ));
    assert!(matches!(
        workspace.stack_cull_state("nope".into()),
        Err(FfiError::InvalidArgument { .. })
    ));
    assert!(
        workspace
            .set_cull(Uuid::now_v7().to_string(), FfiCullFlag::Pick)
            .is_err()
    );
    assert_eq!(
        workspace.cull_flag(Uuid::now_v7().to_string()).unwrap(),
        FfiCullFlag::Neutral,
        "an unknown asset reads as never flagged"
    );
}
