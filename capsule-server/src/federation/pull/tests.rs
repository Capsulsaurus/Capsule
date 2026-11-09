use super::*;

/// Jitter that always draws the same fraction of its bound: `0`, the top, or the middle.
#[derive(Debug)]
enum Fixed {
    Lowest,
    Highest,
    Middle,
}

impl Jitter for Fixed {
    fn below(&self, bound: u64) -> u64 {
        match self {
            Self::Lowest => 0,
            Self::Highest => bound.saturating_sub(1),
            Self::Middle => bound / 2,
        }
    }
}

fn start() -> Timestamp {
    Timestamp::from_second(1_800_000_000).expect("an instant")
}

fn after(span: SignedDuration) -> Timestamp {
    start().checked_add(span).expect("an instant")
}

#[test]
fn a_success_is_next_due_on_the_interval_within_twenty_percent() {
    assert_eq!(
        next_after_success(start(), &Fixed::Lowest),
        after(SignedDuration::from_mins(12)),
        "the earliest is 80% of fifteen minutes"
    );
    assert_eq!(
        next_after_success(start(), &Fixed::Highest),
        after(SignedDuration::from_mins(18)),
        "the latest is 120%"
    );
    assert_eq!(
        next_after_success(start(), &Fixed::Middle),
        after(PULL_INTERVAL),
        "the middle draw is the interval itself"
    );
}

#[test]
fn the_backoff_doubles_from_a_minute_to_a_six_hour_cap() {
    assert_eq!(
        backoff(0),
        SignedDuration::from_mins(1),
        "zero reads as one"
    );
    assert_eq!(backoff(1), SignedDuration::from_mins(1));
    assert_eq!(backoff(2), SignedDuration::from_mins(2));
    assert_eq!(backoff(3), SignedDuration::from_mins(4));
    assert_eq!(backoff(9), SignedDuration::from_mins(256));
    assert_eq!(backoff(10), BACKOFF_CAP, "512 minutes is past the cap");
    assert_eq!(backoff(64), BACKOFF_CAP, "a shift past the width saturates");
    assert_eq!(backoff(u32::MAX), BACKOFF_CAP);
}

#[test]
fn a_failure_is_next_due_anywhere_up_to_its_backoff() {
    assert_eq!(
        next_after_failure(start(), 3, &Fixed::Lowest),
        start(),
        "full jitter reaches zero"
    );
    assert_eq!(
        next_after_failure(start(), 3, &Fixed::Highest),
        after(SignedDuration::from_mins(4)),
        "and the whole back-off"
    );
    assert_eq!(
        next_after_failure(start(), 100, &Fixed::Highest),
        after(BACKOFF_CAP)
    );
}

#[test]
fn the_system_jitter_stays_below_its_bound() {
    let jitter = SystemJitter::new();
    assert_eq!(jitter.below(0), 0);
    assert_eq!(jitter.below(1), 0);
    for _ in 0..1_000 {
        assert!(jitter.below(7) < 7);
    }
}

#[test]
fn a_job_names_its_peer_and_album() {
    let job = JobKey::new(PeerId::new("Other.Test."), AlbumId::new("a1"));
    assert_eq!(job.to_string(), "other.test/a1");
    assert_eq!(format!("{job:?}"), "JobKey(other.test/a1)");
}

#[test]
fn the_lease_outlives_the_pull_deadline() {
    let deadline = SignedDuration::try_from(PULL_DEADLINE).expect("a span");
    assert!(
        deadline < PULL_LEASE,
        "a live holder must finish before its lease can lapse to another replica"
    );
}

#[tokio::test]
async fn the_in_memory_double_conforms() {
    conformance::run_all(&InMemoryPullJobs::new()).await;
}
