//! The harness behind the bounded E2E cases (design/module-map.md, "E2E Test Surface").
//!
//! Three real things, wired the way production wires them, and nothing standing in for any of
//! them:
//!
//! - **The server** is the composition root — [`capsule_server::boot::assemble`] under the
//!   memory profile, the same function `capsule-server serve --memory` runs — bound to an
//!   ephemeral port. Real argon2 accounts, the real provisioned write authority, a real
//!   filesystem blob store under a temp root, the system clock. Not the test-only `Fixture`
//!   the server's own suites use: its `SwallowingBlobs`, `TestAuthority` and `ManualClock` are
//!   doubles, and a case that passed against them would prove the doubles.
//! - **The client** is `capsule-sdk` as shipped: every request leaves through the SDK's one
//!   HTTP client and therefore carries the protocol handshake the server gates on.
//! - **The library** is a real [`capsule_core::lifecycle::Workspace`] on a temp root, with a
//!   fast Argon2id parameter set for the *library* passphrase only (the wrap records its own
//!   parameters, so nothing under test reads a weaker setting than it would in the field).
//!
//! What the harness adds on top of the SDK is exactly the seams the SDK does not have yet, each
//! recorded as a finding in the pull request that landed this crate:
//!
//! - the **provenance rung** ([`push::push_asset`]): the SDK's push ladder ships metadata,
//!   derivatives and the original but never the `provenance` blob, and the server publishes an
//!   asset to the feed only once it holds both index-tier roles;
//! - the **directory publish** ([`Device::publish_directory`]): the server requires the
//!   `X-Capsule-Identity-Key` header on every publish and the SDK's `DirectoryClient` does not
//!   send it, and a directory must name the *server's* account id, which a `Workspace` cannot
//!   learn.
//!
//! Every test that uses this crate names its case — `E2E case N` — so `rg "E2E case N"` finds
//! it, per the module map's contract.

pub mod fixtures;
pub mod push;

use std::collections::BTreeMap;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::keys::{DeviceDirectory, DeviceEntry, DirectoryCore, HybridSigningKey};
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::crypto::provenance::record::ProvenanceRecord;
use capsule_core::lifecycle::Workspace;
use capsule_sdk::albums::{AlbumClient, AlbumTransport};
use capsule_sdk::auth::{AuthClient, Session};
use capsule_sdk::client::AuthenticatedClient;
use capsule_sdk::sync::{FeedEntry, SyncConsumer, SyncState};
use capsule_sdk::upload::{UploadClient, UploadTransport};
use capsule_server::blob::address::{ContentAddress, blob_path};
use capsule_server::boot::{self, Assembled};
use capsule_server::config::{Config, Demands, Overrides};
use tempfile::TempDir;
use uuid::Uuid;

/// The protocol date this build speaks — the same constant the SDK's transport sends.
pub const PROTOCOL_VERSION: &str = capsule_core::crypto::primitives::PROTOCOL_VERSION;

/// A PKCS#8 v1 Ed25519 key, base64: the retired deployment's `.env.example` value, which signs
/// nothing anywhere (the server's own binary test uses the same bytes for the same reason).
pub const JWT_ED25519_DER: &str =
    "MC4CAQAwBQYDK2VwBCIEIN6eTvXEL7xMZWHY8rTk7VbQSGSuRkle5MVfiiYUStLF";

/// Every account's password. The server hashes it with its production argon2 parameters.
pub const PASSWORD: &str = "correct horse battery staple";

/// Every library's passphrase, wrapped under [`FAST_KDF`].
pub const PASSPHRASE: &[u8] = b"library passphrase";

/// Fast Argon2id for the fixture libraries — the CLI's own precedent
/// (`capsule-cli/tests/import_round_trip.rs`). The wrapped blob records these parameters and
/// `unwrap` reads them back, so no code under test runs a weaker setting than it would in the
/// field; only the fixture's own unlock is cheap.
pub const FAST_KDF: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// The feed page size the harness pulls with; small enough that `has_more` paging is exercised
/// by any case that pushes more than a handful of assets.
pub const PAGE_SIZE: u32 = 64;

/// The composition root, assembled and listening on an ephemeral port.
///
/// Holds the [`Assembled`] so a case can reach the operator workers
/// (`assembled.maintenance`) over the same stores the router serves, and the blob root so a
/// case can assert bytes at their content address on disk.
pub struct Server {
    base_url: String,
    /// The assembled application: `app` for the router, `maintenance` for the operator workers.
    pub assembled: Assembled,
    /// `BLOB_ROOT`: where the filesystem blob store files finalized bytes.
    pub blob_root: TempDir,
    serve: tokio::task::JoinHandle<()>,
}

impl Server {
    /// Boot with the server's default protocol window.
    pub async fn boot() -> Self {
        Self::boot_with(None).await
    }

    /// Boot with `PROTOCOL_MIN`/`PROTOCOL_MAX` overridden — the knob case 9 turns to put this
    /// build outside the window.
    pub async fn boot_with_window(min: &str, max: &str) -> Self {
        Self::boot_with(Some((min, max))).await
    }

    async fn boot_with(window: Option<(&str, &str)>) -> Self {
        let blob_root = tempfile::tempdir().expect("a temp blob root");
        // Exactly what `serve --memory` reads: the memory profile needs two variables and
        // derives the rest (cursor MAC key, attestation seed) from the signing key.
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        env.insert(
            "BLOB_ROOT".to_owned(),
            blob_root.path().display().to_string(),
        );
        env.insert("JWT_ED25519_DER".to_owned(), JWT_ED25519_DER.to_owned());
        if let Some((min, max)) = window {
            env.insert("PROTOCOL_MIN".to_owned(), min.to_owned());
            env.insert("PROTOCOL_MAX".to_owned(), max.to_owned());
        }
        let overrides = Overrides {
            memory: true,
            ..Overrides::default()
        };
        let config = Config::load(&env, &overrides, Demands::Serve)
            .expect("the memory profile loads from BLOB_ROOT and JWT_ED25519_DER alone");
        let assembled = boot::assemble(&config)
            .await
            .expect("the composition root assembles under the memory profile");
        let service = assembled.service().expect("the router builds");
        let bound = kynos::server::Server::new(service)
            .bind(("127.0.0.1", 0))
            .prepare()
            .await
            .expect("an ephemeral port binds");
        let address = *bound
            .local_addrs()
            .first()
            .expect("a bound server has an address");
        let serve = tokio::spawn(async move {
            let _ = bound.serve().await;
        });
        tracing::info!(%address, "e2e server listening");
        Self {
            base_url: format!("http://{address}"),
            assembled,
            blob_root,
            serve,
        }
    }

    /// The API root (`http://127.0.0.1:PORT`): what the generated client, the sync consumer,
    /// the recovery client and the upgrade client take.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// `{root}/v1`: what the verify transport and the blob source take.
    #[must_use]
    pub fn v1(&self) -> String {
        format!("{}/v1", self.base_url)
    }

    /// `{root}/v1/auth`: what `AuthClient` and the directory publish take.
    #[must_use]
    pub fn auth_base(&self) -> String {
        format!("{}/v1/auth", self.base_url)
    }

    /// `{root}/v1/upload`: the upload transport's root.
    #[must_use]
    pub fn upload_base(&self) -> String {
        format!("{}/v1/upload", self.base_url)
    }

    /// `{root}/v1/albums`: the album transport's root.
    #[must_use]
    pub fn albums_base(&self) -> String {
        format!("{}/v1/albums", self.base_url)
    }

    /// Where the filesystem blob store files the blob at content address `hex`.
    #[must_use]
    pub fn blob_path(&self, hex: &str) -> PathBuf {
        let address = ContentAddress::parse(hex).expect("a lowercase SHA-256 hex digest");
        blob_path(self.blob_root.path(), &address)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.serve.abort();
    }
}

/// One account on one [`Server`], with its live SDK session and its real library.
///
/// The account's device directory is published by the harness (see
/// [`Device::publish_directory`]) and names two devices: the library's own, so the manifests it
/// signs satisfy invariant 7, and a standalone *proposer* device whose signing key the harness
/// holds — the `Workspace` keeps its device signing key private, and case 8 needs a key that
/// can sign an `UpgradeIntent`.
pub struct Device {
    /// The registered e-mail.
    pub email: String,
    /// The live SDK session; clone it freely, clones share one token store.
    pub session: Session,
    /// The **server's** id for this account, read from `GET /v1/auth/profile`. Distinct from
    /// `workspace.user_id()`, which the library mints locally — there is no seam to align them.
    pub user_id: Uuid,
    /// The real library.
    pub workspace: Workspace,
    /// The library root.
    pub root: TempDir,
    /// Scratch space for files to import.
    pub staging: TempDir,
    /// The identity key the published directory is signed with.
    pub identity: HybridSigningKey,
    /// A device signing key the harness holds, listed in the directory as `proposer_id`.
    pub proposer: HybridSigningKey,
    /// The proposer device's id.
    pub proposer_id: Uuid,
    directory_version: u64,
}

impl Device {
    /// Register a fresh account, create its library, publish its directory and provision its
    /// default album on the server — everything a first upload needs.
    pub async fn register(server: &Server, label: &str) -> Self {
        let email = format!("{label}-{}@e2e.capsule.test", Uuid::now_v7().simple());
        let auth = AuthClient::new(&server.auth_base()).expect("the auth base parses");
        let session = auth
            .register(&email, PASSWORD)
            .await
            .expect("a fresh account registers");
        let generated = AuthenticatedClient::new(server.base_url(), session.clone())
            .expect("the API root parses");
        let profile = generated
            .get_profile(PROTOCOL_VERSION, None)
            .await
            .expect("the profile answers")
            .into_inner();
        let user_id = Uuid::parse_str(&profile.user_id).expect("the account id is a UUID");

        let root = tempfile::tempdir().expect("a temp library root");
        let staging = tempfile::tempdir().expect("a temp staging dir");
        let mut workspace =
            Workspace::create_with_params(root.path(), PASSPHRASE, FAST_KDF).expect("a library");
        let default_album = workspace.default_album_id();
        workspace
            .ensure_album(default_album, "Imports")
            .expect("the default album's keys exist");

        let mut device = Self {
            email,
            session,
            user_id,
            workspace,
            root,
            staging,
            identity: HybridSigningKey::generate(),
            proposer: HybridSigningKey::generate(),
            proposer_id: Uuid::now_v7(),
            directory_version: 0,
        };
        device.publish_directory(server).await;
        let albums = AlbumClient::new(AlbumTransport::with_session(
            device.session.clone(),
            server.albums_base(),
        ));
        push::ensure_album(&albums, default_album)
            .await
            .expect("the default album provisions");
        device
    }

    /// A second live session on the same account — device B in the two-device cases.
    pub async fn login_again(&self, server: &Server) -> Session {
        AuthClient::new(&server.auth_base())
            .expect("the auth base parses")
            .login(&self.email, PASSWORD)
            .await
            .expect("the account signs in again")
            .into_session()
            .expect("the account has no second factor")
    }

    /// The generated REST client over this session.
    #[must_use]
    pub fn generated(&self, server: &Server) -> AuthenticatedClient {
        AuthenticatedClient::new(server.base_url(), self.session.clone())
            .expect("the API root parses")
    }

    /// The upload client over this session, pinned to this build's protocol date.
    #[must_use]
    pub fn upload_client(&self, server: &Server) -> UploadClient {
        UploadClient::new(UploadTransport::with_session(
            self.session.clone(),
            server.upload_base(),
            PROTOCOL_VERSION,
        ))
    }

    /// The library's own entry in its device directory.
    #[must_use]
    pub fn library_device(&self) -> DeviceEntry {
        let id = self.workspace.device_id();
        self.workspace
            .device_directory()
            .device(&id)
            .cloned()
            .expect("a library lists its own device")
    }

    /// The directory the harness publishes for this account: the server's account id, the
    /// library's device and the proposer device, signed by [`Device::identity`].
    #[must_use]
    pub fn directory(&self) -> DeviceDirectory {
        let library = self.library_device();
        DirectoryCore {
            user_id: self.user_id,
            directory_version: self.directory_version,
            updated_at: jiff::Timestamp::now().to_string(),
            devices: vec![
                library.clone(),
                DeviceEntry {
                    device_id: self.proposer_id,
                    dsk_public: self.proposer.verifying_key(),
                    dek_public: None,
                    added_at: library.added_at,
                    revoked_at: None,
                },
            ],
        }
        .sign(&self.identity)
    }

    /// Publish the next version of [`Device::directory`], returning the version stored.
    ///
    /// Sent through the session's HTTP client rather than `capsule_sdk::directory` because the
    /// server requires `X-Capsule-Identity-Key` (invariant 23's second clause) and the SDK's
    /// publish does not carry it — recorded as a finding by the pull request that landed this.
    pub async fn publish_directory(&mut self, server: &Server) -> u64 {
        self.directory_version += 1;
        let body = capsule_core::cbor::to_canonical_vec(&self.directory())
            .expect("a directory serializes");
        let identity = BASE64.encode(self.identity.verifying_key().to_bytes());
        let url = format!("{}/devices/directory", server.auth_base());
        let response = self
            .session
            .execute(|http| {
                http.post(&url)
                    .header("content-type", "application/cbor")
                    .header("x-capsule-identity-key", &identity)
                    .body(body.clone())
            })
            .await
            .expect("the publish reaches the server");
        assert_eq!(
            response.status().as_u16(),
            200,
            "the directory publish is accepted: {}",
            response.text().await.unwrap_or_default()
        );
        let stored: serde_json::Value = response.json().await.expect("a JSON body");
        let version = stored["directory_version"]
            .as_u64()
            .expect("the stored directory version");
        assert_eq!(version, self.directory_version);
        version
    }

    /// Write the synthetic JPEG to staging under `file_name` and import it into the default
    /// album, returning the asset id.
    pub fn import_jpeg(&mut self, file_name: &str) -> Uuid {
        let path = self.staging.path().join(file_name);
        std::fs::write(&path, fixtures::synthetic_jpeg()).expect("the fixture writes");
        let album = self.workspace.default_album_id();
        self.workspace
            .import_asset(album, &path)
            .expect("the JPEG imports")
    }

    /// The head of `asset_id`'s provenance chain.
    #[must_use]
    pub fn head_record(&self, asset_id: &Uuid) -> ProvenanceRecord {
        self.workspace
            .asset(asset_id)
            .expect("the asset is in the library")
            .chain
            .records()
            .last()
            .cloned()
            .expect("a chain is never empty")
    }

    /// Everything the feed holds for this account, from the beginning, through the SDK.
    pub async fn feed(&self, server: &Server) -> Vec<FeedEntry> {
        feed_from_start(server, self.session.clone()).await
    }
}

/// Pull the whole feed for `session` from cursor zero through the SDK consumer.
pub async fn feed_from_start(server: &Server, session: Session) -> Vec<FeedEntry> {
    let consumer =
        SyncConsumer::with_session(server.base_url(), session).expect("the API root parses");
    let mut state = SyncState::new(PROTOCOL_VERSION);
    let mut entries = Vec::new();
    loop {
        let page = consumer
            .pull_into(&mut state, PAGE_SIZE)
            .await
            .expect("the feed answers");
        let more = page.has_more;
        entries.extend(page.entries);
        if !more {
            return entries;
        }
    }
}

/// The feed entry for `asset_id`, if the server publishes it.
#[must_use]
pub fn entry_for<'a>(entries: &'a [FeedEntry], asset_id: &Uuid) -> Option<&'a FeedEntry> {
    let wanted = asset_id.to_string().into_bytes();
    entries.iter().find(|entry| entry.asset_id == wanted)
}
