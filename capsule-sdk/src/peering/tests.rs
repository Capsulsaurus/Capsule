//! Deterministic tests for LAN peering (slice `S-E3`). Each of the peering doc's six Validation
//! bullets gets a test here, plus the cross-module E2E case 5 in its in-process shape.
//!
//! - Discovery is **mocked** ([`MockDiscovery`]) — it is inherently non-deterministic on a real
//!   LAN, so the seam is what we test: the opaque, rotating advertisement.
//! - The mTLS handshake is **real, in-process** — a rustls client and server over a localhost
//!   socket with generated certs — so wrong-cert rejection and the application-layer hybrid check
//!   are exercised against an actual TLS 1.3 channel.
//! - Delta scoping, ranged-GET resume, restore ingest, and stale-revival quarantine run over the
//!   real backup artifact + restore path.

use std::collections::{BTreeMap, BTreeSet};

use capsule_core::backup::{BackupArtifact, BackupAsset};
use capsule_core::crypto::encryption::{encrypt_asset_rekey, seal_metadata_blob};
use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::{
    Amk, AmkVersion, DeviceDirectory, DeviceEntry, DirectoryCore, HybridSigningKey,
};
use capsule_core::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use capsule_core::crypto::provenance::manifest::{ASSET_MANIFEST_VERSION, KeyMode, ManifestCore};
use capsule_core::crypto::provenance::{Action, ProvenanceRecord};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

use super::PeeringError;
use super::channel::{
    PEERING_PROTOCOL, PeerHello, PinnedTrust, VerifiedPeer, accept, connect, verify_hello,
};
use super::delta::{Offer, missing_from, symmetric_difference};
use super::discovery::{
    Discovery, MockDiscovery, OpaqueAdvertisement, SERVICE_TYPE, rotation_epoch,
};
use super::transfer::{
    ArtifactBlobSource, DeltaExport, artifact_address, build_delta_artifact, ingest, pull_artifact,
};

const ALBUM: u128 = 0xA1;
const USER: u128 = 0x05E2;
const PASSPHRASE: &[u8] = b"shared-account-wrap-secret-derived-from-master-key";

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
        }
        .sign(signer)
    }

    fn directory(&self, revoke_b: bool) -> DeviceDirectory {
        self.directory_signed_by(1, revoke_b, &self.ik)
    }

    fn trust(&self, revoke_b: bool) -> PinnedTrust {
        PinnedTrust {
            user_ik: self.ik.verifying_key(),
            directory: self.directory(revoke_b),
        }
    }

    /// Build a `versions`-long provenance chain (Create, then MetadataUpdate for each later
    /// version) over a single ciphertext blob, returning the chain, the ciphertext, and the
    /// metadata blob. Later versions share the exact earlier records, so an older device's head is
    /// literally an ancestor record in a newer device's chain.
    fn records(
        &self,
        asset: u128,
        plaintext: &[u8],
        versions: usize,
    ) -> (Vec<ProvenanceRecord>, Vec<u8>, Vec<u8>) {
        let amk = Amk::from_bytes(self.amk);
        let file_id = Uuid::from_u128(asset);
        let (enc, ct, _fk) = encrypt_asset_rekey(&amk, &file_id, plaintext, None).unwrap();
        let meta = seal_metadata_blob(&amk, &file_id, b"{sidecar}", None)
            .unwrap()
            .0;

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
                metadata_blob_hash: None,
                created_by_user: Uuid::from_u128(USER),
                created_by_device: self.id_a,
                client_version: "t".into(),
                timestamp: "2026-05-31T00:00:00Z".into(),
                action,
                prior_provenance_hash: prior,
                retention_until: None,
            };
            let manifest = core.sign(&self.dev_a, &self.write).unwrap();
            let record = ProvenanceRecord {
                asset_id: file_id,
                manifest,
                prior_provenance_hash: prior,
            };
            prior = Some(record.record_hash());
            chain.push(record);
        }
        (chain, ct, meta)
    }
}

/// Assemble a `BackupAsset` from a chain slice + its blob and metadata.
fn asset_from(chain: &[ProvenanceRecord], ct: &[u8], meta: &[u8]) -> BackupAsset {
    let head = chain.last().unwrap();
    BackupAsset {
        album_id: head.manifest.core.album_id,
        asset_id: head.asset_id,
        ciphertext: ct.to_vec(),
        metadata_blob: meta.to_vec(),
        provenance: chain.to_vec(),
        receipts: Vec::new(),
    }
}

fn address(chain: &[ProvenanceRecord]) -> Hash32 {
    chain.last().unwrap().manifest.core.ciphertext_hash
}

fn head_hash(chain: &[ProvenanceRecord]) -> Hash32 {
    chain.last().unwrap().record_hash()
}

// ── Bullet 1: mDNS opaque identifier (unit) ───────────────────────────────────

/// Generate an advertisement; assert it carries no user handle, no device name. Re-generate after
/// the rotation interval; assert a new opaque identifier.
#[test]
fn mdns_advert_is_opaque_and_rotates() {
    let seed = [0x77u8; 32];
    let user_handle = "alice@example.com";
    let device_name = "Alice's iPhone";

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

    // Same boot, same day → epoch is stable (rotates at most every 24 h).
    assert_eq!(
        rotation_epoch(0xBEEF, 1_760_000_000),
        rotation_epoch(0xBEEF, 1_760_000_000 + 3_600)
    );
}

/// The mocked discovery seam round-trips an opaque advertisement to a browsing peer.
#[test]
fn mock_discovery_advertises_and_browses() {
    let disc = MockDiscovery::new();
    let advert = OpaqueAdvertisement::for_epoch(&[1; 32], 7, 6000);
    let addr = "127.0.0.1:6000".parse().unwrap();
    disc.advertise(&advert, addr).unwrap();

    let found = disc.browse().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].instance_name, advert.advertised_name());
    assert_eq!(found[0].addr, addr);
}

// ── Bullet 2: TLS mutual-auth handshake (unit + real in-process) ───────────────

/// Drive a **real** in-process mutual TLS 1.3 handshake between device A (client) and device B
/// (server), each pinning its own trust, and return both sides' verification outcomes.
async fn run_handshake(
    fx: &Fx,
    server_trust: PinnedTrust,
    client_trust: PinnedTrust,
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
        accept(tcp, id_b, &dev_b, &server_trust).await
    });

    let tcp = TcpStream::connect(addr).await.unwrap();
    let client_res = connect(tcp, fx.id_a, &fx.dev_a, &client_trust).await;
    let server_res = server.await.unwrap();
    (server_res, client_res)
}

/// Two device certificates chaining to the same IK — the handshake succeeds and each side learns
/// the other's verified `device_id` over a real TLS 1.3 channel.
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

/// The application-layer hybrid check in isolation (deterministic, no socket): the full rejection
/// matrix, including the protocol-version gate and an unknown / bad-proof peer.
#[test]
fn hybrid_check_rejection_matrix() {
    let fx = Fx::new();
    let exporter = [0x42u8; 32];
    let trust = fx.trust(false);

    // Valid hello from device B.
    let good = build_hello(&fx, fx.id_b, &fx.dev_b, &exporter);
    assert_eq!(verify_hello(&good, &exporter, &trust).unwrap(), fx.id_b);

    // Protocol mismatch → aborts before any identity check.
    let mut wrong_proto = good.clone();
    wrong_proto.peering_protocol = "1999-01-01".into();
    assert!(matches!(
        verify_hello(&wrong_proto, &exporter, &trust),
        Err(PeeringError::ProtocolMismatch { .. })
    ));

    // A device id absent from the pinned directory → unknown.
    let stranger = build_hello(&fx, Uuid::from_u128(0xDEAD), &fx.dev_b, &exporter);
    assert!(matches!(
        verify_hello(&stranger, &exporter, &trust),
        Err(PeeringError::UnknownDevice(_))
    ));

    // The claimed device is real, but the proof was signed by a different key → hybrid fail.
    let imposter_key = HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]);
    let imposter = build_hello(&fx, fx.id_b, &imposter_key, &exporter);
    assert!(matches!(
        verify_hello(&imposter, &exporter, &trust),
        Err(PeeringError::HybridCheckFailed)
    ));

    // A proof over *different* exporter material (a MITM's session) → hybrid fail.
    let other_session = build_hello(&fx, fx.id_b, &fx.dev_b, &[0x11u8; 32]);
    assert!(matches!(
        verify_hello(&other_session, &exporter, &trust),
        Err(PeeringError::HybridCheckFailed)
    ));

    // Revoked device → rejected even with a valid proof.
    assert!(matches!(
        verify_hello(&good, &exporter, &fx.trust(true)),
        Err(PeeringError::RevokedDevice(_))
    ));
}

/// The handshake hello (with its embedded hybrid signature) round-trips through the JSON framing.
#[test]
fn peer_hello_round_trips_through_json_framing() {
    let fx = Fx::new();
    let hello = build_hello(&fx, fx.id_b, &fx.dev_b, &[0x42u8; 32]);
    let bytes = serde_json::to_vec(&hello).unwrap();
    let back: PeerHello = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back.device_id, hello.device_id);
    assert_eq!(back.peering_protocol, PEERING_PROTOCOL);
    // The recovered proof still verifies under device B's key over the exporter material.
    let trust = fx.trust(false);
    assert_eq!(verify_hello(&back, &[0x42u8; 32], &trust).unwrap(), fx.id_b);
}

/// Test helper mirroring the private `build_hello`: sign the exporter material with a device key.
fn build_hello(
    _fx: &Fx,
    device_id: Uuid,
    signer: &HybridSigningKey,
    exporter: &[u8; 32],
) -> PeerHello {
    PeerHello {
        device_id,
        peering_protocol: PEERING_PROTOCOL.to_string(),
        proof: signer.sign(exporter),
    }
}

// ── Bullet 3: delta calculation (unit) ────────────────────────────────────────

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

// ── Bullet 4: delta-scoped artifact + restore ingest (smoke) ──────────────────

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
        amks: &fx.amks(),
        exporter_device: fx.id_a,
        source_library_version: "1".into(),
        export_timestamp: "2026-05-31T00:00:00Z".into(),
    };
    let artifact = build_delta_artifact(&export, PASSPHRASE, &fx.dev_a).unwrap();

    // Only the missing assets are in the artifact — asset 1 (already held) never entered it.
    let opened = BackupArtifact::open(&artifact, PASSPHRASE, &fx.dev_a.verifying_key()).unwrap();
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
    let restored = ingest(&pulled, PASSPHRASE, &fx.dev_a.verifying_key(), &b_heads).unwrap();

    let applied: BTreeSet<Uuid> = restored.applied.iter().map(|a| a.asset_id).collect();
    assert_eq!(
        applied,
        BTreeSet::from([Uuid::from_u128(2), Uuid::from_u128(3)])
    );
    assert!(restored.quarantined.is_empty());

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
}

// ── Bullet 5: stale-revival quarantine on peer pull (smoke) ───────────────────

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

    let export_of = |assets: &[BackupAsset], device: &HybridSigningKey, dev_id: Uuid| {
        let list = assets.to_vec();
        let export = DeltaExport {
            assets: &list,
            delta: &BTreeSet::from([address(&chain2)]),
            amks: &amks,
            exporter_device: dev_id,
            source_library_version: "1".into(),
            export_timestamp: "2026-05-31T00:00:00Z".into(),
        };
        build_delta_artifact(&export, PASSPHRASE, device).unwrap()
    };

    // A pulls from B (newer): B's chain contains A's head as an ancestor → forward, adopted.
    let from_b = export_of(std::slice::from_ref(&asset_v2), &fx.dev_b, fx.id_b);
    let a_heads = BTreeMap::from([(Uuid::from_u128(1), head_old)]);
    let a_got = ingest(&from_b, PASSPHRASE, &fx.dev_b.verifying_key(), &a_heads).unwrap();
    assert_eq!(a_got.applied.len(), 1, "A adopts the newer forward update");
    assert_eq!(a_got.applied[0].asset_id, Uuid::from_u128(1));
    assert!(a_got.quarantined.is_empty());

    // B pulls from A (older): A's head is NOT an ancestor of B's newer head → stale, quarantined.
    let from_a = export_of(std::slice::from_ref(&asset_v1), &fx.dev_a, fx.id_a);
    let b_heads = BTreeMap::from([(Uuid::from_u128(1), head_new)]);
    let b_got = ingest(&from_a, PASSPHRASE, &fx.dev_a.verifying_key(), &b_heads).unwrap();
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

// ── Bullet 6: resume across LAN drop (smoke) ──────────────────────────────────

/// Start a large artifact transfer, sever the LAN mid-transfer, reconnect; assert a Range-resumed
/// transfer that re-fetches **zero** bytes already held.
#[tokio::test]
async fn ranged_transfer_resumes_across_a_lan_drop_with_zero_duplicate_bytes() {
    let fx = Fx::new();
    // A sizeable artifact so the drop lands mid-stream.
    let big = vec![0x5Au8; 500_000];
    let (chain, ct, meta) = fx.records(1, &big, 1);
    let asset = asset_from(&chain, &ct, &meta);
    let list = vec![asset];
    let export = DeltaExport {
        assets: &list,
        delta: &BTreeSet::from([address(&chain)]),
        amks: &fx.amks(),
        exporter_device: fx.id_a,
        source_library_version: "1".into(),
        export_timestamp: "2026-05-31T00:00:00Z".into(),
    };
    let artifact = build_delta_artifact(&export, PASSPHRASE, &fx.dev_a).unwrap();
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

// ── E2E case 5 (in-process shape): full A→B LAN sync + server reconciliation ───

/// The cross-module case in its in-process shape: mocked discovery → real mTLS + hybrid check →
/// delta → ranged-GET transfer → restore ingest → server reconciliation converges by content hash
/// (a device never re-uploads a blob the server already holds).
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
        accept(tcp, id_b, &dev_b, &server_trust).await
    });
    let tcp = TcpStream::connect(peer_addr).await.unwrap();
    let a_view = connect(tcp, fx.id_a, &fx.dev_a, &fx.trust(false))
        .await
        .unwrap();
    assert_eq!(a_view.device_id, fx.id_b);
    server.await.unwrap().unwrap();

    // 3. Delta: A has nothing, B has one asset. A pulls it.
    let (chain, ct, meta) = fx.records(1, b"shared moment", 1);
    let b_assets = vec![asset_from(&chain, &ct, &meta)];
    let a_offer = Offer::new(BTreeSet::new(), 0);
    let b_offer = Offer::new(BTreeSet::from([address(&chain)]), 1);
    let delta = missing_from(&a_offer, &b_offer);
    assert_eq!(delta, BTreeSet::from([address(&chain)]));

    // 4. B builds the delta-scoped artifact; 5. A pulls it over ranged GET; 6. A ingests via restore.
    let export = DeltaExport {
        assets: &b_assets,
        delta: &delta,
        amks: &fx.amks(),
        exporter_device: fx.id_b,
        source_library_version: "1".into(),
        export_timestamp: "2026-05-31T00:00:00Z".into(),
    };
    let artifact = build_delta_artifact(&export, PASSPHRASE, &fx.dev_b).unwrap();
    let source = ArtifactBlobSource::new(artifact.clone());
    let pulled = pull_artifact(&source, &artifact_address(&artifact), source.len())
        .await
        .unwrap();
    let restored = ingest(
        &pulled,
        PASSPHRASE,
        &fx.dev_b.verifying_key(),
        &BTreeMap::new(),
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
