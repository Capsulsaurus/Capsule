//! **E2E case 3** — sync feed pickup, the client half.
//!
//! The server half is named in `capsule-server/tests/sync.rs`. Here device A uploads through
//! the real SDK and library; device B — a second session on the same account, with a fresh
//! `SyncState` at cursor zero — pulls the feed, sees the entry, and fetches the metadata blob
//! and the original by content address, byte-equal to what A's bundle held.
//!
//! B's `verify_asset` is not asserted: verification needs the album keys, which reach a second
//! device through enrollment or backup (cases 6 and 12), not through the feed.

use capsule_e2e::push::push_asset;
use capsule_e2e::{Device, PAGE_SIZE, PROTOCOL_VERSION, Server, entry_for};
use capsule_sdk::fetch::{BlobSource as _, HttpBlobSource, RangeOutcome, fetch_blob};
use capsule_sdk::sync::{SyncConsumer, SyncState};

// Blocked by #467, and only this tree could show it. `capsule-core` mints the library's
// account id locally (`keystore.rs`, `Uuid::now_v7()`) and writes it into every signed
// manifest as `created_by_user`; the server mints its own at registration
// (`auth/registry.rs`). #405 (merged here) refuses an upload whose `created_by_user` is not
// the authenticated caller — correctly: the design keeps **one** account namespace, the
// server's, and `verify_asset` step 6 and `directory::project_version` both enforce it. What
// is missing is the client seam that would let a library open *as* a server account, which is
// exactly what #467 is filed for. Neither #405's branch nor #409's had the other, so neither
// could see this. Un-ignore when #467 lands its constructor.
#[tokio::test]
#[ignore = "#467: a Workspace has no seam to adopt the server's account id, so the \
           real SDK push path cannot satisfy #405's created_by_user == caller rule"]
async fn e2e_case_3_a_second_device_sees_the_entry_and_fetches_the_bytes() {
    let server = Server::boot().await;
    let mut a = Device::register(&server, "device-a").await;
    let asset = a.import_jpeg("shared.jpg");
    let pushed = push_asset(&a, &server, &asset).await;

    // Device B: same account, its own session, nothing synced yet.
    let session_b = a.login_again(&server).await;
    let consumer =
        SyncConsumer::with_session(server.base_url(), session_b.clone()).expect("a consumer");
    let mut state = SyncState::new(PROTOCOL_VERSION);
    assert!(state.cursor().is_start());
    let page = consumer
        .pull_into(&mut state, PAGE_SIZE)
        .await
        .expect("B's first pull");
    assert!(!page.has_more);
    assert!(!state.cursor().is_start(), "the cursor advanced");
    let album = a.workspace.default_album_id().to_string().into_bytes();
    assert!(
        state.high_water(&album).is_some(),
        "the album's high-water mark is set"
    );

    let entry = entry_for(&page.entries, &asset).expect("A's upload is on B's feed");
    assert_eq!(entry.album_id, album);
    assert!(entry.original_held);

    // The metadata blob, by the content address the entry carries. The feed states no size for
    // it, so B asks for the whole object rather than a range it cannot know the length of.
    let source = HttpBlobSource::new(session_b, server.v1());
    let metadata_address =
        String::from_utf8(entry.metadata_blob.clone()).expect("a hex content address");
    assert_eq!(
        metadata_address,
        pushed
            .bundle
            .metadata_blob_hash
            .expect("a create binds a metadata blob")
            .to_hex()
    );
    let RangeOutcome::Complete {
        bytes: metadata_bytes,
    } = source.get_range(&metadata_address, 0, None).await
    else {
        panic!("the metadata blob serves whole");
    };
    assert_eq!(metadata_bytes, pushed.bundle.metadata_blob);

    // The original, by content address and declared size, byte for byte.
    let original = entry
        .blobs
        .original
        .as_ref()
        .expect("the original is referenced");
    let original_bytes = fetch_blob(&source, &original.ciphertext_hash, original.size)
        .await
        .expect("the original fetches");
    assert_eq!(original_bytes, pushed.bundle.ciphertext);
}
