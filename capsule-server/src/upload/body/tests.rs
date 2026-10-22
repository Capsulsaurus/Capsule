use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

#[tokio::test]
async fn a_read_that_finishes_in_time_is_handed_back() {
    let read = async { 7 };
    assert_eq!(read_within(Duration::from_secs(5), read).await, Ok(7));
}

#[tokio::test]
async fn a_read_that_never_finishes_is_given_up_on_at_the_deadline() {
    let started = std::time::Instant::now();
    let outcome = read_within(Duration::from_millis(50), std::future::pending::<()>()).await;
    assert_eq!(outcome, Err(DeadlinePassed));
    assert!(started.elapsed() >= Duration::from_millis(50));
}

#[tokio::test]
async fn a_slow_but_steady_read_under_the_deadline_lands() {
    let read = async {
        for _ in 0..4 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        "whole"
    };
    assert_eq!(read_within(Duration::from_secs(5), read).await, Ok("whole"));
}

#[tokio::test]
async fn a_zero_deadline_refuses_without_starting_the_read() {
    let polled = Arc::new(AtomicBool::new(false));
    let seen = polled.clone();
    let read = async move {
        seen.store(true, Ordering::SeqCst);
    };
    assert_eq!(read_within(Duration::ZERO, read).await, Err(DeadlinePassed));
    assert!(
        !polled.load(Ordering::SeqCst),
        "a deadline that already passed reads nothing"
    );
}

#[test]
fn the_body_rejections_keep_their_codes_through_the_conversion() {
    let unsupported = ChunkBodyRejection::from(OpaqueBodyRejection::UnsupportedMediaType {
        code: error_codes::UPLOAD_UNSUPPORTED_MEDIA_TYPE,
    });
    assert!(matches!(
        unsupported,
        ChunkBodyRejection::UnsupportedMediaType { code } if code == error_codes::UPLOAD_UNSUPPORTED_MEDIA_TYPE
    ));
    let unreadable = ChunkBodyRejection::from(OpaqueBodyRejection::Unreadable {
        code: error_codes::UPLOAD_MALFORMED_REQUEST,
    });
    assert!(matches!(
        unreadable,
        ChunkBodyRejection::Unreadable { code } if code == error_codes::UPLOAD_MALFORMED_REQUEST
    ));
}
