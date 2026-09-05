//! **E2E case 1** — auth → sync → client-side library query.
//!
//! Sign in → access token → the sync feed returns the account's entries → the client applies
//! them → a local SQLite query lists the expected album. The client leg is the CLI's own
//! orchestration (`capsule_cli::remote::{sync, list}`) over its migrated SQLite store, which
//! is what `capsule sync` and `capsule list` run; the SDK session is the one the CLI would have
//! persisted after `capsule auth login`.

use capsule_cli::remote::{self, RemoteConfig};
use capsule_cli::session::SessionStore;
use capsule_e2e::push::push_asset;
use capsule_e2e::{Device, PROTOCOL_VERSION, Server};
use migration::{Migrator, MigratorTrait as _};

#[tokio::test]
async fn e2e_case_1_sign_in_sync_and_a_local_query_lists_the_album() {
    let server = Server::boot().await;
    let mut device = Device::register(&server, "cli-user").await;
    let asset = device.import_jpeg("first.jpg");
    push_asset(&device, &server, &asset).await;

    // The CLI's state: a migrated SQLite store and the persisted session from sign-in.
    let home = tempfile::tempdir().expect("a temp CLI home");
    let db_url = format!(
        "sqlite://{}?mode=rwc",
        home.path().join("library.sqlite").display()
    );
    let db = sea_orm::Database::connect(&db_url)
        .await
        .expect("the CLI store opens");
    Migrator::up(&db, None)
        .await
        .expect("the CLI migrations run");
    let store = SessionStore::new(home.path().join("session.json"));
    let persisted = device
        .session
        .export()
        .await
        .expect("a live session exports");
    store.save(&persisted).expect("the session persists");

    let remote = RemoteConfig {
        auth_endpoint: server.auth_base(),
        sync_endpoint: server.base_url().to_owned(),
        upload_endpoint: server.upload_base(),
        albums_endpoint: server.albums_base(),
        protocol_version: PROTOCOL_VERSION.to_owned(),
    };
    let summary = remote::sync(&remote, &store, &db, 256, false, false)
        .await
        .expect("`capsule sync` completes");
    assert_eq!(summary.applied, 1, "one entry applied: {summary:?}");
    assert_eq!(summary.albums, 1);
    assert!(!summary.dry_run);

    // The client-side library query lists the expected album and asset.
    let rows = remote::list(&db, false)
        .await
        .expect("`capsule list` answers");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(
        row.album_id,
        device.workspace.default_album_id().to_string().into_bytes()
    );
    assert_eq!(row.asset_id, asset.to_string().into_bytes());
    assert!(row.original_held);
    assert!(!row.tombstoned);

    // A second sync is a no-op — the cursor persisted with the page.
    let again = remote::sync(&remote, &store, &db, 256, false, false)
        .await
        .expect("a second sync completes");
    assert_eq!(again.applied, 0, "nothing new: {again:?}");
}
