use super::*;

/// Hands every case a fresh double bounded as the suite asks.
struct Fresh;

impl conformance::Harness for Fresh {
    fn empty(&self) -> StoreFuture<'_, Box<dyn RejectedHashes>> {
        Box::pin(async { Ok(Box::new(InMemoryRejectedHashes::new(conformance::BOUNDS)) as _) })
    }
}

#[tokio::test]
async fn the_in_memory_table_conforms() {
    conformance::run_all(&Fresh).await;
}

#[test]
fn the_default_bounds_are_the_documented_ones() {
    let bounds = RejectedHashBounds::default();
    assert_eq!(bounds.capacity(), 100_000);
    assert_eq!(bounds.ttl(), SignedDuration::from_hours(90 * 24));
}

#[test]
fn the_horizon_saturates_at_the_start_of_time() {
    let bounds = RejectedHashBounds::default();
    assert_eq!(bounds.expired_at_or_before(Timestamp::MIN), Timestamp::MIN);
    let now: Timestamp = "2026-06-01T00:00:00Z".parse().expect("an instant");
    assert_eq!(bounds.expired_at_or_before(now), now - bounds.ttl());
}

#[tokio::test]
async fn the_double_holds_no_more_than_its_capacity_however_many_are_recorded() {
    let table = InMemoryRejectedHashes::new(conformance::BOUNDS);
    let start: Timestamp = "2026-06-01T00:00:00Z".parse().expect("an instant");
    for n in 0..=200_u8 {
        table
            .record(
                Rejection {
                    hash: Hash32([n; 32]),
                    peer: PeerId::new("home.test"),
                    album: AlbumId::new("album"),
                    asset_id: "asset".to_owned(),
                    rule: "invariant 17".to_owned(),
                },
                start + SignedDuration::from_secs(i64::from(n)),
            )
            .await
            .expect("recorded");
        assert!(table.len() <= 3, "{} rows held", table.len());
    }
    assert_eq!(table.len(), 3);
    assert!(!table.is_empty());
}

#[tokio::test]
async fn instants_are_kept_to_the_microsecond() {
    let table = InMemoryRejectedHashes::new(conformance::BOUNDS);
    let at = Timestamp::new(1_780_000_000, 123_456_789).expect("an instant");
    let rejection = Rejection {
        hash: Hash32([1; 32]),
        peer: PeerId::new("home.test"),
        album: AlbumId::new("album"),
        asset_id: "asset".to_owned(),
        rule: "invariant 17".to_owned(),
    };
    table.record(rejection, at).await.expect("recorded");
    let row = table
        .lookup(&Hash32([1; 32]), at)
        .await
        .expect("looked up")
        .expect("held");
    assert_eq!(row.first_rejected_at.subsec_nanosecond(), 123_456_000);
}
