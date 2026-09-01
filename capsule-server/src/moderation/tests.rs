//! The moderation port's own suite.

use super::*;

fn user() -> UserId {
    UserId::new("01937b7c-0000-7000-8000-000000000001")
}

fn other() -> UserId {
    UserId::new("01937b7c-0000-7000-8000-0000000000ff")
}

fn event(action: ModerationAction, at: i64, reason: Option<&str>) -> ModerationEvent {
    ModerationEvent {
        user_id: user(),
        action,
        asset_id: None,
        at: crate::store::deadline(Timestamp::UNIX_EPOCH, jiff::SignedDuration::from_hours(at)),
        reason: reason.map(str::to_owned),
    }
}

#[tokio::test]
async fn an_account_nothing_has_happened_to_is_active() {
    let store = InMemoryModeration::new();
    assert_eq!(
        store.standing(&user()).await.expect("the store answers"),
        Standing::Active
    );
    assert!(
        store
            .events_for_user(&user())
            .await
            .expect("the store answers")
            .is_empty()
    );
}

#[tokio::test]
async fn suspending_moves_the_standing_and_leaves_a_record_the_user_can_read() {
    // Both, in one operation. A takedown that applied and failed to record itself is the silent
    // operation the contract forbids; a record with no effect tells a user something happened
    // that did not.
    let store = InMemoryModeration::new();
    let since = crate::store::deadline(Timestamp::UNIX_EPOCH, jiff::SignedDuration::from_hours(3));

    store
        .apply(
            event(ModerationAction::Suspended, 3, Some("billing dispute")),
            Some(Standing::Suspended { since }),
        )
        .await
        .expect("the store applies");

    assert_eq!(
        store.standing(&user()).await.expect("the store answers"),
        Standing::Suspended { since }
    );
    let record = store
        .events_for_user(&user())
        .await
        .expect("the store answers");
    assert_eq!(record.len(), 1);
    assert_eq!(record[0].action, ModerationAction::Suspended);
    assert_eq!(record[0].reason.as_deref(), Some("billing dispute"));
}

#[tokio::test]
async fn a_reinstatement_lifts_the_suspension_and_does_not_erase_it() {
    // The record is what a user reads to understand their own account; a reinstatement that
    // deleted the suspension would leave them unable to see that it ever happened.
    let store = InMemoryModeration::new();
    store
        .apply(
            event(ModerationAction::Suspended, 1, None),
            Some(Standing::Suspended {
                since: Timestamp::UNIX_EPOCH,
            }),
        )
        .await
        .expect("the store applies");
    store
        .apply(
            event(ModerationAction::Reinstated, 2, Some("appeal granted")),
            Some(Standing::Active),
        )
        .await
        .expect("the store applies");

    assert_eq!(
        store.standing(&user()).await.expect("the store answers"),
        Standing::Active,
        "suspension is reversible by default"
    );
    let record = store
        .events_for_user(&user())
        .await
        .expect("the store answers");
    assert_eq!(record.len(), 2, "the history stays");
    assert_eq!(record[0].action, ModerationAction::Suspended);
    assert_eq!(record[1].action, ModerationAction::Reinstated);
}

#[tokio::test]
async fn an_asset_action_records_without_touching_standing() {
    // A takedown is about one asset. It must not suspend the account, and passing `None` for the
    // standing is what makes that structural rather than a caller remembering to pass the
    // current value back.
    let store = InMemoryModeration::new();
    let mut taken = event(ModerationAction::TakenDown, 1, None);
    taken.asset_id = Some(AssetId::new("asset-1"));

    store.apply(taken, None).await.expect("the store applies");

    assert_eq!(
        store.standing(&user()).await.expect("the store answers"),
        Standing::Active
    );
    let record = store
        .events_for_user(&user())
        .await
        .expect("the store answers");
    assert_eq!(record[0].asset_id, Some(AssetId::new("asset-1")));
}

#[tokio::test]
async fn a_record_is_scoped_to_its_account() {
    let store = InMemoryModeration::new();
    store
        .apply(event(ModerationAction::Suspended, 1, None), None)
        .await
        .expect("the store applies");

    assert!(
        store
            .events_for_user(&other())
            .await
            .expect("the store answers")
            .is_empty(),
        "one account's moderation record is not another's"
    );
    assert_eq!(
        store.standing(&other()).await.expect("the store answers"),
        Standing::Active
    );
}

#[test]
fn only_an_active_account_may_write() {
    assert!(Standing::Active.may_write());
    assert!(
        !Standing::Suspended {
            since: Timestamp::UNIX_EPOCH
        }
        .may_write()
    );
}

#[test]
fn an_absent_reason_is_a_real_answer() {
    // "Where policy permits" is not "always": a legal hold may come with an obligation not to
    // disclose it. Absent reads as "we are not able to say", which is honest — a fabricated
    // reason would not be.
    let quiet = event(ModerationAction::LegalHold, 1, None);
    assert_eq!(quiet.reason, None);
}
