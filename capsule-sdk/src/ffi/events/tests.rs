use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

const BLOB: &str = "ab";

/// A clock that reads 10 ms more each time it is asked.
fn stepping_clock() -> Elapsed {
    let reads = Arc::new(AtomicU64::new(0));
    Arc::new(move || Duration::from_millis(10 * (reads.fetch_add(1, Ordering::SeqCst) + 1)))
}

fn sent(offset: u64) -> TransferEvent {
    TransferEvent::ChunkSent {
        blob: BLOB.into(),
        session_id: "s".into(),
        offset,
        len: 4,
    }
}

/// Events arrive in publication order, each stamped with the clock read when it arrived.
#[tokio::test]
async fn a_subscription_stamps_events_in_order() {
    let events = TransferEvents::channel(8);
    let subscription = FfiTransferSubscription::with_clock(&events, stepping_clock());
    events.emit(|| sent(0));
    events.emit(|| sent(4));

    let first = subscription.next().await;
    let second = subscription.next().await;

    assert_eq!(
        first,
        Some(FfiTransferFeedItem::Event {
            stamped: FfiStampedTransferEvent {
                at_ms: 10,
                event: FfiTransferEvent::ChunkSent {
                    blob: BLOB.into(),
                    session_id: "s".into(),
                    offset: 0,
                    len: 4,
                },
            },
        })
    );
    let Some(FfiTransferFeedItem::Event { stamped }) = second else {
        panic!("expected the second event, got {second:?}");
    };
    assert_eq!(stamped.at_ms, 20);
    assert!(matches!(
        stamped.event,
        FfiTransferEvent::ChunkSent { offset: 4, .. }
    ));
}

/// Only events published after the subscription opened reach it.
#[tokio::test]
async fn a_subscription_sees_nothing_published_before_it_opened() {
    let events = TransferEvents::channel(8);
    let early = events.subscribe();
    events.emit(|| sent(0));
    drop(early);
    let subscription = FfiTransferSubscription::with_clock(&events, stepping_clock());
    events.emit(|| sent(4));

    let item = subscription.next().await;
    assert!(matches!(
        item,
        Some(FfiTransferFeedItem::Event {
            stamped: FfiStampedTransferEvent {
                event: FfiTransferEvent::ChunkSent { offset: 4, .. },
                ..
            },
        })
    ));
}

/// A subscriber that fell behind reads how many events it missed, then carries on from the
/// oldest one still held.
#[tokio::test]
async fn a_lagging_subscription_reports_what_it_missed() {
    let events = TransferEvents::channel(2);
    let subscription = FfiTransferSubscription::with_clock(&events, stepping_clock());
    for offset in 0..5 {
        events.emit(|| sent(offset * 4));
    }

    assert_eq!(
        subscription.next().await,
        Some(FfiTransferFeedItem::Lagged { missed: 3 })
    );
    let next = subscription.next().await;
    assert!(matches!(
        next,
        Some(FfiTransferFeedItem::Event {
            stamped: FfiStampedTransferEvent {
                event: FfiTransferEvent::ChunkSent { offset: 12, .. },
                ..
            },
        })
    ));
}

/// Once every publisher is gone the subscription ends, and stays ended.
#[tokio::test]
async fn a_subscription_ends_when_its_publisher_is_dropped() {
    let events = TransferEvents::channel(4);
    let subscription = FfiTransferSubscription::open(&events);
    drop(events);

    assert_eq!(subscription.next().await, None);
    assert_eq!(subscription.next().await, None);
}

/// A disabled handle has nothing to subscribe to: the subscription is over at once.
#[tokio::test]
async fn a_subscription_to_a_disabled_handle_is_empty() {
    let subscription = FfiTransferSubscription::open(&TransferEvents::disabled());
    assert_eq!(subscription.next().await, None);
}

/// Every event and retry reason maps field for field.
#[test]
fn every_event_maps_field_for_field() {
    let cases = [
        (
            TransferEvent::SessionCreated {
                blob: BLOB.into(),
                session_id: "s".into(),
                size: 9,
                resume_offset: 3,
            },
            FfiTransferEvent::SessionCreated {
                blob: BLOB.into(),
                session_id: "s".into(),
                size: 9,
                resume_offset: 3,
            },
        ),
        (
            TransferEvent::SessionResumed {
                blob: BLOB.into(),
                session_id: "s".into(),
                size: 9,
                offset: 4,
            },
            FfiTransferEvent::SessionResumed {
                blob: BLOB.into(),
                session_id: "s".into(),
                size: 9,
                offset: 4,
            },
        ),
        (
            TransferEvent::ChunkAcked {
                blob: BLOB.into(),
                session_id: "s".into(),
                offset: 0,
                len: 4,
                acked_offset: 3,
            },
            FfiTransferEvent::ChunkAcked {
                blob: BLOB.into(),
                session_id: "s".into(),
                offset: 0,
                len: 4,
                acked_offset: 3,
            },
        ),
        (
            TransferEvent::ChunkRetried {
                blob: BLOB.into(),
                session_id: "s".into(),
                offset: 0,
                len: 4,
                reason: RetryReason::ServerWait { wait_ms: 7 },
            },
            FfiTransferEvent::ChunkRetried {
                blob: BLOB.into(),
                session_id: "s".into(),
                offset: 0,
                len: 4,
                reason: FfiRetryReason::ServerWait { wait_ms: 7 },
            },
        ),
        (
            TransferEvent::ChecksumRejected {
                blob: BLOB.into(),
                session_id: "s".into(),
                offset: 4,
                len: 4,
            },
            FfiTransferEvent::ChecksumRejected {
                blob: BLOB.into(),
                session_id: "s".into(),
                offset: 4,
                len: 4,
            },
        ),
        (
            TransferEvent::Finalize {
                blob: BLOB.into(),
                session_id: "s".into(),
                size: 9,
            },
            FfiTransferEvent::Finalize {
                blob: BLOB.into(),
                session_id: "s".into(),
                size: 9,
            },
        ),
        (
            TransferEvent::Merged {
                blob: BLOB.into(),
                asset_ref: "a".into(),
            },
            FfiTransferEvent::Merged {
                blob: BLOB.into(),
                asset_ref: "a".into(),
            },
        ),
        (
            TransferEvent::Verify {
                asset_id: "a".into(),
                durable: false,
                blobs: 3,
                not_durable: 1,
            },
            FfiTransferEvent::Verify {
                asset_id: "a".into(),
                durable: false,
                blobs: 3,
                not_durable: 1,
            },
        ),
        (
            TransferEvent::Receipt {
                asset_id: "a".into(),
                blob: BLOB.into(),
                receipt_seq: 2,
            },
            FfiTransferEvent::Receipt {
                asset_id: "a".into(),
                blob: BLOB.into(),
                receipt_seq: 2,
            },
        ),
        (
            TransferEvent::RangeRequested {
                blob: BLOB.into(),
                offset: 0,
                max_len: None,
            },
            FfiTransferEvent::RangeRequested {
                blob: BLOB.into(),
                offset: 0,
                max_len: None,
            },
        ),
        (
            TransferEvent::RangeReceived {
                blob: BLOB.into(),
                offset: 0,
                len: 5,
            },
            FfiTransferEvent::RangeReceived {
                blob: BLOB.into(),
                offset: 0,
                len: 5,
            },
        ),
        (
            TransferEvent::FetchRetried {
                blob: BLOB.into(),
                offset: 5,
                stalled: true,
                wait_ms: 250,
            },
            FfiTransferEvent::FetchRetried {
                blob: BLOB.into(),
                offset: 5,
                stalled: true,
                wait_ms: 250,
            },
        ),
        (
            TransferEvent::FetchVerified {
                blob: BLOB.into(),
                size: 5,
            },
            FfiTransferEvent::FetchVerified {
                blob: BLOB.into(),
                size: 5,
            },
        ),
        (
            TransferEvent::IntegrityRejected {
                blob: BLOB.into(),
                size: 5,
            },
            FfiTransferEvent::IntegrityRejected {
                blob: BLOB.into(),
                size: 5,
            },
        ),
    ];
    for (event, expected) in cases {
        assert_eq!(FfiTransferEvent::from(event), expected);
    }
    assert_eq!(
        FfiRetryReason::from(RetryReason::TransportReset { wait_ms: 1 }),
        FfiRetryReason::TransportReset { wait_ms: 1 }
    );
    assert_eq!(
        FfiRetryReason::from(RetryReason::OffsetRealign { to: 8 }),
        FfiRetryReason::OffsetRealign { to: 8 }
    );
    assert_eq!(
        FfiRetryReason::from(RetryReason::SessionLost),
        FfiRetryReason::SessionLost
    );
}
