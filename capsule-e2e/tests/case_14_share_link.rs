//! **E2E case 14** — a share link issued over TCP by the CLI, opened with the URL alone.
//!
//! The issuer's leg is the CLI's own orchestration (`capsule_cli::share::{create, revoke}`),
//! which is what `capsule share create` and `capsule share revoke` run, over the session
//! `capsule auth login` persists. The viewer's leg has nothing but the printed URL: a bare
//! generated client with no session fetches what the public `/s/` path serves, and the
//! `capsule-wasm` entry points the web viewer calls (`open_share_metadata_wasm`, `open_share`,
//! `ShareScope::decrypt_blob`) open it natively, as
//! `capsule-server/tests/share.rs::a_published_link_opens_through_the_browser_surface_from_what_the_server_serves`
//! does in-process.
//!
//! Covered: an album link with no passphrase, whose scope travels inside the sealed record; an
//! asset link behind a passphrase, whose scope is served from `/wrapped-secret`; the refusal to
//! issue a link over originals the server does not hold yet; and revocation turning the link
//! into the public path's one indistinguishable `404`.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_cli::remote::RemoteConfig;
use capsule_cli::session::SessionStore;
use capsule_cli::share::{self, CreateRequest, IssuedShare, ShareCommandError};
use capsule_core::sharing::ShareScope;
use capsule_e2e::fixtures::{large_synthetic_jpeg, synthetic_jpeg};
use capsule_e2e::push::push_asset;
use capsule_e2e::{Device, PROTOCOL_VERSION, Server};
use capsule_sdk::rest;
use tempfile::TempDir;
use uuid::Uuid;

const LINK_PASSPHRASE: &str = "a passphrase sent separately";

/// The CLI's state for one account: the session store `capsule auth login` writes and the
/// endpoints `CAPSULE_ENDPOINT` resolves to.
struct CliState {
    remote: RemoteConfig,
    store: SessionStore,
    _home: TempDir,
}

async fn signed_in_cli(device: &Device, server: &Server) -> CliState {
    let home = tempfile::tempdir().expect("a temp CLI home");
    let store = SessionStore::new(home.path().join("session.json"));
    let session = device.login_again(server).await;
    store
        .save(&session.export().await.expect("a live session exports"))
        .expect("the session persists");
    CliState {
        remote: RemoteConfig {
            auth_endpoint: server.auth_base(),
            sync_endpoint: server.base_url().to_owned(),
            upload_endpoint: server.upload_base(),
            albums_endpoint: server.albums_base(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
        },
        store,
        _home: home,
    }
}

/// Split a printed share URL into its opaque id and fragment, checking it hangs off `server`.
fn parse_url<'a>(server: &Server, url: &'a str) -> (&'a str, &'a str) {
    let path = url
        .strip_prefix(server.base_url())
        .and_then(|rest| rest.strip_prefix("/s/"))
        .unwrap_or_else(|| panic!("{url} is a /s/ URL on the server's origin"));
    path.split_once('#')
        .unwrap_or_else(|| panic!("{url} carries its fragment"))
}

/// Everything a viewer opened from a URL: each covered asset's id and decrypted bytes.
async fn open_as_viewer(
    server: &Server,
    url: &str,
    passphrase: Option<&str>,
) -> Vec<(String, Vec<u8>)> {
    let (id, fragment) = parse_url(server, url);
    let viewer = rest::Client::new(server.base_url()).expect("the API root parses");

    let served = viewer
        .share_metadata(id)
        .await
        .expect("a live link serves its pointer without a credential")
        .into_inner();
    assert_eq!(served.passphrase_protected, passphrase.is_some());
    let record = blob(&viewer, id, &served.metadata_hash).await;
    let view = capsule_wasm::open_share_metadata_wasm(&record, id, fragment)
        .expect("the served record opens with the URL alone");

    let scope = if passphrase.is_some() {
        assert_eq!(
            view.link_scope_b64().expect("projects"),
            None,
            "a passphrase link's scope is not in the fragment-sealed record"
        );
        let wrapped = viewer
            .share_wrapped_secret(id)
            .await
            .expect("a passphrase link serves its wrapped scope")
            .into_inner();
        // A wrong passphrase is not tried here: every `capsule-wasm` refusal builds a
        // `JsError`, which aborts off wasm32. `capsule_core::sharing`'s own tests cover it.
        BASE64.encode(&wrapped)
    } else {
        view.link_scope_b64()
            .expect("projects")
            .expect("a link-only record carries its scope")
    };
    let scope = capsule_wasm::open_share(&scope, id, fragment, passphrase.map(str::to_owned))
        .expect("the scope opens");

    let mut opened = Vec::new();
    for asset in view.assets() {
        let ciphertext = blob(&viewer, id, &asset.content_hash()).await;
        let plaintext = scope
            .decrypt_blob(
                &asset.asset_id(),
                asset.amk_version(),
                &asset.nonce_prefix_hex(),
                &ciphertext,
                asset.wrapped_file_key_hex(),
            )
            .expect("every covered asset decrypts");
        opened.push((asset.asset_id(), plaintext));
    }
    opened
}

async fn blob(viewer: &rest::Client, id: &str, hash: &str) -> Vec<u8> {
    match viewer
        .share_blob(id, hash, None)
        .await
        .expect("the link serves an address it names")
        .into_inner()
    {
        rest::ShareBlobResponse::Status200(bytes) => bytes.to_vec(),
        rest::ShareBlobResponse::Status206(_) => panic!("no range was asked for"),
    }
}

/// The public path's `404` body for `id`, or a panic when it answers anything else.
async fn not_found(server: &Server, id: &str) -> serde_json::Value {
    let viewer = rest::Client::new(server.base_url()).expect("the API root parses");
    match viewer.share_metadata(id).await {
        Err(rest::Error::Api(response)) => {
            assert_eq!(response.status().as_u16(), 404);
            match response.into_inner() {
                rest::ShareMetadataError::Status404(problem) => {
                    serde_json::to_value(&*problem).expect("a problem serializes")
                }
                other => panic!("a 404 carries the coded problem: {other}"),
            }
        }
        other => panic!("{id} must answer 404, got {other:?}"),
    }
}

fn source_bytes(device: &Device, file_name: &str) -> Vec<u8> {
    std::fs::read(device.staging.path().join(file_name)).expect("the imported file is staged")
}

async fn issue(
    cli: &CliState,
    device: &mut Device,
    scope: ShareScope,
    passphrase: Option<&str>,
) -> Result<IssuedShare, ShareCommandError> {
    share::create(
        &cli.remote,
        &cli.store,
        &mut device.workspace,
        &CreateRequest {
            scope,
            expires_at: None,
            passphrase: passphrase.map(str::to_owned),
        },
        jiff::Timestamp::now(),
    )
    .await
}

#[tokio::test]
async fn e2e_case_14_a_link_the_cli_issues_opens_from_its_url_and_stops_when_revoked() {
    let server = Server::boot().await;
    let mut owner = Device::register(&server, "sharer").await;
    let small = owner.import_file("small.jpg", &synthetic_jpeg());
    let large = owner.import_file("large.jpg", &large_synthetic_jpeg());
    let album = owner.workspace.default_album_id();
    let cli = signed_in_cli(&owner, &server).await;

    // Before the originals are pushed, the CLI refuses rather than register a link whose
    // assets would answer 404 to every viewer — and nothing is registered.
    match issue(&cli, &mut owner, ShareScope::Album(album), None).await {
        Err(ShareCommandError::NotHeld { missing }) => assert_eq!(missing.len(), 2),
        other => panic!("an album link over unpushed originals is refused: {other:?}"),
    }

    push_asset(&owner, &server, &small).await;
    push_asset(&owner, &server, &large).await;

    // An album link with no passphrase: the scope travels inside the sealed record.
    let album_link = issue(&cli, &mut owner, ShareScope::Album(album), None)
        .await
        .expect("`capsule share create --album` issues");
    assert_eq!(album_link.covered, 2);
    assert!(!album_link.passphrase_protected);
    assert!(
        album_link.url.starts_with(&format!(
            "{}/s/{}#",
            server.base_url(),
            album_link.opaque_id
        )),
        "the URL is the origin, the opaque id and the fragment: {}",
        album_link.url
    );
    let mut opened = open_as_viewer(&server, &album_link.url, None).await;
    opened.sort();
    let mut expected = vec![
        (small.to_string(), source_bytes(&owner, "small.jpg")),
        (large.to_string(), source_bytes(&owner, "large.jpg")),
    ];
    expected.sort();
    assert_eq!(opened, expected, "the viewer decrypts every original");

    // An asset link behind a passphrase: the scope is served from `/wrapped-secret`.
    let asset_link = issue(
        &cli,
        &mut owner,
        ShareScope::Asset(large),
        Some(LINK_PASSPHRASE),
    )
    .await
    .expect("`capsule share create --asset --protect` issues");
    assert_eq!(asset_link.covered, 1);
    assert!(asset_link.passphrase_protected);
    assert_eq!(
        open_as_viewer(&server, &asset_link.url, Some(LINK_PASSPHRASE)).await,
        vec![(large.to_string(), source_bytes(&owner, "large.jpg"))]
    );

    // Revocation turns the link into the same 404 an id that never existed gets.
    let never_issued = Uuid::now_v7().simple().to_string();
    let baseline = not_found(&server, &never_issued).await;
    let revoked = share::revoke(&cli.remote, &cli.store, &album_link.opaque_id)
        .await
        .expect("`capsule share revoke` is accepted");
    assert_eq!(revoked, album_link.opaque_id);
    assert_eq!(
        not_found(&server, &album_link.opaque_id).await,
        baseline,
        "a revoked link is indistinguishable from one that never existed"
    );

    // The other link is untouched, and revoking it again answers the same.
    assert_eq!(
        open_as_viewer(&server, &asset_link.url, Some(LINK_PASSPHRASE))
            .await
            .len(),
        1
    );
    share::revoke(&cli.remote, &cli.store, &album_link.opaque_id)
        .await
        .expect("revoking an already-revoked link is accepted");
}
