//! The one suite every [`ModerationStore`] adapter must pass.
//!
//! # The rules the suite exists to protect
//!
//! - **No silent operations.** [`ModerationStore::apply`] records the event whether or not it
//!   moves the standing, and a standing it moves is the standing the next read answers.
//! - **The record is the user's, in the order it happened.** Events read back oldest-first in
//!   the order they were applied — not re-sorted by their instants — and only to the account
//!   they were taken against.
//! - **A reinstatement does not erase.** Lifting a suspension restores [`Standing::Active`] and
//!   leaves the suspension on the record.
//! - **A report is an input, never a decision.** Filing one changes no standing and writes no
//!   event; a duplicate `report_id` is refused and the first report stands; the queue reads in
//!   `report_id` order with every field, the signed bytes included, exactly as filed.
//!
//! What the suite does **not** pin is the in-memory adapter's queue cap
//! ([`super::MAX_IN_MEMORY_REPORTS`]): that is the double's own guard against unbounded process
//! memory, and a durable queue must not evict.
//!
//! # Reusing a harness
//!
//! Every case scopes its own account and report ids, so cases may share one store and
//! [`run_all`] does; the queue cases read only their own reports out of it.

use jiff::{SignedDuration, Timestamp};

use super::{FederatedReport, ModerationAction, ModerationEvent, ModerationStore, Standing};
use crate::store::{AlbumId, AssetId, StoreError, UserId};

/// The store under test.
pub trait Harness: Send + Sync {
    /// The moderation store under test.
    fn moderation(&self) -> &dyn ModerationStore;
}

/// Unwrap a store result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, StoreError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming moderation store must succeed at {doing}: {error}"),
    }
}

/// An instant `secs` seconds after a fixed origin.
fn at(secs: i64) -> Timestamp {
    Timestamp::UNIX_EPOCH + SignedDuration::from_secs(1_700_000_000 + secs)
}

/// `case`'s account.
fn user(case: &str) -> UserId {
    UserId::new(format!("{case}-user"))
}

/// An action against `case`'s account at `secs`.
fn event(case: &str, action: ModerationAction, secs: i64, reason: Option<&str>) -> ModerationEvent {
    ModerationEvent {
        user_id: user(case),
        action,
        asset_id: None,
        at: at(secs),
        reason: reason.map(str::to_owned),
    }
}

/// A report `n` from `case`, about `case`'s account.
fn report(case: &str, n: u32) -> FederatedReport {
    FederatedReport {
        report_id: format!("{case}-report-{n:04}"),
        reporting_server: "peer.example.test".to_owned(),
        reported_user: user(case),
        asset_hash: format!("b3:{case}{n:060}"),
        album_id: AlbumId::new(format!("{case}-album")),
        reason: Some("spam".to_owned()),
        reported_at: at(i64::from(n)),
        received_at: at(i64::from(n) + 1),
        signature: vec![u8::try_from(n % 256).expect("a byte"); 64],
        signed: format!("{case}/signed/{n}").into_bytes(),
    }
}

/// Apply `event`, moving the standing to `standing` if given.
async fn apply(h: &dyn Harness, event: ModerationEvent, standing: Option<Standing>) {
    ok(
        h.moderation().apply(event, standing).await,
        "apply a moderation action",
    );
}

/// `case`'s account's standing.
async fn standing(h: &dyn Harness, case: &str) -> Standing {
    ok(
        h.moderation().standing(&user(case)).await,
        "read a standing",
    )
}

/// `case`'s account's record.
async fn record(h: &dyn Harness, case: &str) -> Vec<ModerationEvent> {
    ok(
        h.moderation().events_for_user(&user(case)).await,
        "read a moderation record",
    )
}

/// The reports in the queue that `case` filed, in queue order.
async fn reports_of(h: &dyn Harness, case: &str) -> Vec<FederatedReport> {
    let prefix = format!("{case}-report-");
    ok(
        h.moderation().pending_reports().await,
        "read the report queue",
    )
    .into_iter()
    .filter(|report| report.report_id.starts_with(&prefix))
    .collect()
}

// ===========================================================================================
// Standing and the record
// ===========================================================================================

/// An account nothing has been done to is active and has an empty record.
pub async fn an_untouched_account_is_active_with_an_empty_record(h: &dyn Harness) {
    let case = "untouched";
    assert_eq!(standing(h, case).await, Standing::Active);
    assert!(record(h, case).await.is_empty());
}

/// A suspension moves the standing and leaves the event the user reads.
pub async fn a_suspension_moves_the_standing_and_is_recorded(h: &dyn Harness) {
    let case = "suspend";
    let suspended = event(case, ModerationAction::Suspended, 10, Some("abuse"));
    apply(
        h,
        suspended.clone(),
        Some(Standing::Suspended { since: at(10) }),
    )
    .await;
    assert_eq!(
        standing(h, case).await,
        Standing::Suspended { since: at(10) }
    );
    assert!(!standing(h, case).await.may_write());
    assert_eq!(record(h, case).await, vec![suspended]);
}

/// Reinstating lifts the suspension and keeps it on the record.
pub async fn a_reinstatement_lifts_the_suspension_and_does_not_erase_it(h: &dyn Harness) {
    let case = "reinstate";
    let suspended = event(case, ModerationAction::Suspended, 10, None);
    let reinstated = event(
        case,
        ModerationAction::Reinstated,
        20,
        Some("appeal upheld"),
    );
    apply(
        h,
        suspended.clone(),
        Some(Standing::Suspended { since: at(10) }),
    )
    .await;
    apply(h, reinstated.clone(), Some(Standing::Active)).await;
    assert_eq!(standing(h, case).await, Standing::Active);
    assert_eq!(record(h, case).await, vec![suspended, reinstated]);
}

/// An action that moves no standing is still recorded, and the standing is left alone.
pub async fn an_asset_action_records_without_touching_standing(h: &dyn Harness) {
    let case = "asset";
    apply(
        h,
        event(case, ModerationAction::Suspended, 1, None),
        Some(Standing::Suspended { since: at(1) }),
    )
    .await;
    let takedown = ModerationEvent {
        asset_id: Some(AssetId::new(format!("{case}-asset"))),
        ..event(case, ModerationAction::TakenDown, 2, Some("copyright"))
    };
    apply(h, takedown.clone(), None).await;
    assert_eq!(
        standing(h, case).await,
        Standing::Suspended { since: at(1) },
        "an asset action must not reinstate or re-suspend the account"
    );
    assert_eq!(record(h, case).await.last(), Some(&takedown));

    let other = "asset-only";
    let hold = ModerationEvent {
        asset_id: Some(AssetId::new(format!("{other}-asset"))),
        ..event(other, ModerationAction::LegalHold, 3, None)
    };
    apply(h, hold.clone(), None).await;
    assert_eq!(standing(h, other).await, Standing::Active);
    assert_eq!(record(h, other).await, vec![hold]);
}

/// The record reads in the order actions were applied, not in the order of their instants.
pub async fn the_record_is_in_the_order_it_was_applied(h: &dyn Harness) {
    let case = "order";
    let events = vec![
        event(case, ModerationAction::LegalHold, 50, None),
        // An earlier instant applied later, and two actions sharing one instant.
        event(case, ModerationAction::HoldLifted, 10, None),
        event(case, ModerationAction::TakenDown, 10, Some("second at ten")),
        event(case, ModerationAction::Suspended, 10, Some("third at ten")),
    ];
    for event in events.clone() {
        apply(h, event, None).await;
    }
    assert_eq!(record(h, case).await, events);
}

/// One account's record and standing are invisible to another.
pub async fn a_record_is_scoped_to_its_account(h: &dyn Harness) {
    let case = "scoped";
    let other = "scoped-other";
    apply(
        h,
        event(case, ModerationAction::Suspended, 1, None),
        Some(Standing::Suspended { since: at(1) }),
    )
    .await;
    assert_eq!(standing(h, other).await, Standing::Active);
    assert!(record(h, other).await.is_empty());
}

/// Every action, and every absent optional field, reads back as written.
pub async fn every_action_and_absent_field_round_trips(h: &dyn Harness) {
    let case = "fields";
    let events: Vec<ModerationEvent> = [
        ModerationAction::Suspended,
        ModerationAction::Reinstated,
        ModerationAction::TakenDown,
        ModerationAction::LegalHold,
        ModerationAction::HoldLifted,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, action)| {
        let secs = i64::try_from(i).expect("a small index");
        ModerationEvent {
            // Alternate present and absent, so both spellings of each optional are exercised.
            asset_id: (i % 2 == 0).then(|| AssetId::new(format!("{case}-asset-{i}"))),
            ..event(case, action, secs, (i % 2 == 1).then_some("a reason"))
        }
    })
    .collect();
    for event in events.clone() {
        apply(h, event, None).await;
    }
    assert_eq!(record(h, case).await, events);
}

/// Instants read back to the microsecond: a suspension's start and an event's `at`.
pub async fn instants_read_back_to_the_microsecond(h: &dyn Harness) {
    let case = "precise";
    let precise = Timestamp::from_nanosecond(1_700_000_000_123_456_789).expect("an instant");
    apply(
        h,
        ModerationEvent {
            at: precise,
            ..event(case, ModerationAction::Suspended, 0, None)
        },
        Some(Standing::Suspended { since: precise }),
    )
    .await;
    let Standing::Suspended { since } = standing(h, case).await else {
        panic!("suspended");
    };
    assert_eq!(since.as_microsecond(), precise.as_microsecond());
    let events = record(h, case).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].at.as_microsecond(), precise.as_microsecond());
    assert_eq!(record(h, case).await, events, "and the same on every read");
}

// ===========================================================================================
// The federated report queue
// ===========================================================================================

/// A filed report is pending, every field and the signed bytes exactly as filed.
pub async fn a_filed_report_is_pending_exactly_as_filed(h: &dyn Harness) {
    let case = "filed";
    let filed = report(case, 1);
    let unreasoned = FederatedReport {
        reason: None,
        ..report(case, 2)
    };
    ok(
        h.moderation().file_report(filed.clone()).await,
        "file a report",
    );
    ok(
        h.moderation().file_report(unreasoned.clone()).await,
        "file a report",
    );
    assert_eq!(reports_of(h, case).await, vec![filed, unreasoned]);
}

/// A second report under the same id is refused, and the first one stands.
pub async fn a_duplicate_report_id_is_refused_and_the_first_stands(h: &dyn Harness) {
    let case = "duplicate";
    let first = report(case, 1);
    ok(
        h.moderation().file_report(first.clone()).await,
        "file a report",
    );
    let impostor = FederatedReport {
        reason: Some("a different complaint".to_owned()),
        signed: b"other bytes".to_vec(),
        ..first.clone()
    };
    let refusal = h.moderation().file_report(impostor).await;
    assert!(
        matches!(refusal, Err(StoreError::Rejected { .. })),
        "a duplicate id is a bug, refused as such: {refusal:?}"
    );
    assert_eq!(reports_of(h, case).await, vec![first]);
}

/// The queue reads in `report_id` order, whatever order the reports were filed in.
pub async fn the_queue_reads_in_report_id_order(h: &dyn Harness) {
    let case = "queue";
    for n in [3, 1, 2] {
        ok(
            h.moderation().file_report(report(case, n)).await,
            "file a report",
        );
    }
    let ids: Vec<String> = reports_of(h, case)
        .await
        .into_iter()
        .map(|report| report.report_id)
        .collect();
    assert_eq!(
        ids,
        vec![
            format!("{case}-report-0001"),
            format!("{case}-report-0002"),
            format!("{case}-report-0003"),
        ]
    );
}

/// Filing a report is an input to a decision: no standing moves and no event is written.
pub async fn filing_a_report_changes_no_standing_and_writes_no_event(h: &dyn Harness) {
    let case = "input";
    ok(
        h.moderation().file_report(report(case, 1)).await,
        "file a report",
    );
    assert_eq!(standing(h, case).await, Standing::Active);
    assert!(record(h, case).await.is_empty());
}

/// A report's two instants read back to the microsecond.
pub async fn a_reports_instants_read_back_to_the_microsecond(h: &dyn Harness) {
    let case = "report-precise";
    let precise = FederatedReport {
        reported_at: Timestamp::from_nanosecond(1_700_000_000_123_456_789).expect("an instant"),
        received_at: Timestamp::from_nanosecond(1_700_000_001_987_654_321).expect("an instant"),
        ..report(case, 1)
    };
    ok(
        h.moderation().file_report(precise.clone()).await,
        "file a report",
    );
    let held = reports_of(h, case).await;
    assert_eq!(held.len(), 1);
    assert_eq!(
        held[0].reported_at.as_microsecond(),
        precise.reported_at.as_microsecond()
    );
    assert_eq!(
        held[0].received_at.as_microsecond(),
        precise.received_at.as_microsecond()
    );
}

// ===========================================================================================
// The whole suite
// ===========================================================================================

/// Run every case above against one harness, in order.
pub async fn run_all(h: &dyn Harness) {
    an_untouched_account_is_active_with_an_empty_record(h).await;
    a_suspension_moves_the_standing_and_is_recorded(h).await;
    a_reinstatement_lifts_the_suspension_and_does_not_erase_it(h).await;
    an_asset_action_records_without_touching_standing(h).await;
    the_record_is_in_the_order_it_was_applied(h).await;
    a_record_is_scoped_to_its_account(h).await;
    every_action_and_absent_field_round_trips(h).await;
    instants_read_back_to_the_microsecond(h).await;

    a_filed_report_is_pending_exactly_as_filed(h).await;
    a_duplicate_report_id_is_refused_and_the_first_stands(h).await;
    the_queue_reads_in_report_id_order(h).await;
    filing_a_report_changes_no_standing_and_writes_no_event(h).await;
    a_reports_instants_read_back_to_the_microsecond(h).await;
}

#[cfg(test)]
mod tests;
