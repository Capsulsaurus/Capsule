//! The one suite every [`AlbumStore`] adapter must pass.
//!
//! # The rules the suite exists to protect
//!
//! - **Provisioning is idempotent for the owner and closed to everyone else.** The same id
//!   provisioned again by its owner is [`ProvisionOutcome::AlreadyProvisioned`] carrying the
//!   record that was stored first, and by anybody else [`ProvisionOutcome::NotAvailable`]; in
//!   both cases nothing is written.
//! - **One ceremony at a time.** A live upgrade under a different intent is
//!   [`UpgradeOutcome::Conflict`] naming the live one; a replay of the same intent answers
//!   [`UpgradeOutcome::Quiescing`] again; an expired ceremony is no ceremony at all and is
//!   replaced or cleared.
//! - **A guess buys nothing.** An album that does not exist and an album that is somebody
//!   else's answer the same [`UpgradeOutcome::NotFound`], and neither writes.
//! - **Clearing is idempotent.** Ending a ceremony on an album that is not quiescing is
//!   [`UpgradeOutcome::Cleared`], not an error.
//! - **What is returned is what is stored.** A returned record is the record the next read
//!   produces, instants included.
//!
//! # Reusing a harness
//!
//! Every case scopes its own album ids, so cases may share one store and [`run_all`] does.

use jiff::{SignedDuration, Timestamp};
use uuid::Uuid;

use super::{AlbumRecord, AlbumStore, ProvisionOutcome, UpgradeOutcome, UpgradeQuiescence};
use crate::store::{AlbumId, OwnerId, StoreError};

/// The store under test.
pub trait Harness: Send + Sync {
    /// The album store under test.
    fn albums(&self) -> &dyn AlbumStore;
}

/// Unwrap a store result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, StoreError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming album store must succeed at {doing}: {error}"),
    }
}

/// An instant `secs` seconds after a fixed origin.
fn at(secs: i64) -> Timestamp {
    Timestamp::UNIX_EPOCH + SignedDuration::from_secs(1_700_000_000 + secs)
}

/// `case`'s own album.
fn album(case: &str) -> AlbumId {
    AlbumId::new(format!("{case}-album"))
}

/// `case`'s account `name`.
fn owner(case: &str, name: &str) -> OwnerId {
    OwnerId::new(format!("{case}-{name}"))
}

/// A fresh record for `case`'s album, bound to `who`.
fn record(case: &str, who: &OwnerId) -> AlbumRecord {
    AlbumRecord {
        album_id: album(case),
        owner_id: who.clone(),
        protocol_version: "2026-01-01".to_owned(),
        created_at: at(0),
        upgrade: None,
    }
}

/// A ceremony under `intent`, accepted at `received_secs` and open for `deadline_secs`.
fn quiescence(intent: u128, received_secs: i64, deadline_secs: u64) -> UpgradeQuiescence {
    UpgradeQuiescence {
        intent: capsule_core::crypto::upgrade::UpgradeIntent {
            intent_id: Uuid::from_u128(intent),
            from_protocol_version: "2026-01-01".to_owned(),
            to_protocol_version: "2026-06-01".to_owned(),
            from_suite_id: 1,
            to_suite_id: 1,
            proposer_user: Uuid::from_u128(0xA1),
            proposer_device: Uuid::from_u128(0xD1),
            deadline_secs,
        },
        received_at: at(received_secs),
    }
}

/// Provision `case`'s album to `who`, expecting it to be created.
async fn provisioned(h: &dyn Harness, case: &str, who: &OwnerId) -> AlbumRecord {
    let outcome = ok(
        h.albums().provision(record(case, who)).await,
        "provision an album",
    );
    let ProvisionOutcome::Created(created) = outcome else {
        panic!("a fresh album must be created, got {outcome:?}");
    };
    created
}

/// What the store holds for `case`'s album.
async fn held(h: &dyn Harness, case: &str) -> Option<AlbumRecord> {
    ok(h.albums().read(&album(case)).await, "read an album")
}

/// Put `case`'s album into quiescence under `q`, as `who`, at `now_secs`.
async fn begin(
    h: &dyn Harness,
    case: &str,
    who: &OwnerId,
    q: UpgradeQuiescence,
    now_secs: i64,
) -> UpgradeOutcome {
    ok(
        h.albums()
            .begin_upgrade(&album(case), who, q, at(now_secs))
            .await,
        "begin an upgrade",
    )
}

/// End `case`'s ceremony `intent`, as `who`, at `now_secs`.
async fn end(
    h: &dyn Harness,
    case: &str,
    who: &OwnerId,
    intent: u128,
    now_secs: i64,
) -> UpgradeOutcome {
    ok(
        h.albums()
            .end_upgrade(&album(case), who, Uuid::from_u128(intent), at(now_secs))
            .await,
        "end an upgrade",
    )
}

// ===========================================================================================
// Provisioning
// ===========================================================================================

/// A first provision creates the album, and the next read returns exactly what was created.
pub async fn a_first_provision_creates_the_album_the_next_read_returns(h: &dyn Harness) {
    let case = "create";
    let alice = owner(case, "alice");
    let created = provisioned(h, case, &alice).await;
    assert_eq!(created, record(case, &alice));
    assert_eq!(held(h, case).await, Some(created));
}

/// An album nobody provisioned reads as absent.
pub async fn an_unprovisioned_album_reads_as_none(h: &dyn Harness) {
    assert_eq!(held(h, "absent").await, None);
}

/// The owner provisioning again is told it is theirs, with the record stored first.
pub async fn re_provisioning_by_the_owner_answers_the_stored_record_and_writes_nothing(
    h: &dyn Harness,
) {
    let case = "again";
    let alice = owner(case, "alice");
    let created = provisioned(h, case, &alice).await;

    let retry = AlbumRecord {
        protocol_version: "2027-01-01".to_owned(),
        created_at: at(500),
        ..record(case, &alice)
    };
    let outcome = ok(h.albums().provision(retry).await, "re-provision an album");
    assert_eq!(
        outcome,
        ProvisionOutcome::AlreadyProvisioned(created.clone()),
        "the pin is fixed at provisioning; a retry must neither move it nor restamp the album"
    );
    assert_eq!(held(h, case).await, Some(created));
}

/// Anybody else provisioning the id is refused, and the owner's record is untouched.
pub async fn an_id_bound_elsewhere_is_not_available_and_changes_nothing(h: &dyn Harness) {
    let case = "elsewhere";
    let alice = owner(case, "alice");
    let created = provisioned(h, case, &alice).await;

    let outcome = ok(
        h.albums()
            .provision(record(case, &owner(case, "mallory")))
            .await,
        "provision somebody else's album",
    );
    assert_eq!(outcome, ProvisionOutcome::NotAvailable);
    assert_eq!(held(h, case).await, Some(created));
}

/// A returned record's instants are the ones the next read produces.
///
/// An adapter that keeps sub-microsecond precision in what it returns and drops it in the column
/// would hand a caller a record the next read does not reproduce.
pub async fn the_returned_instants_are_the_instants_the_next_read_produces(h: &dyn Harness) {
    let case = "precise";
    let alice = owner(case, "alice");
    let mut precise = record(case, &alice);
    precise.created_at = Timestamp::from_nanosecond(1_700_000_000_123_456_789).expect("an instant");
    let ProvisionOutcome::Created(created) = ok(
        h.albums().provision(precise.clone()).await,
        "provision an album",
    ) else {
        panic!("created");
    };
    assert_eq!(
        created.created_at.as_microsecond(),
        precise.created_at.as_microsecond()
    );
    assert_eq!(held(h, case).await, Some(created));

    let mut q = quiescence(0x1, 0, 3_600);
    q.received_at = Timestamp::from_nanosecond(1_700_000_000_987_654_321).expect("an instant");
    let UpgradeOutcome::Quiescing(quiescing) = begin(h, case, &alice, q.clone(), 0).await else {
        panic!("quiescing");
    };
    let received = quiescing.upgrade.as_ref().expect("a ceremony").received_at;
    assert_eq!(received.as_microsecond(), q.received_at.as_microsecond());
    assert_eq!(held(h, case).await, Some(*quiescing));
}

// ===========================================================================================
// The upgrade ceremony
// ===========================================================================================

/// The owner's proposal puts the album into quiescence, and the read shows the ceremony.
pub async fn a_proposal_quiesces_the_album(h: &dyn Harness) {
    let case = "quiesce";
    let alice = owner(case, "alice");
    let created = provisioned(h, case, &alice).await;
    let q = quiescence(0x10, 0, 3_600);

    let outcome = begin(h, case, &alice, q.clone(), 10).await;
    let expected = AlbumRecord {
        upgrade: Some(q),
        ..created
    };
    assert_eq!(
        outcome,
        UpgradeOutcome::Quiescing(Box::new(expected.clone()))
    );
    assert_eq!(held(h, case).await, Some(expected));
}

/// The same intent again is the same ceremony, not a conflict.
pub async fn a_replayed_proposal_answers_quiescing_again(h: &dyn Harness) {
    let case = "replay";
    let alice = owner(case, "alice");
    provisioned(h, case, &alice).await;
    let q = quiescence(0x20, 0, 3_600);
    let UpgradeOutcome::Quiescing(first) = begin(h, case, &alice, q.clone(), 10).await else {
        panic!("quiescing");
    };
    let replay = begin(h, case, &alice, q, 20).await;
    assert_eq!(replay, UpgradeOutcome::Quiescing(first.clone()));
    assert_eq!(held(h, case).await, Some(*first));
}

/// A different intent while one is live conflicts, names the live one, and writes nothing.
pub async fn a_second_live_ceremony_conflicts_and_changes_nothing(h: &dyn Harness) {
    let case = "conflict";
    let alice = owner(case, "alice");
    provisioned(h, case, &alice).await;
    let UpgradeOutcome::Quiescing(first) =
        begin(h, case, &alice, quiescence(0x30, 0, 3_600), 10).await
    else {
        panic!("quiescing");
    };

    let outcome = begin(h, case, &alice, quiescence(0x31, 100, 3_600), 100).await;
    assert_eq!(
        outcome,
        UpgradeOutcome::Conflict {
            intent_id: Uuid::from_u128(0x30)
        }
    );
    assert_eq!(held(h, case).await, Some(*first));
}

/// An expired ceremony is no ceremony: a new proposal replaces it.
pub async fn an_expired_ceremony_is_replaced_by_the_next_proposal(h: &dyn Harness) {
    let case = "expired";
    let alice = owner(case, "alice");
    let created = provisioned(h, case, &alice).await;
    begin(h, case, &alice, quiescence(0x40, 0, 60), 0).await;

    // The window is `received_at + deadline`; at exactly the deadline it has closed.
    let replacement = quiescence(0x41, 60, 3_600);
    let outcome = begin(h, case, &alice, replacement.clone(), 60).await;
    let expected = AlbumRecord {
        upgrade: Some(replacement),
        ..created
    };
    assert_eq!(
        outcome,
        UpgradeOutcome::Quiescing(Box::new(expected.clone()))
    );
    assert_eq!(held(h, case).await, Some(expected));
}

/// An unknown album and somebody else's album answer alike, and neither is written.
pub async fn a_stranger_or_an_unknown_album_is_not_found_and_nothing_is_written(h: &dyn Harness) {
    let case = "stranger";
    let alice = owner(case, "alice");
    let mallory = owner(case, "mallory");
    let created = provisioned(h, case, &alice).await;

    assert_eq!(
        begin(h, case, &mallory, quiescence(0x50, 0, 3_600), 0).await,
        UpgradeOutcome::NotFound
    );
    assert_eq!(
        end(h, case, &mallory, 0x50, 0).await,
        UpgradeOutcome::NotFound
    );
    assert_eq!(held(h, case).await, Some(created));

    assert_eq!(
        begin(h, "stranger-none", &alice, quiescence(0x51, 0, 3_600), 0).await,
        UpgradeOutcome::NotFound
    );
    assert_eq!(
        end(h, "stranger-none", &alice, 0x51, 0).await,
        UpgradeOutcome::NotFound
    );
    assert_eq!(held(h, "stranger-none").await, None);
}

/// Somebody else's live ceremony is untouched by a stranger's attempt to end it.
pub async fn a_stranger_cannot_end_the_owners_ceremony(h: &dyn Harness) {
    let case = "stranger-end";
    let alice = owner(case, "alice");
    provisioned(h, case, &alice).await;
    let UpgradeOutcome::Quiescing(live) =
        begin(h, case, &alice, quiescence(0x58, 0, 3_600), 0).await
    else {
        panic!("quiescing");
    };
    assert_eq!(
        end(h, case, &owner(case, "mallory"), 0x58, 10).await,
        UpgradeOutcome::NotFound
    );
    assert_eq!(held(h, case).await, Some(*live));
}

/// Ending the live ceremony by its own id returns the album to normal operation.
pub async fn ending_the_live_ceremony_clears_it(h: &dyn Harness) {
    let case = "end";
    let alice = owner(case, "alice");
    let created = provisioned(h, case, &alice).await;
    begin(h, case, &alice, quiescence(0x60, 0, 3_600), 0).await;

    let outcome = end(h, case, &alice, 0x60, 10).await;
    assert_eq!(outcome, UpgradeOutcome::Cleared(Box::new(created.clone())));
    assert_eq!(held(h, case).await, Some(created));
}

/// Ending a ceremony on an album that is not quiescing is already done, not an error.
pub async fn ending_when_nothing_is_in_flight_is_cleared(h: &dyn Harness) {
    let case = "end-idle";
    let alice = owner(case, "alice");
    let created = provisioned(h, case, &alice).await;
    assert_eq!(
        end(h, case, &alice, 0x70, 0).await,
        UpgradeOutcome::Cleared(Box::new(created.clone()))
    );
    assert_eq!(held(h, case).await, Some(created));
}

/// Ending somebody else's live ceremony by a different id conflicts and leaves it in force.
pub async fn ending_a_different_live_ceremony_conflicts_and_changes_nothing(h: &dyn Harness) {
    let case = "end-other";
    let alice = owner(case, "alice");
    provisioned(h, case, &alice).await;
    let UpgradeOutcome::Quiescing(live) =
        begin(h, case, &alice, quiescence(0x80, 0, 3_600), 0).await
    else {
        panic!("quiescing");
    };
    assert_eq!(
        end(h, case, &alice, 0x81, 10).await,
        UpgradeOutcome::Conflict {
            intent_id: Uuid::from_u128(0x80)
        }
    );
    assert_eq!(held(h, case).await, Some(*live));
}

/// Once a ceremony's window has closed, any id clears it.
pub async fn an_expired_ceremony_is_cleared_whatever_id_ends_it(h: &dyn Harness) {
    let case = "end-expired";
    let alice = owner(case, "alice");
    let created = provisioned(h, case, &alice).await;
    begin(h, case, &alice, quiescence(0x90, 0, 60), 0).await;
    assert_eq!(
        end(h, case, &alice, 0x91, 61).await,
        UpgradeOutcome::Cleared(Box::new(created.clone()))
    );
    assert_eq!(held(h, case).await, Some(created));
}

/// A ceremony on one album is invisible to another.
pub async fn a_ceremony_does_not_leak_between_albums(h: &dyn Harness) {
    let case = "leak";
    let other = "leak-other";
    let alice = owner(case, "alice");
    provisioned(h, case, &alice).await;
    let untouched = provisioned(h, other, &alice).await;
    begin(h, case, &alice, quiescence(0xA0, 0, 3_600), 0).await;
    assert_eq!(held(h, other).await, Some(untouched));
    // And the second album takes a ceremony of its own without conflicting with the first.
    assert!(matches!(
        begin(h, other, &alice, quiescence(0xA1, 0, 3_600), 0).await,
        UpgradeOutcome::Quiescing(_)
    ));
}

// ===========================================================================================
// The whole suite
// ===========================================================================================

/// Run every case above against one harness, in order.
pub async fn run_all(h: &dyn Harness) {
    a_first_provision_creates_the_album_the_next_read_returns(h).await;
    an_unprovisioned_album_reads_as_none(h).await;
    re_provisioning_by_the_owner_answers_the_stored_record_and_writes_nothing(h).await;
    an_id_bound_elsewhere_is_not_available_and_changes_nothing(h).await;
    the_returned_instants_are_the_instants_the_next_read_produces(h).await;

    a_proposal_quiesces_the_album(h).await;
    a_replayed_proposal_answers_quiescing_again(h).await;
    a_second_live_ceremony_conflicts_and_changes_nothing(h).await;
    an_expired_ceremony_is_replaced_by_the_next_proposal(h).await;
    a_stranger_or_an_unknown_album_is_not_found_and_nothing_is_written(h).await;
    a_stranger_cannot_end_the_owners_ceremony(h).await;
    ending_the_live_ceremony_clears_it(h).await;
    ending_when_nothing_is_in_flight_is_cleared(h).await;
    ending_a_different_live_ceremony_conflicts_and_changes_nothing(h).await;
    an_expired_ceremony_is_cleared_whatever_id_ends_it(h).await;
    a_ceremony_does_not_leak_between_albums(h).await;
}

#[cfg(test)]
mod tests;
