//! Cross-module integration for slice `S-D2`: the real `capsule-sdk` sync
//! consumer driving THIS crate's real `SyncService` over a genuine tonic channel
//! through the salvo↔tonic bridge (mirrors upload's `tests/sdk_client.rs`).
//!
//! Covers the download-sync client Validation bullets that need a real peer:
//! - the **opaque cursor round-trip** — the server-MAC'd `next_cursor` is handed
//!   back verbatim and paginates the feed forward;
//! - the **high-water anti-rewind** against a *validly-MAC'd but older* cursor from
//!   the real server (cursor authenticity, client-side half): the server authenticates
//!   the replayed cursor and re-serves an earlier page, yet the client's per-album
//!   `sync_seq` high-water mark refuses the rewind;
//! - the negotiated-protocol **forward-version** and **forged-cursor** rejections
//!   surfaced as typed errors carrying the stable `error.*` code over the wire.

#![allow(clippy::unwrap_used)]

use capsule_i18n::error_codes;
use capsule_sdk::sync::{SyncConsumer, SyncCursor, SyncError, SyncState};
use salvo::Service;
use salvo::conn::tcp::TcpAcceptor;
use salvo::prelude::Server;

use super::{PROTOCOL, TestCtx, setup};

/// The client's max-known protocol — inside the server's window, above the seeded
/// entries' pin, so nothing is (client-side) forward-version rejected.
const CLIENT_MAX_PROTOCOL: &str = "2026-12-31";

/// Serve the real sync router on an ephemeral TCP port and return its base URL.
async fn serve(ctx: &TestCtx) -> String {
    let router = crate::get_router(ctx.db.clone(), ctx.config.clone())
        .await
        .expect("router");
    let service = Service::new(router);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = TcpAcceptor::try_from(listener).unwrap();
    tokio::spawn(async move { Server::new(acceptor).serve(service).await });
    format!("http://{addr}")
}

fn seqs(page: &capsule_sdk::sync::SyncPage) -> Vec<u64> {
    page.entries.iter().map(|entry| entry.sync_seq).collect()
}

/// The opaque cursor round-trips through the SDK consumer and paginates the real
/// feed forward, advancing the client-held per-album high-water mark each page.
#[tokio::test]
async fn sdk_consumer_round_trips_the_cursor_and_advances_high_water() {
    let ctx = setup().await;
    for _ in 0..4 {
        ctx.seed_entry().await;
    }
    let album = ctx.album_id.clone().into_bytes();

    let url = serve(&ctx).await;
    let channel = SyncConsumer::connect(url).await.expect("connect");
    let mut consumer = SyncConsumer::with_static_token(channel, ctx.token(), PROTOCOL);
    let mut state = SyncState::new(CLIENT_MAX_PROTOCOL);

    // First page (from the empty first-sync cursor): seqs 1,2.
    let page1 = consumer.pull_into(&mut state, 2).await.expect("page 1");
    assert_eq!(seqs(&page1), vec![1, 2]);
    assert_eq!(state.high_water(&album), Some(2));
    assert!(
        !state.cursor().is_start(),
        "the server returned a real opaque next_cursor"
    );

    // Second page: the stored next_cursor round-trips and yields seqs 3,4.
    let page2 = consumer.pull_into(&mut state, 2).await.expect("page 2");
    assert_eq!(seqs(&page2), vec![3, 4]);
    assert_eq!(state.high_water(&album), Some(4));

    // Draining past the tail yields an empty page and leaves the high-water mark.
    let tail = consumer.pull_into(&mut state, 2).await.expect("tail");
    assert!(tail.entries.is_empty());
    assert_eq!(state.high_water(&album), Some(4));
}

/// **Cursor authenticity (client-side).** After advancing past seqs 1..=4, the
/// (malicious) server replays the *authentic* first-sync cursor and re-serves seqs
/// 1,2. The server authenticated the cursor and returned the page — yet the
/// client's high-water mark refuses the rewind, leaving state untouched.
#[tokio::test]
async fn older_authentic_cursor_is_refused_by_the_client_high_water() {
    let ctx = setup().await;
    for _ in 0..4 {
        ctx.seed_entry().await;
    }
    let album = ctx.album_id.clone().into_bytes();

    let url = serve(&ctx).await;
    let channel = SyncConsumer::connect(url).await.expect("connect");
    let mut consumer = SyncConsumer::with_static_token(channel, ctx.token(), PROTOCOL);
    let mut state = SyncState::new(CLIENT_MAX_PROTOCOL);

    let page1 = consumer
        .pull(&SyncCursor::start(), 2)
        .await
        .expect("page 1");
    state.apply_page(&page1).expect("apply page 1");
    let page2 = consumer.pull(&page1.next_cursor, 2).await.expect("page 2");
    state.apply_page(&page2).expect("apply page 2");
    assert_eq!(state.high_water(&album), Some(4));

    // Replay the authentic empty cursor: the server re-serves seqs 1,2 (it verifies
    // and accepts the cursor — this is not a forgery).
    let replay = consumer
        .pull(&SyncCursor::start(), 2)
        .await
        .expect("server re-serves the authentic older cursor");
    assert_eq!(seqs(&replay), vec![1, 2]);

    // The client refuses the rewind on its own high-water mark.
    let err = state.apply_page(&replay).unwrap_err();
    assert!(
        matches!(err, SyncError::Rewind { high_water: 4, .. }),
        "expected a rewind rejection, got {err:?}"
    );
    assert_eq!(state.high_water(&album), Some(4), "state left untouched");
}

/// The negotiated protocol and cursor authenticity are enforced by the real server
/// and surfaced as typed SDK errors carrying the stable `error.*` code: a too-new
/// protocol is forward-version rejected, a forged cursor is rejected.
#[tokio::test]
async fn wire_forward_version_and_forged_cursor_are_typed_rejections() {
    let ctx = setup().await;
    ctx.seed_entry().await;
    let url = serve(&ctx).await;
    let channel = SyncConsumer::connect(url).await.expect("connect");

    // A consumer speaking a protocol beyond the server window is refused.
    let mut ahead = SyncConsumer::with_static_token(channel.clone(), ctx.token(), "2099-01-01");
    let err = ahead
        .pull(&SyncCursor::start(), 10)
        .await
        .expect_err("forward-version must reject");
    assert_eq!(
        err.error_code(),
        Some(error_codes::PROTOCOL_VERSION_UNSUPPORTED),
        "got {err:?}"
    );

    // A forged cursor (right length, wrong MAC) is rejected at the boundary.
    let mut good = SyncConsumer::with_static_token(channel, ctx.token(), PROTOCOL);
    let err = good
        .pull(&SyncCursor::from_bytes(vec![9u8; 41]), 10)
        .await
        .expect_err("forged cursor must reject");
    assert_eq!(
        err.error_code(),
        Some(error_codes::SYNC_CURSOR_INVALID),
        "got {err:?}"
    );
}
