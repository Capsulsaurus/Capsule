//! Deterministic tests for LAN peering, one group per item of the peering doc's Validation list,
//! plus the cross-module LAN-sync-then-server-reconcile case in its in-process shape.
//!
//! - Discovery is **mocked** ([`MockDiscovery`]) — it is inherently non-deterministic on a real
//!   LAN, so the seam is what we test: the opaque, rotating advertisement.
//! - The mTLS handshake is **real, in-process** — a rustls client and server over a localhost
//!   socket with generated certs — so wrong-cert rejection and the application-layer hybrid check
//!   are exercised against an actual TLS 1.3 channel.
//! - Delta scoping, ranged-GET resume, restore ingest, tombstones, partial assets and stale-revival
//!   quarantine run over the real backup artifact + restore path.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::time::{Duration, Instant};

use capsule_core::backup::{BackupArtifact, BackupAsset, BackupError};
use capsule_core::crypto::encryption::{encrypt_asset_rekey, seal_metadata_blob};
use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::keys::{
    Amk, AmkVersion, DeviceDirectory, DeviceEntry, DirectoryCore, HybridSigningKey,
};
use capsule_core::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use capsule_core::crypto::provenance::{
    ASSET_MANIFEST_VERSION, Action, KeyMode, ManifestCore, ProvenanceRecord,
};
use jiff::Timestamp;
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

use super::channel::{
    ArtifactSecret, HandshakeRole, LocalPeer, Message, OFFER_EXCHANGE_PROTOCOL, Offered, Offering,
    PEERING_PROTOCOL, PEERING_PROTOCOL_MIN, PeerChannel, PeerClient, PeerHello, PinnedTrust,
    UpdateDevice, VerifiedPeer, VersionWindow, accept, connect, hello_proof_message, serve,
    verify_hello,
};
use super::delegation::DelegationError;
use super::delta::{
    MAX_OFFER_PAGES, MIN_OFFER_BUCKET, OFFER_PAGE_CAP, Offer, cut_to_missing, decode_offer_page,
    head_address, missing_from, offer_bucket, symmetric_difference,
};
use super::discovery::{
    Discovery, MockDiscovery, OpaqueAdvertisement, SERVICE_TYPE, rotation_epoch,
};
use super::session::{
    DirectoryVersions, HandshakeLimiter, MAX_TRACKED_SOURCES, MemoryDirectoryVersions, PeerSession,
};
use super::transfer::{
    ArtifactBlobSource, DeltaExport, LocalLibrary, artifact_address, build_delta_artifact, ingest,
    pull_artifact, pull_over_channel,
};
use super::{PeeringError, is_lan};
use crate::fetch::FetchScope;

const ALBUM: u128 = 0xA1;
const USER: u128 = 0x05E2;

/// The artifact secret a session would export; fixed here, where there is no TLS session.
fn secret() -> ArtifactSecret {
    ArtifactSecret::from_bytes([0x5E; 32])
}

/// A two-device single-user fixture: the User IK, two device keys, a write-tier key, and an AMK.
struct Fx {
    ik: HybridSigningKey,
    dev_a: HybridSigningKey,
    dev_b: HybridSigningKey,
    id_a: Uuid,
    id_b: Uuid,
    write: HybridSigningKey,
    amk: [u8; 32],
}

impl Fx {
    fn new() -> Self {
        Self {
            ik: HybridSigningKey::from_seed_bytes(&[10; 32], &[11; 32]),
            dev_a: HybridSigningKey::from_seed_bytes(&[20; 32], &[21; 32]),
            dev_b: HybridSigningKey::from_seed_bytes(&[30; 32], &[31; 32]),
            id_a: Uuid::from_u128(0x0A),
            id_b: Uuid::from_u128(0x0B),
            write: HybridSigningKey::from_seed_bytes(&[40; 32], &[41; 32]),
            amk: [0x55; 32],
        }
    }

    fn amks(&self) -> BTreeMap<(Uuid, u32), [u8; 32]> {
        let mut m = BTreeMap::new();
        m.insert((Uuid::from_u128(ALBUM), 1u32), self.amk);
        m
    }

    fn entry(&self, id: Uuid, key: &HybridSigningKey, revoked: bool) -> DeviceEntry {
        DeviceEntry {
            device_id: id,
            dsk_public: key.verifying_key(),
            dek_public: None,
            added_at: "2026-05-01T00:00:00Z".into(),
            revoked_at: revoked.then(|| "2026-05-15T00:00:00Z".to_string()),
            unknown: Default::default(),
        }
    }

    /// The shared directory listing both devices, signed by the User IK. `revoke_b` marks device
    /// B revoked; `signer` lets a test sign it with a *foreign* IK.
    fn directory_signed_by(
        &self,
        version: u64,
        revoke_b: bool,
        signer: &HybridSigningKey,
    ) -> DeviceDirectory {
        DirectoryCore {
            user_id: Uuid::from_u128(USER),
            directory_version: version,
            updated_at: "2026-05-16T00:00:00Z".into(),
            devices: vec![
                self.entry(self.id_a, &self.dev_a, false),
                self.entry(self.id_b, &self.dev_b, revoke_b),
            ],
            unknown: Default::default(),
        }
        .sign(signer)
    }

    fn trust_at(&self, version: u64, revoke_b: bool) -> PinnedTrust {
        PinnedTrust {
            user_ik: self.ik.verifying_key(),
            directory: self.directory_signed_by(version, revoke_b, &self.ik),
        }
    }

    fn trust(&self, revoke_b: bool) -> PinnedTrust {
        self.trust_at(1, revoke_b)
    }

    /// Build a `versions`-long provenance chain (Create, then MetadataUpdate for each later
    /// version) over a single ciphertext blob, returning the chain, the ciphertext, and the
    /// metadata blob. Every record commits to the metadata blob's hash. Later versions share the
    /// exact earlier records, so an older device's head is literally an ancestor record in a newer
    /// device's chain.
    fn records(
        &self,
        asset: u128,
        plaintext: &[u8],
        versions: usize,
    ) -> (Vec<ProvenanceRecord>, Vec<u8>, Vec<u8>) {
        self.records_committing(asset, plaintext, versions, true)
    }

    /// As [`records`](Self::records), choosing whether the records commit to the metadata hash.
    fn records_committing(
        &self,
        asset: u128,
        plaintext: &[u8],
        versions: usize,
        commit_meta: bool,
    ) -> (Vec<ProvenanceRecord>, Vec<u8>, Vec<u8>) {
        let amk = Amk::from_bytes(self.amk);
        let file_id = Uuid::from_u128(asset);
        let (enc, ct, _fk) = encrypt_asset_rekey(&amk, &file_id, plaintext, None).unwrap();
        let meta = seal_metadata_blob(&amk, &file_id, b"{sidecar}", None)
            .unwrap()
            .0;
        let metadata_blob_hash = commit_meta.then(|| hash_bytes(&meta));

        let mut chain: Vec<ProvenanceRecord> = Vec::new();
        let mut prior: Option<Hash32> = None;
        for i in 0..versions {
            let action = if i == 0 {
                Action::Create
            } else {
                Action::MetadataUpdate
            };
            let core = ManifestCore {
                version: ASSET_MANIFEST_VERSION.into(),
                crypto_suite_id: CRYPTO_SUITE_ID,
                protocol_version: PROTOCOL_VERSION.into(),
                file_id,
                album_id: Uuid::from_u128(ALBUM),
                amk_version: AmkVersion(1),
                ciphertext_hash: enc.ciphertext_hash,
                plaintext_size: enc.plaintext_size,
                chunk_size: enc.chunk_size,
                nonce_prefix: enc.nonce_prefix,
                key_mode: KeyMode::Derived,
                wrapped_file_key: None,
                metadata_blob_hash,
                created_by_user: Uuid::from_u128(USER),
                created_by_device: self.id_a,
                client_version: "t".into(),
                timestamp: "2026-05-31T00:00:00Z".into(),
                action,
                prior_provenance_hash: prior,
                upgraded_from: None,
                unknown: Default::default(),
                retention_until: None,
            };
            let manifest = core.sign(&self.dev_a, &self.write).unwrap();
            let record = ProvenanceRecord::new(file_id, manifest, prior);
            prior = Some(record.record_hash());
            chain.push(record);
        }
        (chain, ct, meta)
    }

    /// A delta-scoped artifact of `assets` exported by device A under the fixture secret.
    fn artifact_of(&self, assets: &[BackupAsset]) -> Vec<u8> {
        let delta: BTreeSet<Hash32> = assets
            .iter()
            .map(|a| a.provenance.last().unwrap().manifest.core.ciphertext_hash)
            .collect();
        let export = DeltaExport {
            assets,
            delta: &delta,
            scope: FetchScope::MetadataThumbnailsOriginal,
            amks: &self.amks(),
            exporter_device: self.id_a,
            source_library_version: "1".into(),
            export_timestamp: "2026-05-31T00:00:00Z".into(),
        };
        build_delta_artifact(&export, &secret(), &self.dev_a).unwrap()
    }
}

/// Assemble a `BackupAsset` from a chain slice + its blob and metadata.
fn asset_from(chain: &[ProvenanceRecord], ct: &[u8], meta: &[u8]) -> BackupAsset {
    let head = chain.last().unwrap();
    BackupAsset {
        album_id: head.manifest.core.album_id,
        asset_id: head.asset_id,
        ciphertext: std::sync::Arc::new(ct.to_vec()),
        metadata_blob: meta.to_vec(),
        provenance: chain.to_vec(),
        receipts: Vec::new(),
        attestations: Vec::new(),
    }
}

fn address(chain: &[ProvenanceRecord]) -> Hash32 {
    chain.last().unwrap().manifest.core.ciphertext_hash
}

fn head_hash(chain: &[ProvenanceRecord]) -> Hash32 {
    chain.last().unwrap().record_hash()
}

/// A local library view with no tombstones.
fn library<'a>(
    heads: &'a BTreeMap<Uuid, Hash32>,
    tombstones: &'a BTreeSet<Uuid>,
) -> LocalLibrary<'a> {
    LocalLibrary { heads, tombstones }
}

fn ip(last: u8) -> IpAddr {
    IpAddr::from([192, 168, 1, last])
}

// ── Advertisement (unit) ──────────────────────────────────────────────────────

/// Generate an advertisement; assert it carries no user handle, device name or model.
/// Re-generate after the rotation interval and after a reboot; assert a new opaque identifier.
#[test]
fn mdns_advert_is_opaque_and_rotates() {
    let seed = [0x77u8; 32];
    let user_handle = "alice@example.com";
    let device_name = "Alice's iPhone";
    let model = "iPhone16,2";

    let e0 = rotation_epoch(0xBEEF, 1_760_000_000);
    let advert = OpaqueAdvertisement::for_epoch(&seed, e0, 5555);

    // The advertised name is opaque hex — it leaks neither identity substring, and the service
    // type is the fixed, non-identifying label.
    let name = advert.advertised_name();
    assert_eq!(name.len(), 32, "opaque 128-bit hex instance name");
    assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(!name.contains(user_handle));
    assert!(!name.to_lowercase().contains("alice"));
    assert!(!name.contains(device_name));
    assert!(!name.to_lowercase().contains("iphone"));
    assert!(!name.contains(model));
    assert!(!name.contains('@'));
    assert_eq!(advert.service_type(), SERVICE_TYPE);
    assert!(!SERVICE_TYPE.contains('@'));

    // Same epoch → same name (stable within the rotation window).
    assert_eq!(
        OpaqueAdvertisement::for_epoch(&seed, e0, 5555).advertised_name(),
        name
    );

    // A new rotation epoch → a new, unlinkable opaque name.
    let e1 = rotation_epoch(0xBEEF, 1_760_000_000 + 86_400); // next day
    let rotated = OpaqueAdvertisement::for_epoch(&seed, e1, 5555);
    assert_ne!(
        rotated.advertised_name(),
        name,
        "rotates across the interval"
    );

    // Rebooting rotates immediately (per-boot), even within the same day.
    let reboot = rotation_epoch(0xF00D, 1_760_000_000);
    assert_ne!(reboot, e0);
    assert_ne!(
        OpaqueAdvertisement::for_epoch(&seed, reboot, 5555).advertised_name(),
        name
    );

    // Same boot, same day → epoch is stable.
    assert_eq!(
        rotation_epoch(0xBEEF, 1_760_000_000),
        rotation_epoch(0xBEEF, 1_760_000_000 + 3_600)
    );
}

/// No name outlives 24 h: across a sweep of start times, the epoch an instant later by a full day
/// always differs, and no boot's epoch on one day equals another boot's on any nearby day.
#[test]
fn mdns_advert_rotates_at_least_every_24h_and_never_collides_across_boots() {
    for start in (1_760_000_000i64..1_760_000_000 + 86_400).step_by(3_607) {
        assert_ne!(
            rotation_epoch(7, start),
            rotation_epoch(7, start + 86_400),
            "a name never lives a full day"
        );
    }
    let seed = [3u8; 32];
    let mut names = BTreeSet::new();
    for boot in [0u64, 1, 2, u64::MAX] {
        for day in 0..4i64 {
            let epoch = rotation_epoch(boot, 1_760_000_000 + day * 86_400);
            assert!(
                names.insert(
                    OpaqueAdvertisement::for_epoch(&seed, epoch, 1)
                        .advertised_name()
                        .to_string()
                ),
                "every (boot, day) yields a distinct name"
            );
        }
    }
}

/// The mocked discovery seam round-trips an opaque advertisement to a browsing peer.
#[test]
fn mock_discovery_advertises_and_browses() {
    let disc = MockDiscovery::new();
    let advert = OpaqueAdvertisement::for_epoch(&[1; 32], rotation_epoch(1, 0), 6000);
    let addr = "127.0.0.1:6000".parse().unwrap();
    disc.advertise(&advert, addr).unwrap();

    let found = disc.browse().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].instance_name, advert.advertised_name());
    assert_eq!(found[0].addr, addr);
}

// ── Handshake (unit + real in-process) ────────────────────────────────────────

/// Drive a **real** in-process mutual TLS 1.3 handshake between device A (client) and device B
/// (server), each pinning its own trust and a fresh anti-rollback floor, and return both sides'
/// verification outcomes.
async fn run_handshake(
    fx: &Fx,
    server_trust: PinnedTrust,
    client_trust: PinnedTrust,
) -> (
    Result<VerifiedPeer, PeeringError>,
    Result<VerifiedPeer, PeeringError>,
) {
    run_limited_handshake(fx, server_trust, client_trust, HandshakeLimiter::default()).await
}

async fn run_limited_handshake(
    fx: &Fx,
    server_trust: PinnedTrust,
    client_trust: PinnedTrust,
    limiter: HandshakeLimiter,
) -> (
    Result<VerifiedPeer, PeeringError>,
    Result<VerifiedPeer, PeeringError>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let dev_b = fx.dev_b.clone();
    let id_b = fx.id_b;
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let versions = MemoryDirectoryVersions::default();
        let local = LocalPeer {
            device_id: id_b,
            signer: &dev_b,
            trust: &server_trust,
            versions: &versions,
        };
        accept(tcp, local, &limiter)
            .await
            .map(PeerChannel::into_peer)
    });

    let tcp = TcpStream::connect(addr).await.unwrap();
    let versions = MemoryDirectoryVersions::default();
    let local = LocalPeer {
        device_id: fx.id_a,
        signer: &fx.dev_a,
        trust: &client_trust,
        versions: &versions,
    };
    let client_res = connect(tcp, local).await.map(PeerChannel::into_peer);
    let server_res = server.await.unwrap();
    (server_res, client_res)
}

/// Two device certificates chaining to the same IK — the handshake succeeds, each side learns the
/// other's verified `device_id` over a real TLS 1.3 channel, and both derive the same artifact
/// secret, distinct from the material the proofs signed.
#[tokio::test]
async fn mtls_handshake_succeeds_for_same_ik_devices() {
    let fx = Fx::new();
    let (server_res, client_res) = run_handshake(&fx, fx.trust(false), fx.trust(false)).await;

    let server = server_res.expect("server verifies the client");
    let client = client_res.expect("client verifies the server");
    assert_eq!(server.device_id, fx.id_a, "server learned peer = device A");
    assert_eq!(client.device_id, fx.id_b, "client learned peer = device B");
    // Both sides bound the hybrid identity to the *same* TLS session (RFC 5705 exporter).
    assert_eq!(server.exporter, client.exporter);
    assert_eq!(server.protocol, PEERING_PROTOCOL);
    assert_eq!(client.protocol, PEERING_PROTOCOL);
    // The artifact secret is shared, and is not the value either side signed.
    assert_eq!(
        server.artifact_secret.as_bytes(),
        client.artifact_secret.as_bytes()
    );
    assert_ne!(server.artifact_secret.as_bytes(), &server.exporter);
    assert_eq!(
        format!("{:?}", server.artifact_secret),
        "ArtifactSecret(..)",
        "the secret never reaches a log"
    );
}

/// Two sessions never share an artifact secret: it is fresh per session.
#[tokio::test]
async fn artifact_secret_is_fresh_per_session() {
    let fx = Fx::new();
    let (first, _) = run_handshake(&fx, fx.trust(false), fx.trust(false)).await;
    let (second, _) = run_handshake(&fx, fx.trust(false), fx.trust(false)).await;
    assert_ne!(
        first.unwrap().artifact_secret.as_bytes(),
        second.unwrap().artifact_secret.as_bytes()
    );
}

/// Replace one peer with a **revoked-device** entry — the handshake fails at the hybrid check.
#[tokio::test]
async fn mtls_handshake_rejects_a_revoked_device() {
    let fx = Fx::new();
    // The client (device A) pins a directory in which device B has been revoked.
    let (_server_res, client_res) = run_handshake(&fx, fx.trust(false), fx.trust(true)).await;
    assert!(
        matches!(client_res, Err(PeeringError::RevokedDevice(id)) if id == fx.id_b),
        "a revoked peer cannot complete the handshake: {client_res:?}"
    );
}

/// Replace the pinned directory with one signed by a **foreign** IK — the handshake fails: the
/// peer does not chain to the User IK we trust.
#[tokio::test]
async fn mtls_handshake_rejects_a_foreign_ik() {
    let fx = Fx::new();
    let foreign_ik = HybridSigningKey::from_seed_bytes(&[99; 32], &[98; 32]);
    // The client pins the real IK but a directory actually signed by a foreign IK.
    let client_trust = PinnedTrust {
        user_ik: fx.ik.verifying_key(),
        directory: fx.directory_signed_by(1, false, &foreign_ik),
    };
    let (_server_res, client_res) = run_handshake(&fx, fx.trust(false), client_trust).await;
    assert!(
        matches!(client_res, Err(PeeringError::ForeignIdentity)),
        "a directory not chaining to the pinned IK is rejected: {client_res:?}"
    );
}

/// A peer presenting a directory older than ours is refused over the real channel: the server
/// pins version 1, the client version 2.
#[tokio::test]
async fn mtls_handshake_rejects_a_peer_with_an_older_directory() {
    let fx = Fx::new();
    let (_server_res, client_res) =
        run_handshake(&fx, fx.trust_at(1, false), fx.trust_at(2, false)).await;
    assert!(
        matches!(
            client_res,
            Err(PeeringError::PeerDirectoryStale { theirs: 1, ours: 2 })
        ),
        "a peer on a stale directory is refused: {client_res:?}"
    );
}

/// The handshake rate limit refuses a source address over its budget before any TLS byte.
#[tokio::test]
async fn handshake_rate_limit_refuses_over_budget() {
    let fx = Fx::new();
    let (server_res, client_res) = run_limited_handshake(
        &fx,
        fx.trust(false),
        fx.trust(false),
        HandshakeLimiter::new(0, Duration::from_secs(60)),
    )
    .await;
    assert!(
        matches!(server_res, Err(PeeringError::RateLimited(addr)) if addr.is_loopback()),
        "{server_res:?}"
    );
    assert!(client_res.is_err(), "the refused client gets no channel");
}

/// The limiter admits a budget per source within the window, keeps sources apart, and admits again
/// once the window has passed.
#[test]
fn handshake_limiter_is_per_source_and_slides() {
    let limiter = HandshakeLimiter::new(2, Duration::from_secs(10));
    let t0 = Instant::now();
    assert!(limiter.admit(ip(1), t0));
    assert!(limiter.admit(ip(1), t0 + Duration::from_secs(1)));
    assert!(
        !limiter.admit(ip(1), t0 + Duration::from_secs(2)),
        "third attempt in the window is refused"
    );
    assert!(
        limiter.admit(ip(2), t0 + Duration::from_secs(2)),
        "per source"
    );
    assert!(
        !limiter.admit(ip(1), t0 + Duration::from_secs(9)),
        "still inside the window"
    );
    assert!(
        limiter.admit(ip(1), t0 + Duration::from_secs(10)),
        "the first attempt has aged out"
    );
}

/// A flood of distinct live sources never grows the table past its cap, and never locks a new
/// source out: a full table of live sources evicts the least recently active one, a source
/// that stays tracked keeps its budget, and once the flood ages out idle sources are pruned.
#[test]
fn handshake_limiter_caps_tracked_sources() {
    let limiter = HandshakeLimiter::new(2, Duration::from_secs(10));
    let t0 = Instant::now();
    let at = |n: usize| t0 + Duration::from_millis(n as u64);
    let source = |n: usize| IpAddr::from((n as u32 + 0x0A00_0000).to_be_bytes());
    for n in 0..MAX_TRACKED_SOURCES {
        assert!(limiter.admit(source(n), at(n)));
    }
    assert_eq!(limiter.tracked(), MAX_TRACKED_SOURCES);

    let now = at(MAX_TRACKED_SOURCES);
    assert!(
        limiter.admit(source(MAX_TRACKED_SOURCES), now),
        "a new source is admitted while the table is full of live ones"
    );
    assert_eq!(limiter.tracked(), MAX_TRACKED_SOURCES, "the cap holds");
    assert!(
        limiter.admit(source(MAX_TRACKED_SOURCES), now),
        "the admitted source is tracked with its own budget"
    );
    assert!(
        !limiter.admit(source(MAX_TRACKED_SOURCES), now),
        "the admitted source is held to its budget"
    );
    assert!(
        limiter.admit(source(1), now),
        "a tracked source keeps its budget"
    );
    assert!(
        !limiter.admit(source(1), now),
        "a source that was not evicted keeps the attempts it spent"
    );
    assert!(
        limiter.admit(source(0), now),
        "the least recently active source was the one evicted"
    );
    assert!(
        limiter.admit(source(0), now),
        "the evicted source regains its full budget"
    );
    assert!(!limiter.admit(source(0), now), "and is held to it again");
    assert_eq!(limiter.tracked(), MAX_TRACKED_SOURCES, "the cap holds");

    let later = at(MAX_TRACKED_SOURCES) + Duration::from_secs(11);
    assert!(
        limiter.admit(source(MAX_TRACKED_SOURCES + 1), later),
        "idle sources are pruned to make room"
    );
    assert_eq!(limiter.tracked(), 1, "every idle source was pruned");
}

/// One attacker holding a whole IPv6 /64 cannot lock new peers out: cycling through far more
/// addresses than the table tracks, all live, keeps the table at its cap, and a new peer, in
/// that prefix or outside it, is still admitted.
#[test]
fn handshake_limiter_many_addresses_in_one_prefix_cannot_lock_out() {
    let limiter = HandshakeLimiter::new(2, Duration::from_secs(60));
    let t0 = Instant::now();
    let in_prefix = |n: usize| {
        let mut octets = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        octets[8..].copy_from_slice(&(n as u64).to_be_bytes());
        IpAddr::from(octets)
    };
    for n in 0..MAX_TRACKED_SOURCES * 2 {
        let now = t0 + Duration::from_millis(n as u64);
        assert!(limiter.admit(in_prefix(n), now), "attacker address {n}");
        assert!(limiter.tracked() <= MAX_TRACKED_SOURCES, "the cap holds");
    }
    let now = t0 + Duration::from_millis((MAX_TRACKED_SOURCES * 2) as u64);
    assert!(
        limiter.admit(ip(10), now),
        "a peer outside the prefix is admitted"
    );
    assert!(
        limiter.admit(in_prefix(usize::MAX), now),
        "a new address inside the prefix is admitted"
    );
    assert_eq!(limiter.tracked(), MAX_TRACKED_SOURCES);
}

/// The application-layer check in isolation (deterministic, no socket): the full rejection
/// matrix, including the version window, rollback, and an unknown / bad-proof peer.
#[test]
fn hybrid_check_rejection_matrix() {
    let fx = Fx::new();
    let exporter = [0x42u8; 32];
    let trust = fx.trust(false);
    let versions = MemoryDirectoryVersions::default();

    // Valid hello from device B.
    let good = build_hello(fx.id_b, &fx.dev_b, &exporter, 1);
    assert_eq!(
        verify_as_a(&good, &exporter, &trust, &versions).unwrap(),
        (fx.id_b, PEERING_PROTOCOL.to_string())
    );

    // A peer whose window lies wholly below ours → disjoint, before any identity check.
    let mut old = good.clone();
    old.peering_protocol = "1999-01-01".into();
    old.peering_protocol_min = None;
    assert!(matches!(
        verify_as_a(&old, &exporter, &trust, &versions),
        Err(PeeringError::VersionDisjoint {
            update: UpdateDevice::Peer,
            ..
        })
    ));

    // A malformed window → codec error.
    let mut garbled = good.clone();
    garbled.peering_protocol = "tomorrow".into();
    assert!(matches!(
        verify_as_a(&garbled, &exporter, &trust, &versions),
        Err(PeeringError::Codec(_))
    ));

    // A device id absent from the pinned directory → unknown.
    let stranger = build_hello(Uuid::from_u128(0xDEAD), &fx.dev_b, &exporter, 1);
    assert!(matches!(
        verify_as_a(&stranger, &exporter, &trust, &versions),
        Err(PeeringError::UnknownDevice(_))
    ));

    // The claimed device is real, but the proof was signed by a different key → hybrid fail.
    let imposter_key = HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]);
    let imposter = build_hello(fx.id_b, &imposter_key, &exporter, 1);
    assert!(matches!(
        verify_as_a(&imposter, &exporter, &trust, &versions),
        Err(PeeringError::HybridCheckFailed)
    ));

    // A proof over *different* exporter material (a MITM's session) → hybrid fail.
    let other_session = build_hello(fx.id_b, &fx.dev_b, &[0x11u8; 32], 1);
    assert!(matches!(
        verify_as_a(&other_session, &exporter, &trust, &versions),
        Err(PeeringError::HybridCheckFailed)
    ));

    // Revoked device → rejected even with a valid proof.
    assert!(matches!(
        verify_as_a(&good, &exporter, &fx.trust(true), &versions),
        Err(PeeringError::RevokedDevice(_))
    ));
}

/// A listener that reflects the dialer's own hello back at it gets no channel: the hello names
/// the dialer itself, so it is refused before any key is looked up.
#[test]
fn a_reflected_own_hello_is_refused() {
    let fx = Fx::new();
    let exporter = [0x42u8; 32];
    let trust = fx.trust(false);
    let versions = MemoryDirectoryVersions::default();

    // Device A dials and sends its hello; the listener echoes it back unchanged.
    let own = build_hello_as(HandshakeRole::Dialer, fx.id_a, &fx.dev_a, &exporter, 1);
    let reflected = verify_hello(
        &own,
        HandshakeRole::Listener,
        &exporter,
        fx.id_a,
        &trust,
        &versions,
    );
    assert!(
        matches!(reflected, Err(PeeringError::SelfPeer(id)) if id == fx.id_a),
        "{reflected:?}"
    );
    assert_eq!(PeeringError::SelfPeer(fx.id_a).kind(), "self_peer");

    // A hello naming our own id is refused even with a proof that would otherwise verify.
    let as_listener = build_hello_as(HandshakeRole::Listener, fx.id_a, &fx.dev_a, &exporter, 1);
    assert!(matches!(
        verify_hello(
            &as_listener,
            HandshakeRole::Listener,
            &exporter,
            fx.id_a,
            &trust,
            &versions,
        ),
        Err(PeeringError::SelfPeer(_))
    ));
}

/// The proof is bound to the sender's role: a hello signed as one end never verifies as the
/// other end's, even from the real device over the same session.
#[test]
fn a_hello_with_a_swapped_role_is_refused() {
    let fx = Fx::new();
    let exporter = [0x42u8; 32];
    let trust = fx.trust(false);
    let versions = MemoryDirectoryVersions::default();

    let as_dialer = build_hello_as(HandshakeRole::Dialer, fx.id_b, &fx.dev_b, &exporter, 1);
    let as_listener = build_hello_as(HandshakeRole::Listener, fx.id_b, &fx.dev_b, &exporter, 1);
    let verify = |hello: &PeerHello, from: HandshakeRole| {
        verify_hello(hello, from, &exporter, fx.id_a, &trust, &versions)
    };

    assert_eq!(
        verify(&as_dialer, HandshakeRole::Dialer).unwrap().0,
        fx.id_b
    );
    assert_eq!(
        verify(&as_listener, HandshakeRole::Listener).unwrap().0,
        fx.id_b
    );
    assert!(matches!(
        verify(&as_dialer, HandshakeRole::Listener),
        Err(PeeringError::HybridCheckFailed)
    ));
    assert!(matches!(
        verify(&as_listener, HandshakeRole::Dialer),
        Err(PeeringError::HybridCheckFailed)
    ));
    // The bare exporter, the pre-binding message, no longer verifies under either role.
    let mut unbound = as_dialer.clone();
    unbound.proof = fx.dev_b.sign(&exporter);
    assert!(matches!(
        verify(&unbound, HandshakeRole::Dialer),
        Err(PeeringError::HybridCheckFailed)
    ));
    assert_eq!(HandshakeRole::Dialer.peer(), HandshakeRole::Listener);
    assert_eq!(HandshakeRole::Listener.peer(), HandshakeRole::Dialer);
}

/// Directory anti-rollback: once version 2 (which revokes B) has verified, a stale pinned
/// version 1 (which still lists B as active) is refused, so it cannot readmit B.
#[test]
fn rolled_back_directory_cannot_readmit_a_revoked_device() {
    let fx = Fx::new();
    let exporter = [0x42u8; 32];
    let versions = MemoryDirectoryVersions::default();
    let hello = build_hello(fx.id_b, &fx.dev_b, &exporter, 2);

    assert!(matches!(
        verify_as_a(&hello, &exporter, &fx.trust_at(2, true), &versions),
        Err(PeeringError::RevokedDevice(_))
    ));
    assert_eq!(
        versions.highest().unwrap(),
        2,
        "the verified version is recorded"
    );

    let stale = build_hello(fx.id_b, &fx.dev_b, &exporter, 1);
    assert!(matches!(
        verify_as_a(&stale, &exporter, &fx.trust_at(1, false), &versions),
        Err(PeeringError::DirectoryRollback {
            pinned: 1,
            highest: 2
        })
    ));

    // A floor restored from persistence holds across a restart, and only rises.
    let restored = MemoryDirectoryVersions::starting_at(5);
    restored.record(3).unwrap();
    assert_eq!(restored.highest().unwrap(), 5);
}

/// The handshake hello (with its embedded hybrid signature) round-trips through the JSON framing,
/// and a hello from before the window (no min, no directory version) still decodes.
#[test]
fn peer_hello_round_trips_through_json_framing() {
    let fx = Fx::new();
    let hello = build_hello(fx.id_b, &fx.dev_b, &[0x42u8; 32], 1);
    let bytes = serde_json::to_vec(&hello).unwrap();
    let back: PeerHello = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back.device_id, hello.device_id);
    assert_eq!(back.peering_protocol, PEERING_PROTOCOL);
    assert_eq!(
        back.peering_protocol_min.as_deref(),
        Some(PEERING_PROTOCOL_MIN)
    );
    assert_eq!(back.directory_version, 1);
    // The recovered proof still verifies under device B's key over the exporter material.
    let trust = fx.trust(false);
    let versions = MemoryDirectoryVersions::default();
    assert_eq!(
        verify_as_a(&back, &[0x42u8; 32], &trust, &versions)
            .unwrap()
            .0,
        fx.id_b
    );

    let mut legacy = serde_json::to_value(&hello).unwrap();
    let object = legacy.as_object_mut().unwrap();
    object.remove("peering_protocol_min");
    object.remove("directory_version");
    object.insert("peering_protocol".into(), "2026-07-10".into());
    let legacy: PeerHello = serde_json::from_value(legacy).unwrap();
    assert_eq!(
        legacy.window().unwrap(),
        VersionWindow::new("2026-07-10", "2026-07-10").unwrap()
    );
    assert_eq!(legacy.directory_version, 0);
}

/// Test helper mirroring the private `build_hello` for a dialer: sign the dialer-bound exporter
/// material with a device key.
fn build_hello(
    device_id: Uuid,
    signer: &HybridSigningKey,
    exporter: &[u8; 32],
    directory_version: u64,
) -> PeerHello {
    build_hello_as(
        HandshakeRole::Dialer,
        device_id,
        signer,
        exporter,
        directory_version,
    )
}

/// [`build_hello`] for the sender holding `role`.
fn build_hello_as(
    role: HandshakeRole,
    device_id: Uuid,
    signer: &HybridSigningKey,
    exporter: &[u8; 32],
    directory_version: u64,
) -> PeerHello {
    PeerHello {
        device_id,
        peering_protocol: PEERING_PROTOCOL.to_string(),
        peering_protocol_min: Some(PEERING_PROTOCOL_MIN.to_string()),
        directory_version,
        proof: signer.sign(&hello_proof_message(role, exporter)),
    }
}

/// Verify `hello` as device A (`Fx::id_a`) listening, so the hello comes from a dialer.
fn verify_as_a(
    hello: &PeerHello,
    exporter: &[u8; 32],
    trust: &PinnedTrust,
    versions: &MemoryDirectoryVersions,
) -> Result<(Uuid, String), PeeringError> {
    verify_hello(
        hello,
        HandshakeRole::Dialer,
        exporter,
        Uuid::from_u128(0x0A),
        trust,
        versions,
    )
}

// ── Version window (unit) ─────────────────────────────────────────────────────

/// Overlapping windows pick the highest common version; disjoint windows refuse with the typed
/// error naming the device to update.
#[test]
fn version_window_negotiates_the_highest_common_version() {
    let w = |min: &str, max: &str| VersionWindow::new(min, max).unwrap();
    let ours = w("2026-01-01", "2026-06-01");

    assert_eq!(
        ours.negotiate(&w("2026-03-01", "2026-09-01")).unwrap(),
        "2026-06-01"
    );
    assert_eq!(
        ours.negotiate(&w("2025-01-01", "2026-03-01")).unwrap(),
        "2026-03-01"
    );
    assert_eq!(
        ours.negotiate(&w("2026-06-01", "2026-06-01")).unwrap(),
        "2026-06-01",
        "touching windows meet at their shared edge"
    );

    let newer = w("2026-07-01", "2026-12-01");
    assert!(matches!(
        ours.negotiate(&newer),
        Err(PeeringError::VersionDisjoint {
            update: UpdateDevice::ThisDevice,
            ..
        })
    ));
    assert!(matches!(
        newer.negotiate(&ours),
        Err(PeeringError::VersionDisjoint {
            update: UpdateDevice::Peer,
            ..
        })
    ));

    assert!(VersionWindow::new("2026-06-01", "2026-01-01").is_err());
    assert!(VersionWindow::new("2026-6-1", "2026-07-01").is_err());
    let ours = VersionWindow::ours();
    assert!(ours.min <= ours.max);
}

// ── Delta (unit) ──────────────────────────────────────────────────────────────

/// Two devices with overlapping but distinct content-address sets; the delta is the symmetric
/// difference, and each side's one-way pull is the complement.
#[test]
fn delta_is_the_symmetric_difference() {
    let h = |n: u8| Hash32([n; 32]);
    let a = Offer::new(BTreeSet::from([h(1), h(2), h(3)]), 10);
    let b = Offer::new(BTreeSet::from([h(2), h(3), h(4)]), 12);

    // A is missing what B has and A does not: {4}. B is missing {1}.
    assert_eq!(missing_from(&a, &b), BTreeSet::from([h(4)]));
    assert_eq!(missing_from(&b, &a), BTreeSet::from([h(1)]));

    // The pair's total delta is the symmetric difference {1, 4} = the union of both pulls.
    let sym = symmetric_difference(&a, &b);
    assert_eq!(sym, BTreeSet::from([h(1), h(4)]));
    let union: BTreeSet<Hash32> = missing_from(&a, &b)
        .union(&missing_from(&b, &a))
        .copied()
        .collect();
    assert_eq!(sym, union);
}

fn offer_of(count: usize) -> Offer {
    let addresses = (0..count)
        .map(|n| {
            let mut bytes = [0xFFu8; 32];
            bytes[..8].copy_from_slice(&(n as u64).to_be_bytes());
            Hash32(bytes)
        })
        .collect();
    Offer::new(addresses, 9)
}

/// Offers are padded to a size bucket, so close sizes are indistinguishable on the wire, and
/// capped per request, so a large offer is split into pages that reassemble exactly.
#[test]
fn offers_are_padded_and_capped() {
    let one = offer_of(1).encode_pages();
    let bucketful = offer_of(MIN_OFFER_BUCKET).encode_pages();
    let empty = offer_of(0).encode_pages();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].len(), bucketful[0].len(), "1 and 64 share a bucket");
    assert_eq!(one[0].len(), empty[0].len(), "an empty offer is padded too");
    assert!(offer_of(MIN_OFFER_BUCKET + 1).encode_pages()[0].len() > one[0].len());
    assert_eq!(offer_bucket(MIN_OFFER_BUCKET + 1), 2 * MIN_OFFER_BUCKET);

    let big = offer_of(OFFER_PAGE_CAP + 10);
    let pages = big.encode_pages();
    assert_eq!(pages.len(), 2, "split at the per-request cap");
    let decoded: Vec<_> = pages
        .iter()
        .map(|page| decode_offer_page(page).unwrap())
        .collect();
    assert!(
        decoded
            .iter()
            .all(|page| page.addresses.len() <= OFFER_PAGE_CAP)
    );
    assert_eq!(Offer::from_pages(&decoded).unwrap(), big);
    assert_eq!(
        Offer::from_pages(&[decode_offer_page(&empty[0]).unwrap()]).unwrap(),
        offer_of(0)
    );
}

/// A page the encoder would not produce is refused: wrong size, over the cap, a count beyond the
/// page, non-zero padding, or pages disagreeing on the cursor.
#[test]
fn malformed_offer_pages_are_refused() {
    let page = offer_of(3).encode_pages().remove(0);
    assert!(decode_offer_page(&page[..5]).is_err(), "short header");
    assert!(
        decode_offer_page(&page[..page.len() - 1]).is_err(),
        "torn slot"
    );
    assert!(
        decode_offer_page(&page[..page.len() - 32]).is_err(),
        "63 slots is not a bucket"
    );

    let mut padded = page.clone();
    *padded.last_mut().unwrap() = 1;
    assert!(decode_offer_page(&padded).is_err(), "non-zero padding");

    let mut overcount = page.clone();
    overcount[8..12].copy_from_slice(&65u32.to_be_bytes());
    assert!(
        decode_offer_page(&overcount).is_err(),
        "count beyond the page"
    );

    let mut oversized = vec![0u8; 12 + 2 * OFFER_PAGE_CAP * 32];
    oversized[8..12].copy_from_slice(&1u32.to_be_bytes());
    assert!(
        decode_offer_page(&oversized).is_err(),
        "bucket above the cap"
    );

    let mut a = decode_offer_page(&page).unwrap();
    let b = a.clone();
    a.cursor += 1;
    assert!(Offer::from_pages(&[a, b]).is_err(), "cursor disagreement");
}

/// The sender's assets are cut to the receiver's complement over head record hashes: an asset
/// the receiver holds at the same head is excluded, one it holds at an older head (a metadata
/// edit over the same ciphertext) still moves, and one it lacks moves.
#[test]
fn a_receivers_offer_cuts_the_assets_to_what_it_is_missing() {
    let fx = Fx::new();
    let (c1, ct1, m1) = fx.records(1, b"asset one", 1);
    let (c2, ct2, m2) = fx.records(2, b"asset two", 2);
    let (c3, ct3, m3) = fx.records(3, b"asset three", 1);
    let assets = vec![
        asset_from(&c1, &ct1, &m1),
        asset_from(&c2, &ct2, &m2),
        asset_from(&c3, &ct3, &m3),
    ];
    assert_eq!(head_address(&assets[1]), Some(head_hash(&c2)));
    assert_eq!(
        address(&c2[..1]),
        address(&c2),
        "the edit kept the ciphertext"
    );

    let receiver = Offer::new(BTreeSet::from([head_hash(&c1), head_hash(&c2[..1])]), 7);
    let (kept, excluded) = cut_to_missing(assets.clone(), &receiver);
    let kept: Vec<Uuid> = kept.iter().map(|asset| asset.asset_id).collect();
    assert_eq!(kept, [Uuid::from_u128(2), Uuid::from_u128(3)]);
    assert_eq!(excluded, 1);

    let everything = Offer::new(assets.iter().filter_map(head_address).collect(), 7);
    let (kept, excluded) = cut_to_missing(assets.clone(), &everything);
    assert!(
        kept.is_empty(),
        "a receiver holding every head is sent nothing"
    );
    assert_eq!(excluded, 3);

    let (kept, excluded) = cut_to_missing(assets, &Offer::new(BTreeSet::new(), 0));
    assert_eq!(
        (kept.len(), excluded),
        (3, 0),
        "an empty offer takes everything"
    );
}

// ── Ingest (smoke) ────────────────────────────────────────────────────────────

/// Build a delta-scoped artifact on device A; feed it to device B; assert restore applies exactly
/// the missing assets — and *only* the missing assets ever enter the artifact.
#[tokio::test]
async fn delta_scoped_transfer_moves_only_missing_assets_and_ingests() {
    let fx = Fx::new();
    let (c1, ct1, m1) = fx.records(1, b"asset one", 1);
    let (c2, ct2, m2) = fx.records(2, b"asset two", 1);
    let (c3, ct3, m3) = fx.records(3, b"asset three", 1);

    let a_assets = vec![
        asset_from(&c1, &ct1, &m1),
        asset_from(&c2, &ct2, &m2),
        asset_from(&c3, &ct3, &m3),
    ];

    // Device B already holds asset 1. The delta B needs from A is {2, 3}.
    let a_offer = Offer::new(
        BTreeSet::from([address(&c1), address(&c2), address(&c3)]),
        3,
    );
    let b_offer = Offer::new(BTreeSet::from([address(&c1)]), 1);
    let delta = missing_from(&b_offer, &a_offer);
    assert_eq!(delta, BTreeSet::from([address(&c2), address(&c3)]));

    let export = DeltaExport {
        assets: &a_assets,
        delta: &delta,
        scope: FetchScope::MetadataThumbnailsOriginal,
        amks: &fx.amks(),
        exporter_device: fx.id_a,
        source_library_version: "1".into(),
        export_timestamp: "2026-05-31T00:00:00Z".into(),
    };
    let artifact = build_delta_artifact(&export, &secret(), &fx.dev_a).unwrap();

    // Only the missing assets are in the artifact — asset 1 (already held) never entered it.
    let opened =
        BackupArtifact::open(&artifact, secret().as_bytes(), &fx.dev_a.verifying_key()).unwrap();
    let carried: BTreeSet<Uuid> = opened
        .provenance_heads()
        .iter()
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(
        carried,
        BTreeSet::from([Uuid::from_u128(2), Uuid::from_u128(3)]),
        "delta scoping: the already-held asset is never transferred"
    );

    // Transfer over ranged GET, then ingest through the restore path on device B.
    let source = ArtifactBlobSource::new(artifact.clone());
    let pulled = pull_artifact(&source, &artifact_address(&artifact), source.len())
        .await
        .unwrap();
    assert_eq!(
        pulled, artifact,
        "ranged GET reassembles the exact artifact"
    );

    // B holds asset 1 already; restore adds 2 and 3.
    let mut b_heads = BTreeMap::new();
    b_heads.insert(Uuid::from_u128(1), head_hash(&c1));
    let none = BTreeSet::new();
    let restored = ingest(
        &pulled,
        &secret(),
        &fx.dev_a.verifying_key(),
        library(&b_heads, &none),
    )
    .unwrap();

    let applied: BTreeSet<Uuid> = restored.applied.iter().map(|a| a.asset_id).collect();
    assert_eq!(
        applied,
        BTreeSet::from([Uuid::from_u128(2), Uuid::from_u128(3)])
    );
    assert!(restored.quarantined.is_empty());
    assert!(restored.incomplete.is_empty());

    // Byte-equal rebuild proxy: the decrypted plaintext on B matches A's originals.
    let mut plaintexts: Vec<Vec<u8>> = restored
        .applied
        .iter()
        .map(|a| a.plaintext.clone())
        .collect();
    plaintexts.sort();
    assert_eq!(
        plaintexts,
        vec![b"asset three".to_vec(), b"asset two".to_vec()]
    );

    // An artifact sealed under one session's secret does not open under another's.
    let wrong = ArtifactSecret::from_bytes([0x01; 32]);
    assert!(matches!(
        ingest(
            &pulled,
            &wrong,
            &fx.dev_a.verifying_key(),
            library(&b_heads, &none)
        ),
        Err(PeeringError::Backup(BackupError::Auth(_)))
    ));
}

/// A receiver whose scope does not eagerly hold originals is not sent an artifact (which always
/// carries them); it syncs from the server instead.
#[test]
fn delta_artifact_honours_the_receiver_scope() {
    let fx = Fx::new();
    let (chain, ct, meta) = fx.records(1, b"scoped", 1);
    let assets = vec![asset_from(&chain, &ct, &meta)];
    for scope in [FetchScope::MetadataOnly, FetchScope::MetadataThumbnails] {
        let export = DeltaExport {
            assets: &assets,
            delta: &BTreeSet::from([address(&chain)]),
            scope,
            amks: &fx.amks(),
            exporter_device: fx.id_a,
            source_library_version: "1".into(),
            export_timestamp: "2026-05-31T00:00:00Z".into(),
        };
        assert!(matches!(
            build_delta_artifact(&export, &secret(), &fx.dev_a),
            Err(PeeringError::OutOfScope(s)) if s == scope
        ));
    }
}

/// An asset this device holds a tombstone for — kept past purge, so it is absent from the heads
/// — is never resurrected by a peer that still holds it; the rest of the artifact applies.
#[test]
fn tombstoned_asset_is_not_resurrected() {
    let fx = Fx::new();
    let (c1, ct1, m1) = fx.records(1, b"deleted long ago", 1);
    let (c2, ct2, m2) = fx.records(2, b"still wanted", 1);
    let artifact = fx.artifact_of(&[asset_from(&c1, &ct1, &m1), asset_from(&c2, &ct2, &m2)]);

    let heads = BTreeMap::new();
    let tombstones = BTreeSet::from([Uuid::from_u128(1)]);
    let got = ingest(
        &artifact,
        &secret(),
        &fx.dev_a.verifying_key(),
        library(&heads, &tombstones),
    )
    .unwrap();
    assert_eq!(got.tombstoned, vec![Uuid::from_u128(1)]);
    let applied: Vec<Uuid> = got.applied.iter().map(|a| a.asset_id).collect();
    assert_eq!(applied, vec![Uuid::from_u128(2)]);
}

/// Per asset, all or nothing: an asset whose metadata is missing or is not the blob its manifest
/// commits to is not applied, and does not stop a complete asset beside it.
#[test]
fn partial_asset_is_not_applied() {
    let fx = Fx::new();
    let (c1, ct1, _m1) = fx.records(1, b"metadata lost", 1);
    let (c2, ct2, m2) = fx.records(2, b"complete", 1);
    let (c3, ct3, _m3) = fx.records(3, b"metadata swapped", 1);
    // Asset 1 arrives with no metadata; asset 3 with asset 2's blob in place of its own.
    let artifact = fx.artifact_of(&[
        asset_from(&c1, &ct1, &[]),
        asset_from(&c2, &ct2, &m2),
        asset_from(&c3, &ct3, &m2),
    ]);

    let heads = BTreeMap::new();
    let none = BTreeSet::new();
    let got = ingest(
        &artifact,
        &secret(),
        &fx.dev_a.verifying_key(),
        library(&heads, &none),
    )
    .unwrap();
    let applied: Vec<Uuid> = got.applied.iter().map(|a| a.asset_id).collect();
    assert_eq!(applied, vec![Uuid::from_u128(2)]);
    assert_eq!(got.incomplete, vec![Uuid::from_u128(1), Uuid::from_u128(3)]);
}

/// A chain that commits to no metadata hash still needs a metadata blob to apply.
#[test]
fn uncommitted_chain_needs_a_metadata_blob() {
    let fx = Fx::new();
    let (c1, ct1, m1) = fx.records_committing(1, b"legacy with meta", 1, false);
    let (c2, ct2, _m2) = fx.records_committing(2, b"legacy without meta", 1, false);
    let artifact = fx.artifact_of(&[asset_from(&c1, &ct1, &m1), asset_from(&c2, &ct2, &[])]);

    let heads = BTreeMap::new();
    let none = BTreeSet::new();
    let got = ingest(
        &artifact,
        &secret(),
        &fx.dev_a.verifying_key(),
        library(&heads, &none),
    )
    .unwrap();
    let applied: Vec<Uuid> = got.applied.iter().map(|a| a.asset_id).collect();
    assert_eq!(applied, vec![Uuid::from_u128(1)]);
    assert_eq!(got.incomplete, vec![Uuid::from_u128(2)]);
}

/// Device A holds an old manifest; device B holds a newer chain head. A pulls from B: the forward
/// update is adopted. B pulls from A: the stale state is **quarantined**, never a silent overwrite.
#[tokio::test]
async fn stale_revival_is_quarantined_not_overwritten() {
    let fx = Fx::new();
    // A two-version chain over one asset: v1 = Create, v2 = Create + MetadataUpdate.
    let (chain2, ct, meta) = fx.records(1, b"the photo", 2);
    let chain1 = chain2[..1].to_vec(); // the older device's view: create only

    let head_old = head_hash(&chain1);
    let head_new = head_hash(&chain2);
    assert_ne!(head_old, head_new);

    let asset_v1 = asset_from(&chain1, &ct, &meta); // what A holds/exports
    let asset_v2 = asset_from(&chain2, &ct, &meta); // what B holds/exports
    let amks = fx.amks();
    let none = BTreeSet::new();

    let export_of = |assets: &[BackupAsset], device: &HybridSigningKey, dev_id: Uuid| {
        let list = assets.to_vec();
        let export = DeltaExport {
            assets: &list,
            delta: &BTreeSet::from([address(&chain2)]),
            scope: FetchScope::MetadataThumbnailsOriginal,
            amks: &amks,
            exporter_device: dev_id,
            source_library_version: "1".into(),
            export_timestamp: "2026-05-31T00:00:00Z".into(),
        };
        build_delta_artifact(&export, &secret(), device).unwrap()
    };

    // A pulls from B (newer): B's chain contains A's head as an ancestor → forward, adopted.
    let from_b = export_of(std::slice::from_ref(&asset_v2), &fx.dev_b, fx.id_b);
    let a_heads = BTreeMap::from([(Uuid::from_u128(1), head_old)]);
    let a_got = ingest(
        &from_b,
        &secret(),
        &fx.dev_b.verifying_key(),
        library(&a_heads, &none),
    )
    .unwrap();
    assert_eq!(a_got.applied.len(), 1, "A adopts the newer forward update");
    assert_eq!(a_got.applied[0].asset_id, Uuid::from_u128(1));
    assert!(a_got.quarantined.is_empty());

    // B pulls from A (older): A's head is NOT an ancestor of B's newer head → stale, quarantined.
    let from_a = export_of(std::slice::from_ref(&asset_v1), &fx.dev_a, fx.id_a);
    let b_heads = BTreeMap::from([(Uuid::from_u128(1), head_new)]);
    let b_got = ingest(
        &from_a,
        &secret(),
        &fx.dev_a.verifying_key(),
        library(&b_heads, &none),
    )
    .unwrap();
    assert!(
        b_got.applied.is_empty(),
        "a peer never silently overwrites newer local state"
    );
    assert_eq!(
        b_got.quarantined,
        vec![Uuid::from_u128(1)],
        "the stale peer state is quarantined and surfaced"
    );
}

// ── Revocation mid-session (smoke) ────────────────────────────────────────────

/// A peer revoked during a session is cut off at its next request, stays cut off, and everything
/// it delivered at or after its revocation took effect is quarantined.
#[tokio::test]
async fn peer_revoked_mid_session_is_cut_off_and_its_later_deliveries_quarantined() {
    let fx = Fx::new();
    let (server_res, _client_res) = run_handshake(&fx, fx.trust(false), fx.trust(false)).await;
    let peer = server_res.unwrap();
    let versions = MemoryDirectoryVersions::default();
    let mut session = PeerSession::new(&peer);
    assert_eq!(session.peer(), fx.id_a);

    // Device A is the server's peer here; revoke A in directory version 2.
    let revoke_a = PinnedTrust {
        user_ik: fx.ik.verifying_key(),
        directory: DirectoryCore {
            user_id: Uuid::from_u128(USER),
            directory_version: 2,
            updated_at: "2026-05-16T00:00:00Z".into(),
            devices: vec![
                fx.entry(fx.id_a, &fx.dev_a, true),
                fx.entry(fx.id_b, &fx.dev_b, false),
            ],
            unknown: Default::default(),
        }
        .sign(&fx.ik),
    };

    let at = |s: &str| s.parse::<Timestamp>().unwrap();
    session.authorize(&fx.trust(false), &versions).unwrap();
    session.record_delivery(Uuid::from_u128(1), at("2026-05-14T00:00:00Z"));
    session.authorize(&fx.trust(false), &versions).unwrap();
    session.record_delivery(Uuid::from_u128(2), at("2026-05-15T12:00:00Z"));
    assert!(session.quarantined_deliveries(&fx.trust(false)).is_empty());

    // The directory refreshes: the next request is refused...
    assert!(matches!(
        session.authorize(&revoke_a, &versions),
        Err(PeeringError::RevokedDevice(id)) if id == fx.id_a
    ));
    // ...and the session stays cut off, even against the older directory.
    assert!(matches!(
        session.authorize(&fx.trust(false), &versions),
        Err(PeeringError::RevokedDevice(_))
    ));
    // Only what arrived after the revocation took effect (2026-05-15) is quarantined.
    assert_eq!(
        session.quarantined_deliveries(&revoke_a),
        vec![Uuid::from_u128(2)]
    );
}

/// A peer that vanished from the directory entirely, or whose revocation time does not parse, has
/// every delivery quarantined.
#[tokio::test]
async fn unknown_or_unparseable_revocation_quarantines_everything() {
    let fx = Fx::new();
    let (server_res, _) = run_handshake(&fx, fx.trust(false), fx.trust(false)).await;
    let mut session = PeerSession::new(&server_res.unwrap());
    session.record_delivery(Uuid::from_u128(1), "2026-01-01T00:00:00Z".parse().unwrap());

    let directory = |entry: Option<DeviceEntry>| PinnedTrust {
        user_ik: fx.ik.verifying_key(),
        directory: DirectoryCore {
            user_id: Uuid::from_u128(USER),
            directory_version: 3,
            updated_at: "2026-05-16T00:00:00Z".into(),
            devices: entry.into_iter().collect(),
            unknown: Default::default(),
        }
        .sign(&fx.ik),
    };
    assert_eq!(
        session.quarantined_deliveries(&directory(None)),
        vec![Uuid::from_u128(1)]
    );
    let mut garbled = fx.entry(fx.id_a, &fx.dev_a, true);
    garbled.revoked_at = Some("yesterday".into());
    assert_eq!(
        session.quarantined_deliveries(&directory(Some(garbled))),
        vec![Uuid::from_u128(1)]
    );
}

// ── Resume (smoke) ────────────────────────────────────────────────────────────

/// Start a large artifact transfer, sever the LAN mid-transfer, reconnect; assert a Range-resumed
/// transfer that re-fetches **zero** bytes already held.
#[tokio::test]
async fn ranged_transfer_resumes_across_a_lan_drop_with_zero_duplicate_bytes() {
    let fx = Fx::new();
    // A sizeable artifact so the drop lands mid-stream.
    let big = vec![0x5Au8; 500_000];
    let (chain, ct, meta) = fx.records(1, &big, 1);
    let artifact = fx.artifact_of(&[asset_from(&chain, &ct, &meta)]);
    let total = artifact.len() as u64;
    assert!(
        total > 100_000,
        "artifact is large enough to drop mid-stream"
    );

    // The source serves the whole artifact but drops the first response after 40 KiB.
    let source = ArtifactBlobSource::with_drop_after(artifact.clone(), 40 * 1024);
    let pulled = pull_artifact(&source, &artifact_address(&artifact), total)
        .await
        .unwrap();
    assert_eq!(
        pulled, artifact,
        "resumed transfer reassembles the exact bytes"
    );

    // Prove zero duplicate bytes: every served range starts exactly where the previous ended, and
    // together they tile [0, total) with no overlap.
    let ranges = source.served_ranges();
    assert!(ranges.len() >= 2, "a drop forced at least one resume");
    let mut cursor = 0u64;
    for (start, len) in ranges {
        assert_eq!(
            start, cursor,
            "each range resumes from the persisted offset (no re-fetch)"
        );
        cursor += len;
    }
    assert_eq!(
        cursor, total,
        "the tiled ranges cover the whole artifact exactly once"
    );
}

/// A verified channel pair over a real in-process TLS handshake: device B's end (the serving
/// peer) and device A's end (the pulling peer).
async fn channel_pair(fx: &Fx) -> (PeerChannel, PeerChannel) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (server_trust, client_trust) = (fx.trust(false), fx.trust(false));
    let (server_versions, client_versions) = (
        MemoryDirectoryVersions::default(),
        MemoryDirectoryVersions::default(),
    );
    let limiter = HandshakeLimiter::default();
    let serving = async {
        let (tcp, _) = listener.accept().await.unwrap();
        let local = LocalPeer {
            device_id: fx.id_b,
            signer: &fx.dev_b,
            trust: &server_trust,
            versions: &server_versions,
        };
        accept(tcp, local, &limiter).await
    };
    let dialling = async {
        let tcp = TcpStream::connect(addr).await.unwrap();
        let local = LocalPeer {
            device_id: fx.id_a,
            signer: &fx.dev_a,
            trust: &client_trust,
            versions: &client_versions,
        };
        connect(tcp, local).await
    };
    let (server, client) = tokio::join!(serving, dialling);
    (server.expect("B verifies A"), client.expect("A verifies B"))
}

/// A pull that breaks off mid-range reports the failure that broke it, not the generic "broke
/// earlier" the fetch's next call on the now-broken channel gets, and keeps what arrived.
#[tokio::test]
async fn a_channel_pull_reports_the_first_failure_not_the_broken_channel() {
    let fx = Fx::new();
    let address = "e".repeat(64);
    let len = 64_u64;
    let (mut origin, delegate) = channel_pair(&fx).await;
    origin
        .send(&Message::Listing {
            artifacts: vec![Offered {
                address: address.clone(),
                len,
            }],
        })
        .await
        .expect("sends");
    let client = PeerClient::open(delegate).await.expect("opens");
    // Eight bytes of the range arrive, then a frame larger than the bytes still owed breaks it.
    let breaking = async {
        assert_eq!(origin.receive_offer().await.expect("the opening"), None);
        assert!(matches!(
            origin.recv().await.expect("a request"),
            Message::Range { start: 0, .. }
        ));
        origin
            .send(&Message::RangeReply { len })
            .await
            .expect("sends");
        origin.send_data(&[7; 8]).await.expect("sends");
        origin.send_data(&[0; 100]).await.expect("sends");
    };
    let mut held = Vec::new();
    let ((), pulled) = tokio::join!(
        breaking,
        pull_over_channel(&client, &address, len, &mut held)
    );
    assert!(client.is_broken());
    assert!(
        matches!(pulled, Err(PeeringError::Codec(_))),
        "the oversized frame is the diagnosis: {pulled:?}"
    );
    assert_eq!(held, [7; 8], "what arrived is kept for a resume");
}

// ── Opening offer (real channel) ──────────────────────────────────────────────

/// On a channel at [`OFFER_EXCHANGE_PROTOCOL`] the pulling side's offer crosses, every page of
/// it, before the serving side lists: what the server reads is exactly what the client holds.
#[tokio::test]
async fn the_receivers_offer_crosses_the_channel_before_the_listing() {
    let fx = Fx::new();
    let (mut origin, receiver) = channel_pair(&fx).await;
    assert_eq!(origin.peer().protocol, OFFER_EXCHANGE_PROTOCOL);
    // Two pages: the offer is split at the per-request cap and reassembled whole.
    let offer = offer_of(OFFER_PAGE_CAP + 1);
    let serving = async {
        let read = origin.receive_offer().await.expect("the offer reads");
        origin
            .send(&Message::Listing {
                artifacts: Vec::new(),
            })
            .await
            .expect("lists");
        read
    };
    let (read, client) = tokio::join!(serving, PeerClient::open_offering(receiver, &offer));
    assert_eq!(read, Some(offer));
    assert!(client.expect("the listing reads").listing().is_empty());
    assert!(
        matches!(origin.receive_offer().await, Err(PeeringError::Codec(_))),
        "the opening is read once"
    );
}

/// A pair that negotiated below [`OFFER_EXCHANGE_PROTOCOL`] exchanges no opening: the server
/// reads no offer, the client sends none, and the next frame is the client's first request.
#[tokio::test]
async fn an_older_peer_exchanges_no_offer() {
    let fx = Fx::new();
    let (origin, receiver) = channel_pair(&fx).await;
    let (mut origin, receiver) = (
        origin.speaking(PEERING_PROTOCOL_MIN),
        receiver.speaking(PEERING_PROTOCOL_MIN),
    );
    assert!(PEERING_PROTOCOL_MIN < OFFER_EXCHANGE_PROTOCOL);
    assert_eq!(origin.receive_offer().await.expect("nothing to read"), None);
    origin
        .send(&Message::Listing {
            artifacts: Vec::new(),
        })
        .await
        .expect("lists");
    let client = PeerClient::open_offering(receiver, &offer_of(3))
        .await
        .expect("the listing reads");
    client.finish().await.expect("done");
    assert_eq!(
        origin.recv().await.expect("a frame"),
        Message::Done,
        "no offer went ahead of the client's first request"
    );
}

/// A server that cut its offering without reading the opening still serves: [`serve`] reads and
/// sets the opening aside, so a receiver that sent its offer to a serving side that does not
/// use it (a delegation) is listed and served as before.
#[tokio::test]
async fn serve_sets_an_unread_opening_aside() {
    let fx = Fx::new();
    let (mut origin, receiver) = channel_pair(&fx).await;
    let trust = fx.trust(false);
    let versions = MemoryDirectoryVersions::default();
    let mut offering = Offering::new();
    let address = offering.offer(b"an artifact".to_vec()).expect("offers");
    let serving = serve(&mut origin, &offering, &trust, &versions, |_| {
        Err(DelegationError::AckMismatch)
    });
    let pulling = async {
        let client = PeerClient::open_offering(receiver, &offer_of(2))
            .await
            .expect("the listing reads");
        assert_eq!(client.listing().len(), 1);
        let mut held = Vec::new();
        let bytes = pull_over_channel(&client, &address, 11, &mut held).await;
        client.finish().await.expect("done");
        bytes
    };
    let (served, pulled) = tokio::join!(serving, pulling);
    served.expect("the session ends cleanly");
    assert_eq!(pulled.expect("pulled"), b"an artifact");
}

/// An opening the encoder would not produce ends the read: more pages than the ceiling, a page
/// that is not a bucket, or a frame that is not an offer.
#[tokio::test]
async fn a_malformed_opening_is_refused() {
    let fx = Fx::new();
    let over = u32::try_from(MAX_OFFER_PAGES + 1).unwrap();
    let (mut origin, mut receiver) = channel_pair(&fx).await;
    receiver
        .send(&Message::Offer { pages: over })
        .await
        .expect("sends");
    assert!(matches!(
        origin.receive_offer().await,
        Err(PeeringError::Codec(_))
    ));

    let (mut origin, mut receiver) = channel_pair(&fx).await;
    receiver
        .send(&Message::Offer { pages: 1 })
        .await
        .expect("sends");
    let page = offer_of(3).encode_pages().remove(0);
    receiver
        .send_data(&page[..page.len() - 32])
        .await
        .expect("sends");
    assert!(matches!(
        origin.receive_offer().await,
        Err(PeeringError::Codec(_))
    ));

    let (mut origin, mut receiver) = channel_pair(&fx).await;
    receiver.send(&Message::Done).await.expect("sends");
    assert!(matches!(
        origin.receive_offer().await,
        Err(PeeringError::Codec(_))
    ));
}

// ── LAN sync then server reconcile (in-process shape) ─────────────────────────

/// The cross-module case in its in-process shape: mocked discovery → real mTLS + hybrid check →
/// delta → ranged-GET transfer under the session's artifact secret → restore ingest → server
/// reconciliation converges by content hash (a device never re-uploads a blob the server already
/// holds).
#[tokio::test]
async fn e2e_lan_sync_then_server_reconciliation() {
    let fx = Fx::new();

    // 1. Discovery (mocked): B advertises, A browses and finds it.
    let disc = MockDiscovery::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer_addr = listener.local_addr().unwrap();
    let advert = OpaqueAdvertisement::for_epoch(
        &[9; 32],
        rotation_epoch(1, 1_760_000_000),
        peer_addr.port(),
    );
    disc.advertise(&advert, peer_addr).unwrap();
    let found = disc.browse().unwrap();
    assert_eq!(found.len(), 1);

    // 2. Real mTLS handshake + hybrid check. B (holding the content) is the server; A (behind)
    //    dials and pulls.
    let dev_b = fx.dev_b.clone();
    let id_b = fx.id_b;
    let server_trust = fx.trust(false);
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let versions = MemoryDirectoryVersions::default();
        let local = LocalPeer {
            device_id: id_b,
            signer: &dev_b,
            trust: &server_trust,
            versions: &versions,
        };
        accept(tcp, local, &HandshakeLimiter::default())
            .await
            .map(PeerChannel::into_peer)
    });
    let tcp = TcpStream::connect(peer_addr).await.unwrap();
    let a_trust = fx.trust(false);
    let a_versions = MemoryDirectoryVersions::default();
    let a_view = connect(
        tcp,
        LocalPeer {
            device_id: fx.id_a,
            signer: &fx.dev_a,
            trust: &a_trust,
            versions: &a_versions,
        },
    )
    .await
    .unwrap()
    .into_peer();
    assert_eq!(a_view.device_id, fx.id_b);
    let b_view = server.await.unwrap().unwrap();

    // 3. Delta: A has nothing, B has one asset. A pulls it.
    let (chain, ct, meta) = fx.records(1, b"shared moment", 1);
    let b_assets = vec![asset_from(&chain, &ct, &meta)];
    let a_offer = Offer::new(BTreeSet::new(), 0);
    let b_offer = Offer::new(BTreeSet::from([address(&chain)]), 1);
    let delta = missing_from(&a_offer, &b_offer);
    assert_eq!(delta, BTreeSet::from([address(&chain)]));

    // 4. B builds the delta-scoped artifact under its session secret; 5. A pulls it over ranged
    //    GET; 6. A ingests via restore under its own copy of the same secret.
    let export = DeltaExport {
        assets: &b_assets,
        delta: &delta,
        scope: FetchScope::MetadataThumbnailsOriginal,
        amks: &fx.amks(),
        exporter_device: fx.id_b,
        source_library_version: "1".into(),
        export_timestamp: "2026-05-31T00:00:00Z".into(),
    };
    let artifact = build_delta_artifact(&export, &b_view.artifact_secret, &fx.dev_b).unwrap();
    let source = ArtifactBlobSource::new(artifact.clone());
    let pulled = pull_artifact(&source, &artifact_address(&artifact), source.len())
        .await
        .unwrap();
    let heads = BTreeMap::new();
    let none = BTreeSet::new();
    let restored = ingest(
        &pulled,
        &a_view.artifact_secret,
        &fx.dev_b.verifying_key(),
        library(&heads, &none),
    )
    .unwrap();
    assert_eq!(restored.applied.len(), 1);
    assert_eq!(restored.applied[0].plaintext, b"shared moment");

    // 7. Server reconciliation: A now holds the asset with its signed manifest intact. The upload
    //    policy resolves by content hash — the server already holds this blob (B uploaded it per
    //    policy), so A does not re-upload it, and A/B/server remain convergent.
    let a_content_address = restored.applied[0]
        .provenance
        .last()
        .unwrap()
        .manifest
        .core
        .ciphertext_hash;
    assert_eq!(
        a_content_address,
        address(&chain),
        "A holds the same content address"
    );
    let server_offer = Offer::new(BTreeSet::from([address(&chain)]), 1);
    let a_offer_after = Offer::new(BTreeSet::from([a_content_address]), 1);
    let to_upload = missing_from(&server_offer, &a_offer_after);
    assert!(
        to_upload.is_empty(),
        "content-addressed dedup: A never re-uploads a blob the server already holds"
    );
}

/// Peering dials and admits only the local network.
#[test]
fn only_local_network_addresses_are_lan() {
    let lan = [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.9",
        "192.168.1.20",
        "169.254.7.7",
        "::1",
        "fd12:3456::1",
        "fe80::1",
        "::ffff:192.168.1.20",
    ];
    let wan = [
        "8.8.8.8",
        "192.0.2.1",
        "172.32.0.1",
        "2001:db8::1",
        "::ffff:8.8.8.8",
        "0.0.0.0",
    ];
    for address in lan {
        assert!(is_lan(address.parse().unwrap()), "{address}");
    }
    for address in wan {
        assert!(!is_lan(address.parse().unwrap()), "{address}");
    }
}
