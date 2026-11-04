//! **E2E case 4** — federation cross-server pull (design/module-map.md, "E2E Test Surface").
//!
//! Two federating servers, `alice.test` and `bob.test`, each the real composition root under
//! the memory profile with its own operational key. Each operator pins the other's key as
//! `capsule-server peer pin` does, from the key the other's `server-info` publishes. Alice
//! pushes an asset into her album, lists Bob's account on its roster and mints a capability for
//! Bob's server; Bob posts it through the CLI's own `federation subscribe` orchestration, whose
//! result is the document `capsule federation subscribe --json` prints; and one round of Bob's
//! server's pull worker pulls the album from Alice's server. Bob's client then renders what
//! his server pulled through the CLI's `federation assets` and `federation metadata`
//! orchestration: the album's asset, as its breadcrumb, and the metadata blob the pull stored,
//! byte for byte the one Alice's record commits to.
//!
//! # The wire between the servers is the production one
//!
//! Bob's puller dials `https://alice.test/` over the egress client `serve` builds. Alice's
//! server sits behind a [`TlsFront`] holding a certificate for `alice.test` under a private CA,
//! as a deployment sits behind its TLS proxy, and the `test-egress` seam
//! (`capsule_server::federation::pull::remote::test_egress`) resolves that host to the front
//! and trusts the CA. The origin pin, the HTTPS-only client, the refused redirects and the
//! certificate check are production's.
//!
//! # No case waits on the tick
//!
//! The worker's loop runs a round every `PULL_TICK`, and a job with no schedule is due at once.
//! The case never spawns the loop: it runs one round through `assembled.pull_worker`, so the
//! pull starts the moment the subscription exists and the case reads its outcome directly.
//!
//! # What is not asserted here
//!
//! Who else may read the pulled album: that a non-subscriber is told nothing is the server's
//! own `tests/subscriptions.rs`. What this case proves is the round trip — the capability
//! crossing servers, the signed presentations verifying under the pinned keys, every entry of a
//! non-empty feed passing Bob's re-validation ([`JobOutcome::Pulled`]), and Bob's client
//! reading back exactly what Alice's server served.

use capsule_cli::federation::{self, Subscribed};
use capsule_cli::remote::RemoteConfig;
use capsule_cli::session::SessionStore;
use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::keys::AmkVersion;
use capsule_core::crypto::membership::{AlbumRoster, MemberRole, RosterMember, SignedAlbumRoster};
use capsule_e2e::federation::{SECOND_ED25519_DER, TlsFront};
use capsule_e2e::push::push_asset;
use capsule_e2e::{Device, JWT_ED25519_DER, PROTOCOL_VERSION, Server, entry_for};
use capsule_sdk::albums::{AlbumClient, AlbumTransport};
use capsule_sdk::rest::types::{MintCapabilityRequest, WireScope};
use capsule_server::federation::pull::remote::test_egress;
use capsule_server::federation::pull::{JobKey, JobOutcome};
use capsule_server::federation::{PeerId, parse_signing_key};
use capsule_server::store::AlbumId;

/// Alice's server: the album's home.
const ALICE: &str = "alice.test";

/// Bob's server: the peer that pulls.
const BOB: &str = "bob.test";

/// Pin `peer`'s operational key on `on`, read from the key `peer`'s `server-info` publishes —
/// the act `capsule-server peer pin ORIGIN SIGNING_KEY` performs, over the same peer store.
async fn pin(on: &Server, peer: &Server, peer_id: &str) {
    let info: serde_json::Value = reqwest::get(format!(
        "{}/.well-known/capsule/server-info",
        peer.base_url()
    ))
    .await
    .expect("server-info answers")
    .json()
    .await
    .expect("server-info is JSON");
    assert_eq!(info["server_id"], peer_id, "the peer names itself: {info}");
    let key = parse_signing_key(
        info["signing_key"]
            .as_str()
            .expect("server-info publishes a signing key"),
    )
    .expect("the published key is a raw Ed25519 key");
    on.assembled
        .maintenance
        .peers
        .pin(&PeerId::parse(peer_id).expect("a host name"), key)
        .await
        .expect("the peer store records the pin");
}

/// Publish the first roster of `owner`'s default album, naming `member` a reader, signed by
/// the owner's proposer device.
async fn share_with(owner: &Device, server: &Server, member: uuid::Uuid) {
    let roster = AlbumRoster {
        album_id: owner.workspace.default_album_id(),
        roster_version: 1,
        amk_epoch: AmkVersion(1),
        attested_by_user: owner.user_id,
        attested_by_device: owner.proposer_id,
        attested_at: jiff::Timestamp::now().to_string(),
        members: vec![RosterMember {
            user_id: member,
            role: MemberRole::Reader,
        }],
    };
    let signed = SignedAlbumRoster::sign(roster, &owner.proposer).expect("the roster signs");
    let published = AlbumClient::new(AlbumTransport::with_session(
        owner.session.clone(),
        server.albums_base(),
    ))
    .publish_roster(&signed)
    .await
    .expect("the owner's roster is accepted");
    assert_eq!(published.member_count, 1);
}

#[tokio::test]
async fn e2e_case_4_bobs_server_pulls_alices_album_with_the_capability_bob_subscribed_with() {
    // Alice's server, and the TLS front its federation origin answers on.
    let alice_server = Box::pin(Server::boot_federating(ALICE, JWT_ED25519_DER)).await;
    let front = TlsFront::start(ALICE, alice_server.address()).await;
    test_egress::route(ALICE, front.address());
    test_egress::trust_root(front.ca_der().to_vec());

    // Bob's server, assembled after the route exists so its egress client carries it.
    let bob_server = Box::pin(Server::boot_federating(BOB, SECOND_ED25519_DER)).await;
    let worker = bob_server
        .assembled
        .pull_worker
        .clone()
        .expect("a federating deployment assembles the pull worker");

    // Each operator pins the other.
    pin(&bob_server, &alice_server, ALICE).await;
    pin(&alice_server, &bob_server, BOB).await;

    // Alice's album holds an asset, and Bob's account is on its roster.
    let mut alice = Box::pin(Device::register(&alice_server, "alice")).await;
    let bob = Box::pin(Device::register(&bob_server, "bob")).await;
    let asset = alice.import_jpeg("shared.jpg");
    Box::pin(push_asset(&alice, &alice_server, &asset)).await;
    assert!(
        entry_for(&alice.feed(&alice_server).await, &asset).is_some(),
        "Alice's own feed carries the asset, so the album Bob pulls is not empty"
    );
    share_with(&alice, &alice_server, bob.user_id).await;

    // Alice mints a capability for Bob's server, carrying Bob's membership.
    let album = alice.workspace.default_album_id();
    let minted = alice
        .generated(&alice_server)
        .issue_capability(
            &album.to_string(),
            PROTOCOL_VERSION,
            None,
            &MintCapabilityRequest {
                peer: BOB.to_owned(),
                member: bob.user_id.to_string(),
                scope: WireScope::Read,
                ttl_seconds: None,
                renewable_until: None,
            },
        )
        .await
        .expect("the owner mints a capability for Bob's server")
        .into_inner();
    assert_eq!(minted.peer, BOB);
    assert_eq!(minted.album_id, album.to_string());

    // Bob subscribes through the CLI, over the session `capsule auth login` persists.
    let home = tempfile::tempdir().expect("a temp CLI home");
    let store = SessionStore::new(home.path().join("session.json"));
    let signed_in = bob.login_again(&bob_server).await;
    store
        .save(&signed_in.export().await.expect("a live session exports"))
        .expect("the session persists");
    let remote = RemoteConfig {
        auth_endpoint: bob_server.auth_base(),
        sync_endpoint: bob_server.base_url().to_owned(),
        upload_endpoint: bob_server.upload_base(),
        albums_endpoint: bob_server.albums_base(),
        protocol_version: PROTOCOL_VERSION.to_owned(),
    };
    let subscribed: Subscribed = federation::subscribe(&remote, &store, ALICE, &minted.token)
        .await
        .expect("`capsule federation subscribe` records the subscription");
    // The document `--json` prints.
    let printed = serde_json::to_value(&subscribed).expect("the report serializes");
    assert_eq!(printed["created"], true, "{printed:#}");
    assert_eq!(printed["subscription"]["peer"], ALICE);
    assert_eq!(printed["subscription"]["album_id"], album.to_string());
    assert_eq!(printed["subscription"]["jti"], minted.jti);
    assert_eq!(printed["subscription"]["live"], true);
    assert!(
        printed.to_string().find(&minted.token).is_none(),
        "the report never carries the token"
    );

    // One round of Bob's server's worker: the job is due at once and pulls over TLS.
    let job = JobKey::new(PeerId::new(ALICE), AlbumId::new(album.to_string().as_str()));
    let round = worker.tick().await;
    let outcome = round
        .outcomes
        .iter()
        .find(|(key, _)| key == &job)
        .map(|(_, outcome)| outcome.clone());
    assert!(
        matches!(outcome, Some(JobOutcome::Pulled { .. })),
        "Bob's server pulls Alice's album on the first round: {round:#?}"
    );

    // The schedule moved past the pull, so the next round leaves the job alone.
    let again = worker.tick().await;
    assert!(
        again
            .outcomes
            .iter()
            .any(|(key, outcome)| key == &job && *outcome == JobOutcome::NotDue),
        "a pulled job is not due again at once: {again:#?}"
    );

    // And Bob still follows the album.
    let listed = federation::list(&remote, &store)
        .await
        .expect("`capsule federation list` answers");
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert!(listed[0].live);

    // Bob's client renders what his server pulled: the album's asset, as the document
    // `capsule federation assets --json` prints, with the head Alice's own feed carries.
    let alice_feed = alice.feed(&alice_server).await;
    let alices = entry_for(&alice_feed, &asset).expect("Alice's feed carries the asset");
    let pulled = federation::assets(&remote, &store, ALICE, &album.to_string())
        .await
        .expect("`capsule federation assets` lists the pulled album");
    let printed = serde_json::to_value(&pulled).expect("the report serializes");
    assert_eq!(printed["peer"], ALICE, "{printed:#}");
    assert_eq!(printed["album_id"], album.to_string());
    let assets = printed["assets"].as_array().expect("a list of assets");
    assert_eq!(assets.len(), 1, "the one asset Alice pushed: {printed:#}");
    assert_eq!(assets[0]["asset_id"], asset.to_string());
    assert_eq!(assets[0]["author"], alice.user_id.to_string());
    assert_eq!(
        assets[0]["head"],
        hash_bytes(&alices.manifest_cbor).to_hex(),
        "the breadcrumb's head is the record Alice's server serves"
    );

    // And the metadata blob the pull stored, byte for byte the one Alice's record commits to.
    let metadata = federation::metadata(
        &remote,
        &store,
        ALICE,
        &album.to_string(),
        &asset.to_string(),
    )
    .await
    .expect("`capsule federation metadata` fetches the pulled blob");
    assert_eq!(
        hash_bytes(&metadata).to_hex(),
        String::from_utf8(alices.metadata_blob.clone()).expect("a hex content address"),
        "the blob Bob's server holds is the one Alice's asset carries"
    );
}
