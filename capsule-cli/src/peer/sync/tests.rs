//! `peer offer` and `peer pull` end to end over a real in-process peering channel: two devices
//! of one account, the receiver's tombstones persisted in a CLI database it reopens, and the
//! fail-closed refusal when they cannot be read.

use std::path::{Path, PathBuf};
use std::time::Duration;

use capsule_core::crypto::primitives::{Argon2Params, DeviceTier};
use capsule_sdk::peering::MemoryDirectoryVersions;
use migration::{Migrator, MigratorTrait};
use sea_orm::{Database, DatabaseConnection};

use super::*;
use crate::peering_state::record_purged;

const FAST: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// A scratch directory removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("capsule-cli-peer-sync-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("a scratch dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// A fresh, migrated connection to this scratch's CLI database: what a restarted process
    /// opens.
    async fn db(&self) -> DatabaseConnection {
        let url = format!("sqlite://{}?mode=rwc", self.0.join("cli.sqlite").display());
        let db = Database::connect(&url).await.expect("connect sqlite");
        Migrator::up(&db, None).await.expect("migrate");
        db
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Two devices of one account. `a` holds the album and every photo; `b` is a second device of
/// the same account that holds the album's keys and none of the photos. Both pin the account's
/// directory listing the two devices.
struct Pair {
    a: Workspace,
    b: Workspace,
    trust: PinnedTrust,
    album: Uuid,
}

fn pair(scratch: &Scratch) -> Pair {
    let lib_a = scratch.path().join("a");
    let lib_b = scratch.path().join("b");
    let mut a = Workspace::create_with_params(&lib_a, b"device a", FAST).expect("library a");
    let album = a.create_album("Trip").expect("an album");

    let secret = b"a recovery secret for the second device";
    let escrow = a
        .escrow_master_key(secret, DeviceTier::LowRam)
        .expect("an escrow");
    let master = capsule_core::backup::recover_master_key(&escrow, secret).expect("recovers");
    let mut b = Workspace::create_from_master(
        &lib_b,
        b"device b",
        FAST,
        capsule_core::crypto::keys::MasterKey::from_bytes(master),
        a.user_id(),
        &a.sealed_identity(),
    )
    .expect("library b");
    let escrowed = a.album_key_escrow(&album).expect("the album escrow");
    b.adopt_album_key_escrows(&[(album, escrowed)])
        .expect("b holds the album");

    let entry = b
        .device_directory()
        .device(&b.device_id())
        .expect("b lists itself")
        .clone();
    let directory = a.cross_sign_device(entry).expect("b is cross-signed");
    let trust = PinnedTrust {
        user_ik: a.user_ik_public(),
        directory,
    };
    Pair { a, b, trust, album }
}

fn photo(scratch: &Scratch, workspace: &mut Workspace, album: Uuid, name: &str) -> Uuid {
    let source = scratch.path().join(name);
    std::fs::write(
        &source,
        [b"\xFF\xD8\xFF a photo named ".as_slice(), name.as_bytes()].concat(),
    )
    .expect("a source");
    workspace.import_asset(album, &source).expect("imports")
}

/// One `peer offer` from `a` to one `peer pull` on `b`, over a real channel on loopback.
async fn sync_once(
    pair: &mut Pair,
    tombstones: &BTreeSet<Uuid>,
    assets: BTreeSet<Uuid>,
) -> (OfferSummary, PullSummary) {
    let listener = PeerListener::bind(
        "127.0.0.1:0".parse().expect("an address"),
        Duration::from_secs(30),
    )
    .await
    .expect("binds");
    let address = listener.local_addr().expect("bound").to_string();
    let options = OfferOptions {
        peer: address,
        to: Some(pair.b.device_id()),
        assets,
    };
    let (a_versions, b_versions) = (
        MemoryDirectoryVersions::default(),
        MemoryDirectoryVersions::default(),
    );
    let local_a = LocalPeer {
        device_id: pair.a.device_id(),
        signer: pair.a.device_signer(),
        trust: &pair.trust,
        versions: &a_versions,
    };
    let offered = offer_over_peer(&pair.a, local_a, &pair.trust, &options);
    let pulled = receive(
        &mut pair.b,
        &pair.trust,
        &b_versions,
        tombstones,
        0,
        &listener,
    );
    tokio::join!(offered, pulled)
}

fn sorted(mut ids: Vec<Uuid>) -> Vec<Uuid> {
    ids.sort();
    ids
}

/// A purged asset a peer offers after a restart is refused as `tombstoned`, end to end over the
/// channel: the receiver recorded the purge, reopened its database, and ingested against what it
/// read back. The rest of the artifact still applies, and a second pull finds it held.
#[tokio::test]
async fn a_purged_asset_offered_after_a_restart_is_tombstoned_and_the_rest_applies() {
    let scratch = Scratch::new();
    let mut pair = pair(&scratch);
    let album = pair.album;
    let kept = photo(&scratch, &mut pair.a, album, "kept.jpg");
    let purged = photo(&scratch, &mut pair.a, album, "purged.jpg");

    {
        let db = scratch.db().await;
        record_purged(&db, &[purged], jiff::Timestamp::now())
            .await
            .expect("the purge is recorded");
    }
    // A restart: a new connection reads what the last one wrote.
    let tombstones = ingest_tombstones(&scratch.db().await)
        .await
        .expect("the tombstones read back");

    let (offered, pulled) = sync_once(&mut pair, &tombstones, BTreeSet::new()).await;
    assert!(offered.failed.is_empty(), "{:?}", offered.failed);
    assert_eq!(offered.receiver, Some(pair.b.device_id()));
    assert_eq!(sorted(offered.assets.clone()), sorted(vec![kept, purged]));
    assert_eq!(
        offered.served_bytes, offered.bytes,
        "the whole artifact was pulled"
    );

    assert!(pulled.failed.is_empty(), "{:?}", pulled.failed);
    assert_eq!(pulled.peer, Some(pair.a.device_id()));
    assert_eq!(
        pulled.artifacts,
        vec![offered.artifact.clone().expect("offered")]
    );
    assert_eq!(pulled.applied, vec![kept]);
    assert_eq!(pulled.tombstoned, vec![purged]);
    assert!(pulled.identical.is_empty() && pulled.quarantined.is_empty());
    assert!(pulled.incomplete.is_empty());
    assert!(
        pair.b.asset(&kept).is_some(),
        "the applied asset is written"
    );
    assert!(
        pair.b.asset(&purged).is_none(),
        "the purged asset is not resurrected"
    );
    assert_eq!(
        pair.b.local_heads().get(&kept),
        pair.a.local_heads().get(&kept)
    );

    // The receiver's offer now names the kept asset's head, so it is never sent again; the
    // purged one it holds no head for is, and is refused again.
    let (offered_again, again) = sync_once(&mut pair, &tombstones, BTreeSet::new()).await;
    assert_eq!(offered_again.assets, vec![purged]);
    assert_eq!(offered_again.excluded, 1);
    assert!(again.failed.is_empty(), "{:?}", again.failed);
    assert!(again.identical.is_empty(), "a held asset is not sent");
    assert_eq!(again.tombstoned, vec![purged]);
    assert!(again.applied.is_empty());
}

/// An offer to a receiver that already holds every asset at the same head sends an artifact
/// with no assets, and the `--json` summary counts what the receiver's offer excluded.
#[tokio::test]
async fn an_offer_to_a_receiver_holding_every_head_sends_no_assets() {
    let scratch = Scratch::new();
    let mut pair = pair(&scratch);
    let album = pair.album;
    let one = photo(&scratch, &mut pair.a, album, "one.jpg");
    let two = photo(&scratch, &mut pair.a, album, "two.jpg");
    let none = BTreeSet::new();

    let (first, pulled) = sync_once(&mut pair, &none, BTreeSet::new()).await;
    assert_eq!(first.excluded, 0, "the receiver held nothing");
    assert_eq!(sorted(pulled.applied), sorted(vec![one, two]));

    let (offered, pulled) = sync_once(&mut pair, &none, BTreeSet::new()).await;
    assert!(offered.failed.is_empty(), "{:?}", offered.failed);
    assert!(offered.assets.is_empty(), "{:?}", offered.assets);
    assert_eq!(offered.excluded, 2);
    let artifact = offered
        .artifact
        .clone()
        .expect("an empty artifact is still offered");
    assert_eq!(pulled.artifacts, vec![artifact]);
    assert!(pulled.failed.is_empty(), "{:?}", pulled.failed);
    assert!(pulled.applied.is_empty() && pulled.identical.is_empty());

    let json = serde_json::to_value(&offered).expect("serializes");
    assert_eq!(json["excluded"], 2);
}

/// An offer to a receiver that sends no offer pages (one that opens the channel without one, as
/// an older build does) still sends every managed asset, and the receiver's ingest reports each
/// one it holds `identical`.
#[tokio::test]
async fn an_offer_to_a_receiver_that_sends_no_offer_sends_every_asset() {
    let scratch = Scratch::new();
    let mut pair = pair(&scratch);
    let album = pair.album;
    let one = photo(&scratch, &mut pair.a, album, "one.jpg");
    let two = photo(&scratch, &mut pair.a, album, "two.jpg");
    let none = BTreeSet::new();
    let (_, pulled) = sync_once(&mut pair, &none, BTreeSet::new()).await;
    assert_eq!(sorted(pulled.applied), sorted(vec![one, two]));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds");
    let options = OfferOptions {
        peer: listener.local_addr().expect("bound").to_string(),
        to: Some(pair.b.device_id()),
        assets: BTreeSet::new(),
    };
    let (a_versions, b_versions) = (
        MemoryDirectoryVersions::default(),
        MemoryDirectoryVersions::default(),
    );
    let local_a = LocalPeer {
        device_id: pair.a.device_id(),
        signer: pair.a.device_signer(),
        trust: &pair.trust,
        versions: &a_versions,
    };
    let local_b = LocalPeer {
        device_id: pair.b.device_id(),
        signer: pair.b.device_signer(),
        trust: &pair.trust,
        versions: &b_versions,
    };
    // A receiver that offers nothing: it opens the session with no offer and pulls the listing.
    let limiter = HandshakeLimiter::default();
    let no_offer = async {
        let (tcp, _) = listener.accept().await.expect("a peer dials");
        let channel = capsule_sdk::peering::accept(tcp, local_b, &limiter)
            .await
            .expect("verified");
        let client = PeerClient::open(channel).await.expect("the listing reads");
        let mut artifacts = Vec::new();
        for offered in client.listing().to_vec() {
            let mut held = Vec::new();
            let bytes = pull_over_channel(&client, &offered.address, offered.len, &mut held)
                .await
                .expect("pulled");
            artifacts.push(bytes);
        }
        client.finish().await.expect("done");
        (client.peer().artifact_secret.clone(), artifacts)
    };
    let (offered, (secret, artifacts)) = tokio::join!(
        offer_over_peer(&pair.a, local_a, &pair.trust, &options),
        no_offer
    );
    assert!(offered.failed.is_empty(), "{:?}", offered.failed);
    assert_eq!(offered.excluded, 0);
    assert_eq!(sorted(offered.assets.clone()), sorted(vec![one, two]));

    let exporter = pair
        .trust
        .directory
        .device(&pair.a.device_id())
        .expect("a is listed")
        .dsk_public
        .clone();
    let heads = pair.b.local_heads();
    let local = LocalLibrary {
        heads: &heads,
        tombstones: &none,
    };
    assert_eq!(artifacts.len(), 1);
    let restored = ingest(&artifacts[0], &secret, &exporter, local).expect("ingests");
    assert_eq!(sorted(restored.identical), sorted(vec![one, two]));
    assert!(restored.applied.is_empty());
}

/// `--asset` narrows the artifact to the named assets, and one this library does not hold is
/// reported rather than silently dropped.
#[tokio::test]
async fn an_offer_carries_only_the_named_assets_and_reports_one_not_held() {
    let scratch = Scratch::new();
    let mut pair = pair(&scratch);
    let album = pair.album;
    let named = photo(&scratch, &mut pair.a, album, "named.jpg");
    let _other = photo(&scratch, &mut pair.a, album, "other.jpg");
    let absent = Uuid::from_u128(0xAB5E);

    let (offered, pulled) = sync_once(
        &mut pair,
        &BTreeSet::new(),
        [named, absent].into_iter().collect(),
    )
    .await;
    assert_eq!(offered.assets, vec![named]);
    assert_eq!(offered.failed.len(), 1);
    assert_eq!(offered.failed[0].kind, "not_held");
    assert_eq!(offered.failed[0].subject, absent.to_string());
    assert_eq!(pulled.applied, vec![named]);
}

/// An offer to a device that is not the `--to` one sends nothing, and the receiver's session
/// still ends cleanly with nothing pulled.
#[tokio::test]
async fn an_offer_to_another_device_than_named_sends_nothing() {
    let scratch = Scratch::new();
    let mut pair = pair(&scratch);
    let album = pair.album;
    photo(&scratch, &mut pair.a, album, "held.jpg");
    let listener = PeerListener::bind(
        "127.0.0.1:0".parse().expect("an address"),
        Duration::from_secs(30),
    )
    .await
    .expect("binds");
    let options = OfferOptions {
        peer: listener.local_addr().expect("bound").to_string(),
        to: Some(Uuid::from_u128(0x0DD)),
        assets: BTreeSet::new(),
    };
    let versions = MemoryDirectoryVersions::default();
    let none = BTreeSet::new();
    let local_a = LocalPeer {
        device_id: pair.a.device_id(),
        signer: pair.a.device_signer(),
        trust: &pair.trust,
        versions: &versions,
    };
    let (offered, pulled) = tokio::join!(
        offer_over_peer(&pair.a, local_a, &pair.trust, &options),
        receive(&mut pair.b, &pair.trust, &versions, &none, 0, &listener)
    );
    assert_eq!(offered.artifact, None);
    assert_eq!(offered.failed[0].kind, "peer_mismatch");
    assert!(pulled.artifacts.is_empty() && pulled.applied.is_empty());
    assert!(pulled.failed.is_empty(), "{:?}", pulled.failed);
}

/// A pull with no peer before the listen timeout reports it and writes nothing.
#[tokio::test]
async fn a_pull_with_no_peer_reports_the_timeout() {
    let scratch = Scratch::new();
    let mut pair = pair(&scratch);
    let listener = PeerListener::bind(
        "127.0.0.1:0".parse().expect("an address"),
        Duration::from_millis(50),
    )
    .await
    .expect("binds");
    let versions = MemoryDirectoryVersions::default();
    let pulled = receive(
        &mut pair.b,
        &pair.trust,
        &versions,
        &BTreeSet::new(),
        0,
        &listener,
    )
    .await;
    assert_eq!(pulled.peer, None);
    assert_eq!(pulled.failed.len(), 1);
    assert_eq!(pulled.failed[0].kind, "peering_timeout");
    assert!(pulled.has_failures());
}

/// A listener that fails before a peer is verified pulled nothing, so the run fails: the
/// failure is in `failed`, not only among the refusals.
#[test]
fn a_listener_failure_fails_the_pull() {
    let mut summary = PullSummary::default();
    let client: Option<()> = admitted_client(Admitted::Failed, "127.0.0.1:9", &mut summary);
    assert!(client.is_none());
    assert_eq!(summary.failed.len(), 1);
    assert_eq!(summary.failed[0].kind, "peering_io");
    assert_eq!(summary.failed[0].subject, "127.0.0.1:9");
    assert!(summary.has_failures());
}

/// A completed handshake is handed back and records no failure.
#[test]
fn an_admitted_session_records_no_failure() {
    let mut summary = PullSummary::default();
    let source = "127.0.0.1:9".parse().expect("an address");
    let client = admitted_client(Admitted::Session(source, 7_u8), "127.0.0.1:9", &mut summary);
    assert_eq!(client, Some(7));
    assert!(!summary.has_failures());
}

/// A tombstone store that cannot be read refuses the ingest: `peer pull` fails before it opens
/// a session, binds, or touches the network, rather than ingesting against an empty set.
#[tokio::test]
async fn an_unreadable_tombstone_store_refuses_the_pull() {
    let scratch = Scratch::new();
    let db = scratch.db().await;
    sea_orm::ConnectionTrait::execute_unprepared(&db, "DROP TABLE peering_tombstones")
        .await
        .expect("drops the table");
    assert!(matches!(
        ingest_tombstones(&db).await,
        Err(PeerError::Tombstones(_))
    ));

    let mut workspace =
        Workspace::create_with_params(&scratch.path().join("lib"), b"pw", FAST).expect("a library");
    // Nothing answers here, and no session is stored: the refusal must come first.
    let remote = RemoteConfig {
        auth_endpoint: "http://127.0.0.1:9/v1/auth".to_owned(),
        sync_endpoint: "http://127.0.0.1:9".to_owned(),
        upload_endpoint: "http://127.0.0.1:9/v1/upload".to_owned(),
        albums_endpoint: "http://127.0.0.1:9/v1/albums".to_owned(),
        protocol_version: capsule_core::crypto::primitives::PROTOCOL_VERSION.to_owned(),
    };
    let store = SessionStore::new(scratch.path().join("no-session.json"));
    let versions = DbDirectoryVersions::open(scratch.db().await, workspace.device_id())
        .await
        .expect("the floor opens");
    let listener = PeerListener::bind(
        "127.0.0.1:0".parse().expect("an address"),
        Duration::from_secs(30),
    )
    .await
    .expect("binds");
    let refused = pull(&remote, &store, &mut workspace, &db, &versions, &listener).await;
    assert!(
        matches!(refused, Err(PeerError::Tombstones(_))),
        "{refused:?}"
    );
}

/// The two sync legs parse, with `--json`, and `peer pull` requires the address it listens on.
#[test]
fn the_sync_commands_parse() {
    let device = Uuid::from_u128(1).to_string();
    let asset = Uuid::from_u128(2).to_string();
    for args in [
        vec![
            "capsule",
            "peer",
            "offer",
            "--library",
            "lib",
            "--peer",
            "192.168.1.2:7420",
            "--to",
            &device,
            "--asset",
            &asset,
            "--json",
        ],
        vec![
            "capsule",
            "peer",
            "offer",
            "--library",
            "lib",
            "--peer",
            "h:1",
        ],
        vec![
            "capsule",
            "peer",
            "pull",
            "--library",
            "lib",
            "--listen",
            "0.0.0.0:7420",
            "--listen-timeout",
            "60",
            "--json",
        ],
    ] {
        assert!(
            crate::cli::try_parse_from(false, &args).is_ok(),
            "{args:?} parses"
        );
    }
    for args in [
        vec!["capsule", "peer", "pull", "--library", "lib"],
        vec!["capsule", "peer", "offer", "--library", "lib"],
    ] {
        assert!(
            crate::cli::try_parse_from(false, &args).is_err(),
            "{args:?} is refused"
        );
    }
}
