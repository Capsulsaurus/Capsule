//! The one suite every account adapter must pass.
//!
//! # Why the four ports share one suite
//!
//! [`AccountRegistry`], [`AccountDirectory`], [`AccountProfiles`] and [`PasswordChange`] are
//! four ports because they answer four questions with four *disclosure* contracts — registration
//! must say whether an address is taken, authentication must not — and their own module docs
//! argue at length for keeping them apart. They are not four stores. Every adapter that exists
//! or is planned implements all four over one account row, and most of the properties worth
//! asserting cross them: a password change is only interesting because the *directory* then
//! grants the new password and refuses the old one, and a lockout is only interesting because a
//! password change clears it.
//!
//! So the harness hands out all four, and a case reaches for the ones its property spans.
//!
//! # What this suite cannot assert, stated rather than faked
//!
//! **The timing-equalized miss.** [`AccountDirectory`]'s contract is that no caller can tell an
//! unknown account from a wrong password, and half of that is a *response time*. A timing
//! assertion is flaky by nature and would be the `S-C35` mistake in a new place — a suite
//! claiming to prove a property under conditions it cannot create. What is asserted instead is
//! the consequence a suite *can* see: both answers are the same value, and neither path is
//! distinguishable through the port. The work itself is asserted where it is decidable, in
//! `credential`'s own tests (`absorbing_a_miss_costs_what_a_verification_costs`), and structurally
//! by the fact that the miss path calls the same verifier.
//!
//! **Concurrency.** `create` is required to be one operation against two racing registrations,
//! and a single-process suite cannot exhibit that race. What it asserts is the observable
//! consequence — the second registration is refused and the first account's credential still
//! works — and the structural guarantee stays in the adapter, as one lock here and as a unique
//! index in Postgres.
//!
//! # Reusing a harness
//!
//! Every case scopes its addresses and ids to itself, so cases may share one harness and
//! [`run_all`] does. A case may move the harness clock forward and never moves it back.

use jiff::{SignedDuration, Timestamp};

use super::directory::{AccountDirectory, Authentication, DirectoryError};
use super::profile::{
    AccountProfiles, PasswordChange, PasswordChanged, ProfileRecord, ProfileUpdate,
};
use super::registry::{AccountRegistry, Registration};
use crate::store::{StoreFuture, UserId};

/// The four account ports under test, plus the two things a suite cannot do through them.
///
/// `lockout_attempts` is asked of the harness rather than read from a constant because the
/// threshold is a **deployment setting** (`LOCKOUT_MAX_ATTEMPTS`), so a harness built with a
/// tighter one must still pass — and a suite that hardcoded ten would silently stop testing the
/// ceiling the moment a deployment moved it.
pub trait Harness: Send + Sync {
    /// Where accounts are created (`S-C53`).
    fn registry(&self) -> &dyn AccountRegistry;
    /// Who exists, and whether a presented password is theirs.
    fn directory(&self) -> &dyn AccountDirectory;
    /// The facts an account keeps about itself (`S-C54`).
    fn profiles(&self) -> &dyn AccountProfiles;
    /// Where a password is replaced (`S-C54`).
    fn passwords(&self) -> &dyn PasswordChange;

    /// How many consecutive failures this harness locks an account out after.
    fn lockout_attempts(&self) -> u32;
    /// How long that lockout lasts.
    fn lockout_window(&self) -> SignedDuration;

    /// Move the harness `by` forward in its own time.
    ///
    /// The seam that keeps the lockout cases backend-agnostic: the deterministic double advances
    /// a manual clock, and a container-backed harness rebuilds its adapter over a clock it drives.
    fn advance(&self, by: SignedDuration) -> StoreFuture<'_, ()>;
}

/// Unwrap a directory result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, DirectoryError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming adapter must succeed at {doing}: {error}"),
    }
}

/// Unwrap an expected-present value.
#[track_caller]
fn present<T>(value: Option<T>, what: &str) -> T {
    match value {
        Some(value) => value,
        None => panic!("{what} must be present"),
    }
}

/// The password every case registers with unless it is about passwords.
const PASSWORD: &str = "correct horse battery staple";

/// `case`'s own address, so cases may share a harness.
fn email(case: &str) -> String {
    format!("{case}@example.test")
}

/// `case`'s own account id.
///
/// A UUID-shaped string rather than a bare word, because that is what `new_user_id` mints and
/// an adapter storing it in a typed column would notice the difference.
fn user(case: &str, n: u8) -> UserId {
    UserId::new(format!(
        "018f3f1e-4b7a-7c9d-8e2f-{:012x}",
        u64::from(n) + hash(case)
    ))
}

/// A small stable spread so two cases' ids do not collide.
fn hash(case: &str) -> u64 {
    case.bytes().fold(1_u64, |held, byte| {
        held.wrapping_mul(131).wrapping_add(u64::from(byte)) & 0x0000_ffff_ffff
    }) * 256
}

/// Register `case`'s account and return its id.
async fn seed(h: &dyn Harness, case: &str) -> (String, UserId) {
    let address = email(case);
    let id = user(case, 0);
    assert_eq!(
        ok(
            h.registry()
                .create(&address, PASSWORD, &id, Timestamp::UNIX_EPOCH)
                .await,
            "create an account",
        ),
        Registration::Created(id.clone()),
    );
    (address, id)
}

/// Present a wrong password `times` times against `address`.
async fn fail(h: &dyn Harness, address: &str, times: u32) {
    for _ in 0..times {
        let _ = h.directory().authenticate(address, "wrong").await;
    }
}

// ===========================================================================================
// Registration and sign-in
// ===========================================================================================

/// The whole point of an account adapter: register, then sign in to what was registered.
pub async fn registering_then_signing_in_works(h: &dyn Harness) {
    let (address, id) = seed(h, "signin").await;
    assert_eq!(
        ok(
            h.directory().authenticate(&address, PASSWORD).await,
            "authenticate",
        ),
        Authentication::Granted(id),
    );
}

/// A taken address is reported, and the account behind it is untouched.
///
/// The one disclosure this surface makes, and the alternative — answering success and creating
/// nothing — is worse in every way, because a client that believed it would then fail to sign in
/// with no explanation.
pub async fn a_taken_address_is_reported_and_nothing_is_written(h: &dyn Harness) {
    let (address, first) = seed(h, "taken").await;
    let second = user("taken", 1);
    assert_eq!(
        ok(
            h.registry()
                .create(
                    &address,
                    "a different password entirely",
                    &second,
                    Timestamp::UNIX_EPOCH,
                )
                .await,
            "create a second account for one address",
        ),
        Registration::AlreadyExists,
    );
    // The first account's credential still works, so nothing was overwritten — which is what
    // makes this a refusal rather than a silent takeover of somebody's address.
    assert_eq!(
        ok(
            h.directory().authenticate(&address, PASSWORD).await,
            "authenticate",
        ),
        Authentication::Granted(first),
    );
}

/// An unknown address and a wrong password are one answer.
///
/// The port collapses them into one value on purpose, so no caller *can* tell them apart and no
/// future caller can reintroduce an enumeration oracle by branching on something the port does
/// not offer.
pub async fn an_unknown_address_and_a_wrong_password_are_one_answer(h: &dyn Harness) {
    let (address, _) = seed(h, "oracle").await;
    assert_eq!(
        ok(
            h.directory()
                .authenticate("nobody-oracle@example.test", PASSWORD)
                .await,
            "authenticate an unknown address",
        ),
        Authentication::Refused,
    );
    assert_eq!(
        ok(
            h.directory()
                .authenticate(&address, "the wrong password")
                .await,
            "authenticate with a wrong password",
        ),
        Authentication::Refused,
    );
}

/// Addresses are compared verbatim.
///
/// Case folding is a normalization policy no port describes, and a policy invented by one
/// adapter is a policy the others have to guess at — so `Foo@example.test` and
/// `foo@example.test` are two accounts until a slice says otherwise. Asserted rather than left
/// implicit because it is exactly the kind of thing a `citext` column or a `lower()` index
/// changes without anyone deciding to.
pub async fn addresses_are_compared_verbatim(h: &dyn Harness) {
    let (address, _) = seed(h, "verbatim").await;
    assert_eq!(
        ok(
            h.directory()
                .authenticate(&address.to_uppercase(), PASSWORD)
                .await,
            "authenticate a differently-cased address",
        ),
        Authentication::Refused,
    );
}

/// Re-authentication takes the account from the credential and never from the request.
///
/// An operation that took an address it was handed would be usable to test another account's
/// password from inside any authenticated session.
pub async fn re_authentication_takes_the_account_from_the_credential(h: &dyn Harness) {
    let (_, id) = seed(h, "reauth").await;
    assert_eq!(
        ok(
            h.directory().authenticate_user(&id, PASSWORD).await,
            "re-authenticate",
        ),
        Authentication::Granted(id.clone()),
    );
    let stranger = user("reauth", 9);
    assert_eq!(
        ok(
            h.directory().authenticate_user(&stranger, PASSWORD).await,
            "re-authenticate as a stranger",
        ),
        Authentication::Refused,
    );
}

// ===========================================================================================
// The lockout
// ===========================================================================================

/// Enough failures lock the account, and a correct password is told so.
///
/// `Locked` is the one refusal a *correct* password also receives, which is why it is a separate
/// value: a client showing "wrong password" here would send somebody round a loop that cannot
/// succeed.
pub async fn enough_failures_lock_the_account_and_a_correct_password_is_told_so(h: &dyn Harness) {
    let (address, _) = seed(h, "lockout").await;
    fail(h, &address, h.lockout_attempts()).await;
    assert_eq!(
        ok(
            h.directory().authenticate(&address, PASSWORD).await,
            "authenticate a locked account",
        ),
        Authentication::Locked,
    );
}

/// A success before the ceiling clears the count.
pub async fn a_success_before_the_ceiling_clears_the_count(h: &dyn Harness) {
    let (address, id) = seed(h, "clears").await;
    fail(h, &address, h.lockout_attempts() - 1).await;
    assert_eq!(
        ok(
            h.directory().authenticate(&address, PASSWORD).await,
            "authenticate",
        ),
        Authentication::Granted(id.clone()),
    );
    // Back to zero: the next wrong password does not tip an already-full counter over.
    fail(h, &address, 1).await;
    assert_eq!(
        ok(
            h.directory().authenticate(&address, PASSWORD).await,
            "authenticate",
        ),
        Authentication::Granted(id),
    );
}

/// The lockout decays, because nothing else in this server can clear it.
///
/// There is no unlock operation on any surface, and `login`, `reauthenticate` and `password` all
/// refuse on `Locked` before they verify anything — so without a decay a lockout is a
/// permanently lost account rather than a throttle. The boundary is asserted from both sides
/// because an off-by-one here is a lockout that never engages.
pub async fn a_lockout_decays_because_nothing_else_can_clear_it(h: &dyn Harness) {
    let (address, id) = seed(h, "decay").await;
    fail(h, &address, h.lockout_attempts()).await;
    assert_eq!(
        ok(
            h.directory().authenticate(&address, PASSWORD).await,
            "authenticate a locked account",
        ),
        Authentication::Locked,
    );

    ok_store(
        h.advance(h.lockout_window() - SignedDuration::from_secs(1))
            .await,
    );
    assert_eq!(
        ok(
            h.directory().authenticate(&address, PASSWORD).await,
            "authenticate one second inside the window",
        ),
        Authentication::Locked,
    );

    ok_store(h.advance(SignedDuration::from_secs(1)).await);
    assert_eq!(
        ok(
            h.directory().authenticate(&address, PASSWORD).await,
            "authenticate past the window",
        ),
        Authentication::Granted(id),
    );
}

/// Attempts made during a lockout do not extend it.
///
/// Deliberate, and the direction is not obvious. Extending the window on every attempt would
/// hand anybody who can reach the endpoint a way to keep somebody else's account locked forever
/// by hammering it — a denial of service on an account rather than a defence of it. So the
/// window runs from the last **counted** failure.
pub async fn attempts_during_a_lockout_do_not_extend_it(h: &dyn Harness) {
    let (address, id) = seed(h, "hammer").await;
    fail(h, &address, h.lockout_attempts()).await;
    // Hammer it three times, each a little later, all inside the window.
    let step = h.lockout_window() / 8;
    for _ in 0..3 {
        ok_store(h.advance(step).await);
        fail(h, &address, 1).await;
    }
    assert_eq!(
        ok(
            h.directory().authenticate(&address, PASSWORD).await,
            "authenticate inside the window",
        ),
        Authentication::Locked,
    );
    ok_store(h.advance(h.lockout_window()).await);
    assert_eq!(
        ok(
            h.directory().authenticate(&address, PASSWORD).await,
            "authenticate past the window",
        ),
        Authentication::Granted(id),
        "hammering a locked account must not move the deadline",
    );
}

/// Failures spread wider than the window never accumulate.
///
/// Ten mistypes over a year is not a guessing run, and counting them as one would lock an
/// account on a tenth attempt made months after the ninth.
pub async fn failures_spread_wider_than_the_window_never_accumulate(h: &dyn Harness) {
    let (address, id) = seed(h, "spread").await;
    for _ in 0..h.lockout_attempts() * 2 {
        fail(h, &address, 1).await;
        ok_store(
            h.advance(h.lockout_window() + SignedDuration::from_secs(1))
                .await,
        );
    }
    assert_eq!(
        ok(
            h.directory().authenticate(&address, PASSWORD).await,
            "authenticate",
        ),
        Authentication::Granted(id),
    );
}

// ===========================================================================================
// Password change
// ===========================================================================================

/// A password change replaces the credential, in both directions.
pub async fn a_password_change_grants_the_new_password_and_refuses_the_old(h: &dyn Harness) {
    let (address, id) = seed(h, "rotate").await;
    assert_eq!(
        ok(
            h.passwords()
                .set_password(&id, "a brand new password", Timestamp::UNIX_EPOCH)
                .await,
            "replace a password",
        ),
        PasswordChanged::Yes,
    );
    assert_eq!(
        ok(
            h.directory()
                .authenticate(&address, "a brand new password")
                .await,
            "authenticate with the new password",
        ),
        Authentication::Granted(id),
    );
    assert_eq!(
        ok(
            h.directory().authenticate(&address, PASSWORD).await,
            "authenticate with the old password",
        ),
        Authentication::Refused,
    );
}

/// A password change clears a lockout.
///
/// The port requires it: a change is a successful credential presentation, and leaving the
/// failure count behind would bar somebody from an account they just proved they own.
pub async fn a_password_change_clears_a_lockout(h: &dyn Harness) {
    let (address, id) = seed(h, "unlock").await;
    fail(h, &address, h.lockout_attempts()).await;
    assert_eq!(
        ok(
            h.passwords()
                .set_password(&id, "a brand new password", Timestamp::UNIX_EPOCH)
                .await,
            "replace a password",
        ),
        PasswordChanged::Yes,
    );
    assert_eq!(
        ok(
            h.directory()
                .authenticate(&address, "a brand new password")
                .await,
            "authenticate after the change",
        ),
        Authentication::Granted(id),
    );
}

/// Changing the password of an account that does not exist writes nothing.
pub async fn changing_a_password_for_an_absent_account_writes_nothing(h: &dyn Harness) {
    let stranger = user("absent", 7);
    assert_eq!(
        ok(
            h.passwords()
                .set_password(&stranger, "irrelevant", Timestamp::UNIX_EPOCH)
                .await,
            "replace a stranger's password",
        ),
        PasswordChanged::NoSuchAccount,
    );
    assert!(
        ok(h.profiles().read(&stranger).await, "read a profile").is_none(),
        "a password change must not create the account it could not find"
    );
}

// ===========================================================================================
// Profiles
// ===========================================================================================

/// A profile reads back what registration wrote, and takes an edit.
pub async fn a_profile_reads_back_what_registration_wrote_and_takes_an_edit(h: &dyn Harness) {
    let (address, id) = seed(h, "profile").await;
    let profile: ProfileRecord = present(
        ok(h.profiles().read(&id).await, "read a profile"),
        "the account's profile",
    );
    assert_eq!(profile.user_id, id);
    assert_eq!(profile.email, address);
    assert_eq!(profile.display_name, None);
    assert_eq!(profile.created_at, Timestamp::UNIX_EPOCH);

    let updated = present(
        ok(
            h.profiles()
                .update(
                    &id,
                    &ProfileUpdate {
                        display_name: Some(Some("Ada Lovelace".to_owned())),
                    },
                )
                .await,
            "update a profile",
        ),
        "the updated profile",
    );
    assert_eq!(updated.display_name.as_deref(), Some("Ada Lovelace"));
}

/// An absent field leaves the name alone; `Some(None)` clears it.
///
/// The whole reason the field is a nested option: a flat one cannot express "leave it alone", so
/// every partial update would clear the name nobody mentioned.
pub async fn an_absent_field_and_a_cleared_one_are_different_updates(h: &dyn Harness) {
    let (_, id) = seed(h, "nested").await;
    let named = present(
        ok(
            h.profiles()
                .update(
                    &id,
                    &ProfileUpdate {
                        display_name: Some(Some("Grace Hopper".to_owned())),
                    },
                )
                .await,
            "set a display name",
        ),
        "the updated profile",
    );
    assert_eq!(named.display_name.as_deref(), Some("Grace Hopper"));

    let untouched = present(
        ok(
            h.profiles().update(&id, &ProfileUpdate::default()).await,
            "apply an empty update",
        ),
        "the profile",
    );
    assert_eq!(untouched.display_name.as_deref(), Some("Grace Hopper"));

    let cleared = present(
        ok(
            h.profiles()
                .update(
                    &id,
                    &ProfileUpdate {
                        display_name: Some(None),
                    },
                )
                .await,
            "clear a display name",
        ),
        "the profile",
    );
    assert_eq!(cleared.display_name, None);
}

/// A profile for an account that does not exist is absent, not an error.
///
/// Reachable with a perfectly valid credential: a session outlives the account row it names if
/// the account is deleted while a token is live. The route answers `404` rather than `500`,
/// because the server is working correctly and the account is gone.
pub async fn a_profile_for_an_absent_account_is_absent_and_not_an_error(h: &dyn Harness) {
    let stranger = user("ghost", 5);
    assert!(
        ok(
            h.profiles().read(&stranger).await,
            "read a stranger's profile"
        )
        .is_none(),
    );
    assert!(
        ok(
            h.profiles()
                .update(&stranger, &ProfileUpdate::default())
                .await,
            "update a stranger's profile",
        )
        .is_none(),
    );
}

/// Unwrap the harness's own seam, which speaks `StoreError` rather than `DirectoryError`.
#[track_caller]
fn ok_store(result: Result<(), crate::store::StoreError>) {
    if let Err(error) = result {
        panic!("a conforming harness must be able to move its own clock: {error}");
    }
}

// ===========================================================================================
// The whole suite
// ===========================================================================================

/// Run every case above against one harness, in order.
pub async fn run_all(h: &dyn Harness) {
    registering_then_signing_in_works(h).await;
    a_taken_address_is_reported_and_nothing_is_written(h).await;
    an_unknown_address_and_a_wrong_password_are_one_answer(h).await;
    addresses_are_compared_verbatim(h).await;
    re_authentication_takes_the_account_from_the_credential(h).await;

    enough_failures_lock_the_account_and_a_correct_password_is_told_so(h).await;
    a_success_before_the_ceiling_clears_the_count(h).await;
    a_lockout_decays_because_nothing_else_can_clear_it(h).await;
    attempts_during_a_lockout_do_not_extend_it(h).await;
    failures_spread_wider_than_the_window_never_accumulate(h).await;

    a_password_change_grants_the_new_password_and_refuses_the_old(h).await;
    a_password_change_clears_a_lockout(h).await;
    changing_a_password_for_an_absent_account_writes_nothing(h).await;

    a_profile_reads_back_what_registration_wrote_and_takes_an_edit(h).await;
    an_absent_field_and_a_cleared_one_are_different_updates(h).await;
    a_profile_for_an_absent_account_is_absent_and_not_an_error(h).await;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jiff::SignedDuration;

    use super::{Harness, run_all};
    use crate::auth::accounts_memory::{InMemoryAccounts, MAX_FAILED_ATTEMPTS};
    use crate::auth::credential::Credentials;
    use crate::auth::directory::AccountDirectory;
    use crate::auth::profile::{AccountProfiles, PasswordChange};
    use crate::auth::registry::AccountRegistry;
    use crate::store::StoreFuture;
    use crate::store::memory::ManualClock;

    /// A fifteen-minute lockout window, as a deployment gets by default.
    const WINDOW: SignedDuration = SignedDuration::from_mins(15);

    /// The deterministic double, on a clock the suite drives.
    #[derive(Debug)]
    struct MemoryHarness {
        accounts: InMemoryAccounts,
        clock: Arc<ManualClock>,
    }

    impl Harness for MemoryHarness {
        fn registry(&self) -> &dyn AccountRegistry {
            &self.accounts
        }

        fn directory(&self) -> &dyn AccountDirectory {
            &self.accounts
        }

        fn profiles(&self) -> &dyn AccountProfiles {
            &self.accounts
        }

        fn passwords(&self) -> &dyn PasswordChange {
            &self.accounts
        }

        fn lockout_attempts(&self) -> u32 {
            MAX_FAILED_ATTEMPTS
        }

        fn lockout_window(&self) -> SignedDuration {
            WINDOW
        }

        fn advance(&self, by: SignedDuration) -> StoreFuture<'_, ()> {
            Box::pin(async move {
                self.clock.advance(by);
                Ok(())
            })
        }
    }

    fn harness() -> MemoryHarness {
        let clock = Arc::new(ManualClock::default());
        MemoryHarness {
            accounts: InMemoryAccounts::new(
                Credentials::new().expect("the platform hashes"),
                clock.clone(),
                MAX_FAILED_ATTEMPTS,
                WINDOW,
            ),
            clock,
        }
    }

    /// The development profile's adapter is only a legitimate stand-in for Postgres to the
    /// extent it passes this.
    ///
    /// One test rather than one per case, unlike [`crate::store::conformance`]'s: every case here
    /// registers an account, which costs an Argon2id hash at the server-side parameter set, and
    /// sixteen fresh harnesses would pay for sixteen decoys on top. The suite shares one harness
    /// and every case scopes its own addresses, which is what makes that safe.
    #[tokio::test]
    async fn the_in_memory_account_store_conforms() {
        run_all(&harness()).await;
    }
}
