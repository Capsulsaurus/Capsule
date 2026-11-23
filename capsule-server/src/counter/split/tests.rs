use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};

use super::*;
use crate::counter::InMemoryCounters;
use crate::store::UserId;

/// The whole list of process-local keys, pinned: adding one is a decision ADR-0015 has to make,
/// not a one-line edit nobody reviews.
#[test]
fn only_the_per_chunk_throughput_budgets_are_process_local() {
    let local = [
        CounterKey::UploadChunks(UserId::new("u1")),
        CounterKey::UploadChunksSource("203.0.113.7".to_owned()),
    ];
    for key in &local {
        assert!(is_process_local(key), "{} is counted locally", key.as_str());
    }
    let shared = [
        CounterKey::LoginAttempts(UserId::new("u1")),
        CounterKey::UploadSessions(UserId::new("u1")),
        CounterKey::UploadSessionsSource("203.0.113.7".to_owned()),
        CounterKey::SecondFactor("challenge".to_owned()),
        CounterKey::RegistrationSource("203.0.113.7".to_owned()),
        CounterKey::ShareLink("link".to_owned()),
        CounterKey::DropSource("203.0.113.7".to_owned()),
        CounterKey::FederatedReportNonce("nonce".to_owned()),
        CounterKey::PeerRequests("peer.example".to_owned()),
        CounterKey::OidcDevice,
        CounterKey::EnrollmentRedemptionMalformed,
    ];
    for key in &shared {
        assert!(
            !is_process_local(key),
            "{} is counted deployment-wide",
            key.as_str()
        );
    }
}

/// A hit lands in the store its key routes to, and only there.
#[tokio::test]
async fn a_hit_is_charged_to_the_store_its_key_routes_to() {
    let local: Arc<dyn CounterStore> = Arc::new(InMemoryCounters::new());
    let shared: Arc<dyn CounterStore> = Arc::new(InMemoryCounters::new());
    let split = SplitCounters::new(Arc::clone(&local), Arc::clone(&shared));
    let budget = Budget::new(1, SignedDuration::from_mins(1));
    let at = Timestamp::UNIX_EPOCH;

    let chunk = CounterKey::UploadChunks(UserId::new("u1"));
    let login = CounterKey::LoginAttempts(UserId::new("u1"));
    assert!(
        split
            .hit(&chunk, budget, at)
            .await
            .expect("answers")
            .admits()
    );
    assert!(
        split
            .hit(&login, budget, at)
            .await
            .expect("answers")
            .admits()
    );

    assert!(
        !local
            .peek(&chunk, budget, at)
            .await
            .expect("answers")
            .admits()
    );
    assert!(
        shared
            .peek(&chunk, budget, at)
            .await
            .expect("answers")
            .admits()
    );
    assert!(
        !shared
            .peek(&login, budget, at)
            .await
            .expect("answers")
            .admits()
    );
    assert!(
        local
            .peek(&login, budget, at)
            .await
            .expect("answers")
            .admits()
    );

    split.reset(&login).await.expect("resets");
    assert!(
        shared
            .peek(&login, budget, at)
            .await
            .expect("answers")
            .admits()
    );
}
