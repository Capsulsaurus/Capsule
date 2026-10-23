//! The one suite every [`DeviceDirectoryStore`] adapter must pass.
//!
//! # The rules the suite exists to protect
//!
//! - **Invariant 23.** A publish whose version does not strictly exceed the stored one is
//!   [`PublishOutcome::Stale`] carrying the stored version, and changes nothing.
//! - **The anchor is set once.** The first publish's identity key is the account's for
//!   good; a later publish under any other key is [`PublishOutcome::IdentityMismatch`] and
//!   changes nothing — and that refusal wins over staleness, so a wrong key is never told which
//!   of the two checks it failed.
//! - **Verbatim bytes.** What is fetched is exactly what was published.
//! - **One account's directory is nobody else's.** Anchors and versions are per account.
//!
//! The atomicity of the comparison and the write cannot be exhibited by a single-process suite;
//! it is the adapter's structural guarantee (one mutex in memory, one transaction lock in
//! Postgres), and the suite asserts its consequence — a refusal leaves the stored row as it was.
//!
//! # Reusing a harness
//!
//! Every case scopes its own account ids, so cases may share one store and [`run_all`] does.

use jiff::{SignedDuration, Timestamp};

use super::{DeviceDirectoryStore, PublishOutcome, PublishedDirectory};
use crate::store::{StoreError, UserId};

/// The store under test.
pub trait Harness: Send + Sync {
    /// The device-directory store under test.
    fn directories(&self) -> &dyn DeviceDirectoryStore;
}

/// Unwrap a store result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, StoreError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => {
            panic!("a conforming device-directory store must succeed at {doing}: {error}")
        }
    }
}

/// `case`'s account.
fn user(case: &str) -> UserId {
    UserId::new(format!("{case}-user"))
}

/// A directory for `case` at `version`, under identity key `key`.
///
/// The store never decodes the document or the key — verification happens before the port is
/// reached — so both are arbitrary bytes that differ per version and per key.
fn directory(case: &str, version: u64, key: u8) -> PublishedDirectory {
    PublishedDirectory {
        user_id: user(case),
        directory_version: version,
        document: format!("{case}/v{version}/k{key}").into_bytes(),
        identity_key: vec![key; 32],
        published_at: Timestamp::UNIX_EPOCH
            + SignedDuration::from_secs(1_700_000_000 + i64::from(key) * 1_000)
            + SignedDuration::from_secs(i64::try_from(version).expect("a small version")),
    }
}

/// Publish `record`.
async fn publish(h: &dyn Harness, record: PublishedDirectory) -> PublishOutcome {
    ok(
        h.directories().publish(record).await,
        "publish a device directory",
    )
}

/// What `case`'s account currently has published.
async fn fetched(h: &dyn Harness, case: &str) -> Option<PublishedDirectory> {
    ok(
        h.directories().fetch(&user(case)).await,
        "fetch a device directory",
    )
}

/// The first publish is accepted and served back exactly.
pub async fn a_first_publish_is_accepted_and_served_back_verbatim(h: &dyn Harness) {
    let case = "first";
    let first = directory(case, 1, 1);
    assert_eq!(
        publish(h, first.clone()).await,
        PublishOutcome::Published {
            directory_version: 1
        }
    );
    assert_eq!(fetched(h, case).await, Some(first));
}

/// An account that never published fetches nothing.
pub async fn an_account_that_never_published_fetches_none(h: &dyn Harness) {
    assert_eq!(fetched(h, "never").await, None);
}

/// A strictly greater version under the anchor replaces the directory, however far it jumps.
pub async fn an_advancing_version_replaces_the_directory(h: &dyn Harness) {
    let case = "advance";
    publish(h, directory(case, 1, 1)).await;
    let second = directory(case, 2, 1);
    assert_eq!(
        publish(h, second.clone()).await,
        PublishOutcome::Published {
            directory_version: 2
        }
    );
    assert_eq!(fetched(h, case).await, Some(second));

    let leap = directory(case, 1_000, 1);
    assert_eq!(
        publish(h, leap.clone()).await,
        PublishOutcome::Published {
            directory_version: 1_000
        }
    );
    assert_eq!(fetched(h, case).await, Some(leap));
}

/// An equal or lower version is stale, names the stored version, and changes nothing.
pub async fn a_version_that_does_not_advance_is_stale_and_changes_nothing(h: &dyn Harness) {
    let case = "stale";
    publish(h, directory(case, 1, 1)).await;
    let held = directory(case, 5, 1);
    publish(h, held.clone()).await;

    for version in [5, 4, 1] {
        let mut rollback = directory(case, version, 1);
        rollback.document = b"a rollback attempt".to_vec();
        assert_eq!(
            publish(h, rollback).await,
            PublishOutcome::Stale { stored: 5 },
            "version {version} does not strictly exceed 5"
        );
    }
    assert_eq!(fetched(h, case).await, Some(held));
}

/// A publish under a key that is not the anchor is refused and changes nothing.
pub async fn a_different_key_is_an_identity_mismatch_and_changes_nothing(h: &dyn Harness) {
    let case = "mismatch";
    let anchored = directory(case, 1, 1);
    publish(h, anchored.clone()).await;
    assert_eq!(
        publish(h, directory(case, 2, 2)).await,
        PublishOutcome::IdentityMismatch
    );
    assert_eq!(fetched(h, case).await, Some(anchored));
}

/// A wrong key is refused as a mismatch even when its version is also stale.
pub async fn a_wrong_key_with_a_stale_version_is_refused_as_a_mismatch(h: &dyn Harness) {
    let case = "precedence";
    let held = directory(case, 3, 1);
    publish(h, held.clone()).await;
    assert_eq!(
        publish(h, directory(case, 1, 2)).await,
        PublishOutcome::IdentityMismatch,
        "the anchor is checked first, so a wrong key learns nothing about the stored version"
    );
    assert_eq!(fetched(h, case).await, Some(held));
}

/// The anchor survives later publishes: it is the first key, not the latest accepted one.
pub async fn the_anchor_is_the_first_publishes_key_for_good(h: &dyn Harness) {
    let case = "anchor";
    publish(h, directory(case, 1, 1)).await;
    publish(h, directory(case, 2, 1)).await;
    publish(h, directory(case, 3, 1)).await;
    assert_eq!(
        publish(h, directory(case, 4, 9)).await,
        PublishOutcome::IdentityMismatch
    );
    let held = fetched(h, case).await.expect("published");
    assert_eq!(held.identity_key, vec![1; 32]);
    assert_eq!(held.directory_version, 3);
}

/// Each account anchors its own key and keeps its own version.
pub async fn a_directory_is_scoped_to_its_account(h: &dyn Harness) {
    let case = "scoped";
    let other = "scoped-other";
    publish(h, directory(case, 7, 1)).await;
    let theirs = directory(other, 1, 2);
    assert_eq!(
        publish(h, theirs.clone()).await,
        PublishOutcome::Published {
            directory_version: 1
        },
        "another account's anchor and version are not this one's"
    );
    assert_eq!(fetched(h, other).await, Some(theirs));
    assert_eq!(
        fetched(h, case).await.expect("published").directory_version,
        7
    );
}

/// The publication instant reads back to the microsecond, and reads back the same every time.
pub async fn the_publication_instant_reads_back_to_the_microsecond(h: &dyn Harness) {
    let case = "precise";
    let mut precise = directory(case, 1, 1);
    precise.published_at =
        Timestamp::from_nanosecond(1_700_000_000_123_456_789).expect("an instant");
    publish(h, precise.clone()).await;
    let first = fetched(h, case).await.expect("published");
    assert_eq!(
        first.published_at.as_microsecond(),
        precise.published_at.as_microsecond()
    );
    assert_eq!(fetched(h, case).await, Some(first));
}

/// Run every case above against one harness, in order.
pub async fn run_all(h: &dyn Harness) {
    a_first_publish_is_accepted_and_served_back_verbatim(h).await;
    an_account_that_never_published_fetches_none(h).await;
    an_advancing_version_replaces_the_directory(h).await;
    a_version_that_does_not_advance_is_stale_and_changes_nothing(h).await;
    a_different_key_is_an_identity_mismatch_and_changes_nothing(h).await;
    a_wrong_key_with_a_stale_version_is_refused_as_a_mismatch(h).await;
    the_anchor_is_the_first_publishes_key_for_good(h).await;
    a_directory_is_scoped_to_its_account(h).await;
    the_publication_instant_reads_back_to_the_microsecond(h).await;
}

#[cfg(test)]
mod tests;
