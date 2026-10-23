use super::*;

#[test]
fn empty_group_has_no_derived_state() {
    assert_eq!(GroupCullState::derive(std::iter::empty()), None);
}

#[test]
fn all_rejected_members_derive_all_rejected() {
    let flags = [CullFlag::Reject, CullFlag::Reject, CullFlag::Reject];
    assert_eq!(
        GroupCullState::derive(flags),
        Some(GroupCullState::AllRejected)
    );
}

#[test]
fn any_pick_wins_over_rejects_and_neutrals() {
    // A single pick among rejects/neutrals surfaces the group as a keeper — the
    // "any-pick" arm outranks "mixed".
    let flags = [CullFlag::Reject, CullFlag::Pick, CullFlag::Neutral];
    assert_eq!(GroupCullState::derive(flags), Some(GroupCullState::AnyPick));
}

#[test]
fn a_single_pick_makes_the_group_any_pick_not_all_rejected() {
    let flags = [CullFlag::Reject, CullFlag::Pick];
    assert_eq!(GroupCullState::derive(flags), Some(GroupCullState::AnyPick));
}

#[test]
fn rejects_mixed_with_neutrals_are_mixed_not_all_rejected() {
    let flags = [CullFlag::Reject, CullFlag::Neutral];
    assert_eq!(GroupCullState::derive(flags), Some(GroupCullState::Mixed));
}

#[test]
fn all_neutral_group_is_mixed() {
    let flags = [CullFlag::Neutral, CullFlag::Neutral];
    assert_eq!(GroupCullState::derive(flags), Some(GroupCullState::Mixed));
}

#[test]
fn derivation_is_order_independent() {
    // The derived state cannot depend on member iteration order.
    let forward = [CullFlag::Pick, CullFlag::Reject, CullFlag::Neutral];
    let reverse = [CullFlag::Neutral, CullFlag::Reject, CullFlag::Pick];
    assert_eq!(
        GroupCullState::derive(forward),
        GroupCullState::derive(reverse)
    );
}
