//! Upload delegation, over a real library's signed bundle: the hand-off, the delegate's
//! verification, the origin's ledger, and every row of the peering page's edge-case table that
//! is decided on one device (the server half is `capsule-server`'s, the whole flow E2E case 15).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::keys::{
    DeviceDirectory, DeviceEntry, DirectoryCore, HybridSigningKey, HybridVerifyingKey,
};
use capsule_core::crypto::primitives::Argon2Params;
use capsule_library::library::{
    BlobVerdict, CustodyReceipt, CustodyReceiptCore, StorageVerdict, VerifierError,
};
use capsule_library::lifecycle::Workspace;
use tokio::net::{TcpListener, TcpStream};

use super::*;
use crate::net::ConnectionClass;
use crate::peering::channel::{
    LocalPeer, MAX_ARTIFACT_LEN, MAX_LISTING, Message, Offered, RangeFailure, ServedRange, accept,
    connect,
};
use crate::peering::session::{HandshakeLimiter, MemoryDirectoryVersions, SharedTrust};
use crate::testmock::{MockRequest, MockResponse, MockServer};
use crate::verify::{StaticToken as VerifyToken, StorageVerifyClient, VerifyTransport};

const FAST: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// The clock the deadlines below run on, and a reboot's.
const BOOT: &str = "boot-1";
const REBOOT: &str = "boot-2";

/// A real library with one imported asset, its bundle, and a directory naming the library's
/// device and a delegate whose key the test holds.
struct Fixture {
    _dir: tempfile::TempDir,
    workspace: Workspace,
    bundle: UploadBundle,
    ik: HybridSigningKey,
    delegate: Uuid,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().expect("a temp dir");
        let lib = dir.path().join("library");
        std::fs::create_dir_all(&lib).expect("the library dir");
        let src = dir.path().join("photo.jpg");
        std::fs::write(&src, b"\xFF\xD8\xFF the original a phone hands to a laptop")
            .expect("the source");
        let mut workspace =
            Workspace::create_with_params(&lib, b"passphrase", FAST).expect("a library");
        let album = workspace.default_album_id();
        workspace
            .ensure_album(album, "Imports")
            .expect("the album keys");
        let asset = workspace.import_asset(album, &src).expect("the import");
        let bundle = workspace.upload_bundle(&asset).expect("the bundle");
        Self {
            _dir: dir,
            workspace,
            bundle,
            ik: HybridSigningKey::from_seed64(&[0x1D; 64]),
            delegate: Uuid::from_u128(0xDE1E_6A7E),
        }
    }

    fn origin(&self) -> Uuid {
        self.workspace.device_id()
    }

    fn origin_entry(&self) -> DeviceEntry {
        self.workspace
            .device_directory()
            .device(&self.origin())
            .cloned()
            .expect("the library lists its own device")
    }

    fn delegate_entry(&self, revoked: bool) -> DeviceEntry {
        DeviceEntry {
            device_id: self.delegate,
            dsk_public: HybridSigningKey::from_seed64(&[0xDE; 64]).verifying_key(),
            dek_public: None,
            added_at: self.origin_entry().added_at,
            revoked_at: revoked.then(|| "2026-10-01T00:00:00Z".to_owned()),
            unknown: Default::default(),
        }
    }

    /// The account's directory at `version`, listing `entries`, signed by the account IK.
    fn directory(&self, version: u64, entries: Vec<DeviceEntry>) -> DeviceDirectory {
        DirectoryCore {
            user_id: self.workspace.user_id(),
            directory_version: version,
            updated_at: "2026-10-01T00:00:00Z".to_owned(),
            devices: entries,
            unknown: Default::default(),
        }
        .sign(&self.ik)
    }

    fn trust_of(&self, directory: DeviceDirectory) -> PinnedTrust {
        PinnedTrust {
            user_ik: self.ik.verifying_key(),
            directory,
        }
    }

    /// Both devices, neither revoked.
    fn trust(&self) -> PinnedTrust {
        self.trust_of(self.directory(5, vec![self.origin_entry(), self.delegate_entry(false)]))
    }

    fn sealed(&self) -> SealedHandOff {
        seal(&self.bundle, self.delegate, &self.trust()).expect("the hand-off seals")
    }

    fn verify(&self, bytes: &[u8]) -> Result<VerifiedHandOff, DelegationError> {
        verify_hand_off(
            bytes,
            &self.trust(),
            &MemoryDirectoryVersions::default(),
            self.delegate,
            &self.bundle.protocol_version,
        )
    }
}

/// Re-encode a decoded hand-off after `edit`.
fn edited(bytes: &[u8], edit: impl FnOnce(&mut HandOff)) -> Vec<u8> {
    let mut hand_off = HandOff::decode(bytes).expect("the hand-off decodes");
    edit(&mut hand_off);
    hand_off.encode().expect("the hand-off encodes")
}

/// Point member `index`'s declared address at its (edited) bytes, as a delegate covering its
/// tracks would.
fn redeclare(hand_off: &mut HandOff, index: usize) {
    let hash = hash_bytes(&hand_off.blobs[index]).to_hex();
    let member = &mut hand_off.header.members[index];
    member.size = hand_off.blobs[index].len() as u64;
    member.hash.clone_from(&hash);
    member.manifest_envelope.ciphertext_hash = hash;
}

fn index_of(hand_off: &HandOff, role: BlobRole) -> usize {
    hand_off
        .header
        .members
        .iter()
        .position(|member| member.blob_role == role)
        .expect("the bundle carries the role")
}

fn tampered(result: Result<VerifiedHandOff, DelegationError>) -> bool {
    matches!(result, Err(DelegationError::Tampered(_)))
}

// ─── The hand-off ─────────────────────────────────────────────────────────────

#[test]
fn a_sealed_hand_off_verifies_and_carries_the_bundle_exactly_as_the_origin_would_upload_it() {
    let fixture = Fixture::new();
    let sealed = fixture.sealed();
    let verified = fixture
        .verify(&sealed.bytes)
        .expect("the delegate verifies it");

    assert_eq!(verified.address, sealed.address);
    assert_eq!(verified.asset_id(), fixture.bundle.asset_id);
    // Member for member, the request the origin's own push would send, and the bytes it would
    // send under it: the delegate re-signs, re-encrypts and re-derives nothing.
    let expected: Vec<(String, Vec<u8>)> = bundle_blobs(&fixture.bundle)
        .iter()
        .map(|(blob, hash)| {
            (
                serde_json::to_string(&create_request(&fixture.bundle, blob, hash))
                    .expect("a request serializes"),
                blob.bytes.to_vec().expect("the bytes read"),
            )
        })
        .collect();
    let carried: Vec<(String, Vec<u8>)> = verified
        .members()
        .map(|(request, bytes)| {
            (
                serde_json::to_string(request).expect("a request serializes"),
                bytes.to_vec(),
            )
        })
        .collect();
    assert_eq!(carried, expected);
    // The origin's own record is of the bytes it computed itself.
    assert_eq!(
        sealed
            .own
            .iter()
            .map(|b| b.hash.to_hex())
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|(_, bytes)| hash_bytes(bytes).to_hex())
            .collect::<Vec<_>>()
    );
    assert_eq!(sealed.release[0], fixture.bundle.ciphertext_hash);
}

#[test]
fn the_origin_seals_only_for_another_unrevoked_device_of_its_directory() {
    let fixture = Fixture::new();
    assert!(matches!(
        seal(&fixture.bundle, fixture.origin(), &fixture.trust()),
        Err(DelegationError::SelfDelegation)
    ));
    assert!(matches!(
        seal(&fixture.bundle, Uuid::from_u128(7), &fixture.trust()),
        Err(DelegationError::Peering(PeeringError::UnknownDevice(_)))
    ));
    let revoked = fixture.trust_of(fixture.directory(
        5,
        vec![fixture.origin_entry(), fixture.delegate_entry(true)],
    ));
    assert!(matches!(
        seal(&fixture.bundle, fixture.delegate, &revoked),
        Err(DelegationError::Peering(PeeringError::RevokedDevice(_)))
    ));
}

#[test]
fn the_container_round_trips_and_refuses_anything_malformed() {
    let fixture = Fixture::new();
    let sealed = fixture.sealed();
    let decoded = HandOff::decode(&sealed.bytes).expect("the hand-off decodes");
    assert_eq!(decoded.encode().expect("re-encodes"), sealed.bytes);

    // A partial hand-off — cut anywhere — is never a bundle to upload.
    for cut in [0, 4, 12, sealed.bytes.len() / 2, sealed.bytes.len() - 1] {
        assert!(
            matches!(
                HandOff::decode(&sealed.bytes[..cut]),
                Err(DelegationError::Malformed(_))
            ),
            "cut at {cut}"
        );
    }
    let mut trailing = sealed.bytes.clone();
    trailing.push(0);
    assert!(matches!(
        HandOff::decode(&trailing),
        Err(DelegationError::Malformed(_))
    ));
    let newer = edited(&sealed.bytes, |h| h.header.format = HAND_OFF_FORMAT + 1);
    assert!(matches!(
        HandOff::decode(&newer),
        Err(DelegationError::UnsupportedFormat { .. })
    ));
}

// ─── The delegate's verification (edge cases) ─────────────────────────────────

#[test]
fn a_delegate_that_alters_a_byte_of_any_member_is_refused() {
    // Tampering: the delegate cannot change a byte without the hash, the manifest signature, or
    // both failing — here, at the delegate, before the server ever sees it.
    let fixture = Fixture::new();
    let sealed = fixture.sealed();
    let members = HandOff::decode(&sealed.bytes)
        .expect("decodes")
        .header
        .members
        .len();
    for index in 0..members {
        // The bytes alone: they no longer hash to the declared address.
        let flipped = edited(&sealed.bytes, |h| h.blobs[index][0] ^= 0x01);
        assert!(tampered(fixture.verify(&flipped)), "member {index}");
        // The bytes and their declared address together: the signed manifest commits to the
        // original and metadata, the provenance member is the signature, and derivatives are
        // committed by their signed derivative manifests.
        let covered = edited(&sealed.bytes, |h| {
            let last = h.blobs[index].len() - 1;
            h.blobs[index][last] ^= 0x01;
            redeclare(h, index);
        });
        assert!(
            tampered(fixture.verify(&covered)),
            "member {index}, redeclared"
        );
    }
}

#[test]
fn a_hand_off_missing_a_member_is_refused_whole() {
    let fixture = Fixture::new();
    let sealed = fixture.sealed();
    for role in [BlobRole::Original, BlobRole::Provenance, BlobRole::Metadata] {
        let partial = edited(&sealed.bytes, |h| {
            let index = index_of(h, role);
            h.header.members.remove(index);
            h.blobs.remove(index);
        });
        assert!(tampered(fixture.verify(&partial)), "{role:?}");
    }
}

/// Append a member under `role` carrying `bytes`, with a request shaped like the original's.
fn append_member(hand_off: &mut HandOff, role: BlobRole, content_type: &str, bytes: Vec<u8>) {
    let template = hand_off.header.members[index_of(hand_off, BlobRole::Original)].clone();
    let mut request = template;
    request.blob_role = role;
    request.content_type = content_type.to_owned();
    hand_off.header.members.push(request);
    hand_off.blobs.push(bytes);
    let last = hand_off.blobs.len() - 1;
    redeclare(hand_off, last);
}

#[test]
fn a_derivative_no_origin_signed_derivative_manifest_commits_to_is_refused() {
    use capsule_core::crypto::provenance::{DerivativeCore, DerivativeRole};

    let fixture = Fixture::new();
    let sealed = fixture.sealed();
    // A thumbnail slipped in with no derivative manifest naming it.
    let bare = edited(&sealed.bytes, |h| {
        append_member(
            h,
            BlobRole::Derivative,
            "image/jxl",
            b"a thumbnail".to_vec(),
        );
    });
    assert!(tampered(fixture.verify(&bare)));

    // And one whose derivative manifest the delegate signed itself.
    let thumbnail = b"a thumbnail".to_vec();
    let foreign = HybridSigningKey::from_seed64(&[0xDE; 64]);
    let derivative = DerivativeCore {
        version: "derivative-manifest/v1".into(),
        crypto_suite_id: fixture.bundle.crypto_suite_id,
        protocol_version: Some(fixture.bundle.protocol_version.clone()),
        amk_version: None,
        source_asset_id: fixture.bundle.asset_id,
        role: DerivativeRole::Thumbnail,
        format: "image/jxl".into(),
        ciphertext_hash: hash_bytes(&thumbnail),
        nonce_prefix: [0; 7],
        generated_by_device: fixture.origin(),
        generated_by_client: "capsule-cli/0.1.0".into(),
        model_id: None,
        model_version: None,
        generated_at: fixture.bundle.timestamp.clone(),
        prior_provenance_hash: None,
    }
    .sign(&foreign, &foreign)
    .expect("signs");
    let forged = edited(&sealed.bytes, |h| {
        append_member(
            h,
            BlobRole::DerivativeManifest,
            "application/octet-stream",
            cbor::to_canonical_vec(&derivative).expect("encodes"),
        );
        append_member(h, BlobRole::Derivative, "image/jxl", thumbnail.clone());
    });
    assert!(tampered(fixture.verify(&forged)));
}

#[test]
fn a_manifest_the_origin_did_not_sign_is_refused() {
    let fixture = Fixture::new();
    let sealed = fixture.sealed();
    // The delegate re-signs the manifest with its own key: a courier never signs.
    let resigned = edited(&sealed.bytes, |h| {
        let index = index_of(h, BlobRole::Provenance);
        let mut record = ProvenanceRecord::decode(&h.blobs[index]).expect("a record");
        let delegate_key = HybridSigningKey::from_seed64(&[0xDE; 64]);
        record.manifest = record
            .manifest
            .core
            .clone()
            .sign(&delegate_key, &delegate_key)
            .expect("signs");
        h.blobs[index] = cbor::to_canonical_vec(&record).expect("encodes");
        redeclare(h, index);
    });
    assert!(tampered(fixture.verify(&resigned)));
}

#[test]
fn an_older_delegate_refuses_a_bundle_newer_than_it_speaks() {
    let fixture = Fixture::new();
    let sealed = fixture.sealed();
    let refused = verify_hand_off(
        &sealed.bytes,
        &fixture.trust(),
        &MemoryDirectoryVersions::default(),
        fixture.delegate,
        "2000-01-01",
    );
    assert!(matches!(
        refused,
        Err(DelegationError::ProtocolUnsupported { .. })
    ));
}

#[test]
fn a_hand_off_for_another_delegate_is_refused() {
    let fixture = Fixture::new();
    let sealed = fixture.sealed();
    let refused = verify_hand_off(
        &sealed.bytes,
        &fixture.trust(),
        &MemoryDirectoryVersions::default(),
        Uuid::from_u128(99),
        &fixture.bundle.protocol_version,
    );
    assert!(matches!(
        refused,
        Err(DelegationError::WrongDelegate { addressed }) if addressed == fixture.delegate
    ));
}

#[test]
fn an_origin_the_pinned_directory_does_not_admit_is_refused() {
    let fixture = Fixture::new();
    let sealed = fixture.sealed();
    let verify_under = |trust: PinnedTrust, versions: &MemoryDirectoryVersions| {
        verify_hand_off(
            &sealed.bytes,
            &trust,
            versions,
            fixture.delegate,
            &fixture.bundle.protocol_version,
        )
    };
    // Another account's directory: the origin is not one of its devices.
    let foreign = fixture.trust_of(fixture.directory(5, vec![fixture.delegate_entry(false)]));
    assert!(matches!(
        verify_under(foreign, &MemoryDirectoryVersions::default()),
        Err(DelegationError::Peering(PeeringError::UnknownDevice(_)))
    ));
    // A revoked origin.
    let mut revoked_origin = fixture.origin_entry();
    revoked_origin.revoked_at = Some("2026-10-01T00:00:00Z".to_owned());
    let revoked =
        fixture.trust_of(fixture.directory(5, vec![revoked_origin, fixture.delegate_entry(false)]));
    assert!(matches!(
        verify_under(revoked, &MemoryDirectoryVersions::default()),
        Err(DelegationError::Peering(PeeringError::RevokedDevice(_)))
    ));
    // A directory older than one this device verified: it could readmit a revoked origin.
    assert!(matches!(
        verify_under(fixture.trust(), &MemoryDirectoryVersions::starting_at(6)),
        Err(DelegationError::Peering(
            PeeringError::DirectoryRollback { .. }
        ))
    ));
    // A directory the account's IK did not sign.
    let forged = PinnedTrust {
        user_ik: HybridSigningKey::from_seed64(&[0x0F; 64]).verifying_key(),
        directory: fixture.trust().directory,
    };
    assert!(matches!(
        verify_under(forged, &MemoryDirectoryVersions::default()),
        Err(DelegationError::Peering(PeeringError::ForeignIdentity))
    ));
}

// ─── Persist, then acknowledge ────────────────────────────────────────────────

/// An inbox whose disk is full.
struct FullDisk;

impl DelegateInbox for FullDisk {
    fn persist(&self, _: &str, _: &[u8]) -> io::Result<()> {
        Err(io::Error::other("no space left on device"))
    }
    fn load(&self, _: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn addresses(&self) -> io::Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn remove(&self, _: &str) -> io::Result<()> {
        Ok(())
    }
    fn quarantine(&self, _: &str, _: &Quarantine) -> io::Result<()> {
        Err(io::Error::other("no space left on device"))
    }
    fn quarantined(&self) -> io::Result<Vec<(String, Quarantine)>> {
        Ok(Vec::new())
    }
    fn load_quarantined(&self, _: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn discard(&self, _: &str) -> io::Result<Option<Quarantine>> {
        Ok(None)
    }
}

#[test]
fn the_delegate_acknowledges_only_what_it_persisted() {
    let fixture = Fixture::new();
    let sealed = fixture.sealed();
    let versions = MemoryDirectoryVersions::default();
    let inbox = MemoryInbox::default();
    let (_, ack) = accept_hand_off(
        &sealed.bytes,
        &fixture.trust(),
        &versions,
        fixture.delegate,
        &fixture.bundle.protocol_version,
        &inbox,
    )
    .expect("accepted");
    assert_eq!(
        ack,
        HandOffAck {
            asset_id: fixture.bundle.asset_id,
            hand_off: sealed.address.clone(),
            delegate_device: fixture.delegate,
        }
    );
    assert_eq!(
        inbox.load(&sealed.address).expect("reads"),
        Some(sealed.bytes.clone())
    );

    // Nothing persisted, nothing acknowledged: the origin keeps the upload.
    assert!(matches!(
        accept_hand_off(
            &sealed.bytes,
            &fixture.trust(),
            &versions,
            fixture.delegate,
            &fixture.bundle.protocol_version,
            &FullDisk,
        ),
        Err(DelegationError::Inbox(_))
    ));
    // And a tampered hand-off is never persisted.
    let empty = MemoryInbox::default();
    let flipped = edited(&sealed.bytes, |h| h.blobs[0][0] ^= 0x01);
    assert!(
        accept_hand_off(
            &flipped,
            &fixture.trust(),
            &versions,
            fixture.delegate,
            &fixture.bundle.protocol_version,
            &empty,
        )
        .is_err()
    );
    assert!(empty.addresses().expect("reads").is_empty());
}

// ─── The origin's ledger ──────────────────────────────────────────────────────

/// A verifier answering the release gate with fixed facts, for the asset's declared hashes.
struct Facts {
    durable: bool,
    receipt: bool,
    reachable: bool,
    asked: Mutex<Vec<Vec<Hash32>>>,
}

impl Facts {
    fn new(durable: bool, receipt: bool) -> Self {
        Self {
            durable,
            receipt,
            reachable: true,
            asked: Mutex::default(),
        }
    }
}

impl StorageVerifier for Facts {
    fn verify(
        &self,
        asset_id: Uuid,
        blob_hashes: &[Hash32],
    ) -> Result<StorageVerdict, VerifierError> {
        self.asked.lock().expect("lock").push(blob_hashes.to_vec());
        if !self.reachable {
            return Err(VerifierError::Transport("offline".into()));
        }
        Ok(StorageVerdict {
            asset_id,
            durable: self.durable,
            blobs: blob_hashes
                .iter()
                .map(|hash| BlobVerdict {
                    hash: *hash,
                    role: CoreBlobRole::Original,
                    stored: self.durable,
                    indexed: self.durable,
                    retrievable: self.durable,
                })
                .collect(),
            checked_at: "2026-10-22T00:00:00Z".into(),
        })
    }

    fn receipt_verified(&self, _: Uuid, _: &[Hash32]) -> Result<bool, VerifierError> {
        Ok(self.receipt)
    }
}

fn ledger_with(fixture: &Fixture) -> (OriginLedger, SealedHandOff) {
    let sealed = fixture.sealed();
    let mut ledger = OriginLedger::new();
    ledger.hand_off(&sealed, BOOT, 1_000, 60_000);
    (ledger, sealed)
}

#[test]
fn the_origin_shows_the_asset_as_handed_to_its_delegate_until_the_server_holds_it() {
    let fixture = Fixture::new();
    let (mut ledger, sealed) = ledger_with(&fixture);
    let delegation = ledger.get(&sealed.asset_id).expect("recorded");
    assert_eq!(delegation.state, DelegationState::HandedOff);
    assert_eq!(
        delegation.status_line("laptop"),
        "handed to laptop, awaiting server"
    );
    // An acknowledgement is not custody: it changes the state, not the pin.
    ledger
        .acknowledge(&HandOffAck {
            asset_id: sealed.asset_id,
            hand_off: sealed.address.clone(),
            delegate_device: fixture.delegate,
        })
        .expect("the ack matches");
    let delegation = ledger.get(&sealed.asset_id).expect("recorded");
    assert_eq!(delegation.state, DelegationState::Acknowledged);
    assert_eq!(
        delegation.status_line("laptop"),
        "handed to laptop, awaiting server"
    );
    // An ack for another hand-off, or from another device, is refused.
    for ack in [
        HandOffAck {
            asset_id: sealed.asset_id,
            hand_off: "00".repeat(32),
            delegate_device: fixture.delegate,
        },
        HandOffAck {
            asset_id: sealed.asset_id,
            hand_off: sealed.address.clone(),
            delegate_device: Uuid::from_u128(99),
        },
    ] {
        assert!(matches!(
            ledger.acknowledge(&ack),
            Err(DelegationError::AckMismatch)
        ));
    }
}

#[test]
fn a_delegate_that_dies_or_withholds_the_bundle_is_re_queued_at_the_monotonic_deadline() {
    let fixture = Fixture::new();
    let (mut ledger, sealed) = ledger_with(&fixture);
    let trust = fixture.trust();
    assert!(ledger.due(&trust, BOOT, 60_999).is_empty(), "not yet");
    assert_eq!(
        ledger.due(&trust, BOOT, 61_000),
        vec![(sealed.asset_id, RequeueReason::DeadlineExpired)]
    );
    let delegation = ledger.get(&sealed.asset_id).expect("recorded");
    assert_eq!(delegation.state, DelegationState::Requeued);
    assert_eq!(
        delegation.status_line("laptop"),
        "re-queued to upload from this device"
    );
    assert!(
        ledger.due(&trust, BOOT, 999_999).is_empty(),
        "re-queued once"
    );
}

#[test]
fn a_deadline_set_before_a_reboot_is_not_trusted_after_it() {
    let fixture = Fixture::new();
    let (mut ledger, sealed) = ledger_with(&fixture);
    // The new boot's clock reads lower than the deadline: measuring it would wait far too long.
    assert_eq!(
        ledger.due(&fixture.trust(), REBOOT, 5),
        vec![(sealed.asset_id, RequeueReason::ClockReset)]
    );
}

#[test]
fn a_revoked_delegate_is_re_queued_at_once() {
    let fixture = Fixture::new();
    let (mut ledger, sealed) = ledger_with(&fixture);
    let revoked = fixture.trust_of(fixture.directory(
        6,
        vec![fixture.origin_entry(), fixture.delegate_entry(true)],
    ));
    assert_eq!(
        ledger.due(&revoked, BOOT, 2_000),
        vec![(sealed.asset_id, RequeueReason::DelegateRevoked)]
    );
}

#[test]
fn the_origin_releases_only_on_its_own_custody_check_of_its_own_hashes() {
    let fixture = Fixture::new();
    let (mut ledger, sealed) = ledger_with(&fixture);
    let id = sealed.asset_id;

    // A delegate's acknowledgement, or anything short of durable + receipted, keeps it pinned.
    for (facts, reason) in [
        (Facts::new(false, true), RetainReason::NotDurable),
        (Facts::new(true, false), RetainReason::ReceiptMissing),
        (
            Facts {
                reachable: false,
                ..Facts::new(true, true)
            },
            RetainReason::VerifyUnavailable,
        ),
    ] {
        assert_eq!(
            ledger.release(id, &facts, true, true).expect("recorded"),
            ReleaseOutcome::Retained(reason)
        );
    }
    // A copy that no longer reads back is kept whatever the server says.
    assert_eq!(
        ledger
            .release(id, &Facts::new(true, true), true, false)
            .expect("recorded"),
        ReleaseOutcome::Retained(RetainReason::ReadBackMismatch)
    );
    // A failed `verify_asset` is not a release either.
    assert_eq!(
        ledger
            .release(id, &Facts::new(true, true), false, true)
            .expect("recorded"),
        ReleaseOutcome::Retained(RetainReason::NotDurable)
    );

    let facts = Facts::new(true, true);
    assert_eq!(
        ledger.release(id, &facts, true, true).expect("recorded"),
        ReleaseOutcome::Released {
            deletes: Vec::new()
        }
    );
    // The gate was asked about the hashes the origin computed itself, never anything else.
    assert_eq!(
        facts.asked.lock().expect("lock").as_slice(),
        std::slice::from_ref(&sealed.release)
    );
    assert_eq!(
        ledger.get(&id).expect("recorded").state,
        DelegationState::Released
    );
}

#[test]
fn when_both_devices_upload_either_receipt_releases_the_original() {
    // Double upload: the delegate was slow, the deadline passed, the origin uploaded itself, and
    // then the delegate uploaded too. Dedup resolved it server-side; the gate names content
    // hashes, so the release does not care which upload landed them.
    let fixture = Fixture::new();
    let (mut ledger, sealed) = ledger_with(&fixture);
    assert_eq!(ledger.due(&fixture.trust(), BOOT, 61_000).len(), 1);
    assert!(matches!(
        ledger.release(sealed.asset_id, &Facts::new(true, true), true, true),
        Ok(ReleaseOutcome::Released { .. })
    ));
}

#[test]
fn a_delete_after_hand_off_is_held_until_the_creates_custody() {
    let fixture = Fixture::new();
    let (mut ledger, sealed) = ledger_with(&fixture);
    let id = sealed.asset_id;
    let delete = b"a signed delete".to_vec();
    assert_eq!(
        ledger.hold_delete(id, delete.clone()),
        DeleteDisposition::Held
    );
    assert_eq!(
        ledger
            .release(id, &Facts::new(false, false), true, true)
            .expect("recorded"),
        ReleaseOutcome::Retained(RetainReason::NotDurable),
        "still not indexed: the delete stays held"
    );
    assert_eq!(
        ledger
            .release(id, &Facts::new(true, true), true, true)
            .expect("recorded"),
        ReleaseOutcome::Released {
            deletes: vec![delete.clone()]
        }
    );
    // After custody, and for an asset never delegated, a delete goes now.
    assert_eq!(
        ledger.hold_delete(id, delete.clone()),
        DeleteDisposition::SendNow(delete.clone())
    );
    assert_eq!(
        OriginLedger::new().hold_delete(Uuid::from_u128(5), delete.clone()),
        DeleteDisposition::SendNow(delete)
    );
}

#[test]
fn the_ledger_survives_a_round_trip_through_its_serialized_form() {
    let fixture = Fixture::new();
    let (ledger, _) = ledger_with(&fixture);
    let json = serde_json::to_string(&ledger).expect("serializes");
    let back: OriginLedger = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(back, ledger);
}

// ─── Uploading under the delegate's own session ───────────────────────────────

/// The `received_at` every mock receipt carries, and the clock the delegate checks them at.
const RECEIVED_AT: &str = "2026-10-22T00:00:00Z";

fn attestation_key() -> HybridSigningKey {
    HybridSigningKey::from_seed64(&[9u8; 64])
}

struct FixedClock;

impl ReleaseClock for FixedClock {
    fn now_unix(&self) -> i64 {
        RECEIVED_AT
            .parse::<jiff::Timestamp>()
            .expect("an instant")
            .as_second()
    }
}

/// A server that stages, finalizes, verifies and receipts, recording every create body.
struct Courier {
    asset_id: String,
    protocol: String,
    creates: Mutex<Vec<serde_json::Value>>,
    sessions: Mutex<HashMap<String, (String, u64, String)>>,
    finalized: Mutex<Vec<(String, u64, String)>>,
    durable: AtomicBool,
}

impl Courier {
    fn answer(&self, req: &MockRequest) -> MockResponse {
        let body: serde_json::Value =
            serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
        match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/storage/verify") => self.verdict(&body),
            ("POST", _) => self.create(body),
            ("PATCH", path) => self.chunk(req, path),
            ("GET", path) if path.ends_with("/receipts") => self.receipts(),
            other => MockResponse::api_error(404, "Not Found", "error.test", &format!("{other:?}")),
        }
    }

    fn create(&self, body: serde_json::Value) -> MockResponse {
        let hash = body["hash"].as_str().expect("a hash").to_owned();
        let size = body["size"].as_u64().expect("a size");
        let role = body["blob_role"].as_str().expect("a role").to_owned();
        let id = format!("sess-{}", &hash[..16]);
        self.sessions
            .lock()
            .expect("lock")
            .insert(id.clone(), (hash, size, role));
        self.creates.lock().expect("lock").push(body);
        MockResponse::new(201, "Created")
            .header("X-Capsule-Offset", "0")
            .json_body(format!(
                r#"{{"id":{id:?},"upload_url":"/upload/{id}","suggested_chunk_size":262144}}"#
            ))
    }

    fn chunk(&self, req: &MockRequest, path: &str) -> MockResponse {
        let id = path.rsplit('/').next().expect("an id");
        let session = self.sessions.lock().expect("lock")[id].clone();
        let offset: u64 = req
            .header("X-Capsule-Offset")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let next = offset + req.body.len() as u64;
        if next == session.1 {
            self.finalized.lock().expect("lock").push(session);
        }
        MockResponse::new(204, "No Content").header("X-Capsule-Offset", next.to_string())
    }

    fn verdict(&self, body: &serde_json::Value) -> MockResponse {
        let finalized = self.finalized.lock().expect("lock");
        let durable = self.durable.load(Ordering::SeqCst);
        let mut all = true;
        let blobs: Vec<String> = body["assets"][0]["blob_hashes"]
            .as_array()
            .expect("hashes")
            .iter()
            .map(|hash| {
                let hash = hash.as_str().expect("a hash");
                let held = durable && finalized.iter().any(|(h, _, _)| h == hash);
                all &= held;
                format!(
                    r#"{{"hash":{hash:?},"role":"original","stored":{held},"indexed":{held},"retrievable":{held}}}"#
                )
            })
            .collect();
        MockResponse::new(200, "OK").json_body(format!(
            r#"{{"verdicts":[{{"asset_id":{aid:?},"durable":{all},"blobs":[{blobs}],"checked_at":{RECEIVED_AT:?}}}]}}"#,
            aid = self.asset_id,
            blobs = blobs.join(","),
        ))
    }

    fn receipts(&self) -> MockResponse {
        let key = attestation_key();
        let receipts: Vec<String> = self
            .finalized
            .lock()
            .expect("lock")
            .iter()
            .enumerate()
            .map(|(seq, (hash, size, role))| {
                let core = CustodyReceiptCore {
                    version: "custody-receipt/v1".into(),
                    crypto_suite_id: capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
                    protocol_version: self.protocol.clone(),
                    server_id: "capsule.example".into(),
                    server_key_id: Hash32::from_bytes([0x11; 32]),
                    receipt_seq: seq as u64 + 1,
                    prior_receipt_hash: None,
                    upload_id: format!("sess-{seq}"),
                    asset_id: self.asset_id.clone(),
                    blob_role: role.clone(),
                    ciphertext_hash: Hash32::from_hex(hash).expect("a hash"),
                    size: *size,
                    envelope_hash: (role == "provenance")
                        .then(|| Hash32::from_hex(hash).expect("a hash")),
                    uploaded_by_user: "someone".into(),
                    uploaded_by_device: None,
                    received_at: RECEIVED_AT.into(),
                    unknown: Default::default(),
                };
                let receipt = CustodyReceipt {
                    server_sig: key.sign(&core.signing_bytes()),
                    core,
                };
                format!(
                    r#"{{"receipt_seq":{},"receipt_cbor":{:?}}}"#,
                    receipt.core.receipt_seq,
                    BASE64.encode(receipt.to_canonical_cbor())
                )
            })
            .collect();
        MockResponse::new(200, "OK").json_body(format!(
            r#"{{"asset_id":{aid:?},"receipts":[{receipts}]}}"#,
            aid = self.asset_id,
            receipts = receipts.join(","),
        ))
    }
}

async fn courier(fixture: &Fixture) -> (std::sync::Arc<Courier>, MockServer) {
    let state = std::sync::Arc::new(Courier {
        asset_id: fixture.bundle.asset_id.to_string(),
        protocol: fixture.bundle.protocol_version.clone(),
        creates: Mutex::default(),
        sessions: Mutex::default(),
        finalized: Mutex::default(),
        durable: AtomicBool::new(true),
    });
    let handler = state.clone();
    let server = MockServer::start(move |req: &MockRequest| handler.answer(req)).await;
    (state, server)
}

fn coordinator(server: &MockServer) -> ReleaseCoordinator<FixedClock> {
    let client = StorageVerifyClient::new(VerifyTransport::with_static_token(
        reqwest::Client::new(),
        server.base_url(),
        VerifyToken("delegate-token".into()),
    ));
    let keys: Vec<HybridVerifyingKey> = vec![attestation_key().verifying_key()];
    ReleaseCoordinator::with_clock(client, keys, 60, FixedClock)
}

fn accepted(fixture: &Fixture, inbox: &MemoryInbox) -> VerifiedHandOff {
    let sealed = fixture.sealed();
    accept_hand_off(
        &sealed.bytes,
        &fixture.trust(),
        &MemoryDirectoryVersions::default(),
        fixture.delegate,
        &fixture.bundle.protocol_version,
        inbox,
    )
    .expect("accepted")
    .0
}

#[tokio::test]
async fn the_delegate_uploads_every_request_verbatim_then_deletes_its_copy_on_custody() {
    let fixture = Fixture::new();
    let inbox = MemoryInbox::default();
    let hand_off = accepted(&fixture, &inbox);
    let (state, server) = courier(&fixture).await;
    let client = server.client(&fixture.bundle.protocol_version);
    let scheduler = StagedScheduler::new(
        capsule_library::import::UploadPolicy::Full,
        ConnectionClass::Unmetered,
    );

    let report = upload_hand_off(&client, &scheduler, &hand_off)
        .await
        .expect("the upload runs");
    assert!(report.deferred.is_empty());
    assert_eq!(report.pushed.len(), hand_off.members().count());
    // Every create the server saw is the origin's request, field for field.
    let sent: Vec<serde_json::Value> = state.creates.lock().expect("lock").clone();
    let sealed: Vec<serde_json::Value> = hand_off
        .members()
        .map(|(request, _)| serde_json::to_value(request).expect("serializes"))
        .collect();
    assert_eq!(sent, sealed);

    // Not durable yet: the copy stays.
    state.durable.store(false, Ordering::SeqCst);
    assert_eq!(
        settle(&coordinator(&server), &hand_off, &report.deferred, &inbox)
            .await
            .expect("settles"),
        Settled::Kept(RetainReason::NotDurable)
    );
    assert_eq!(
        inbox.addresses().expect("reads"),
        vec![hand_off.address.clone()]
    );

    state.durable.store(true, Ordering::SeqCst);
    assert_eq!(
        settle(&coordinator(&server), &hand_off, &report.deferred, &inbox)
            .await
            .expect("settles"),
        Settled::Removed
    );
    assert!(inbox.addresses().expect("reads").is_empty());
}

#[tokio::test]
async fn the_delegate_follows_its_own_metering_policy_and_keeps_what_it_deferred() {
    let fixture = Fixture::new();
    let inbox = MemoryInbox::default();
    let hand_off = accepted(&fixture, &inbox);
    let (state, server) = courier(&fixture).await;
    let client = server.client(&fixture.bundle.protocol_version);
    // The delegate is on a metered link: its own staged policy defers the original tier.
    let scheduler = StagedScheduler::new(
        capsule_library::import::UploadPolicy::Staged,
        ConnectionClass::Metered,
    );
    let report = upload_hand_off(&client, &scheduler, &hand_off)
        .await
        .expect("the upload runs");
    let original = hand_off
        .members()
        .find(|(request, _)| request.blob_role == BlobRole::Original)
        .map(|(request, _)| request.hash.clone())
        .expect("an original");
    assert_eq!(report.deferred, vec![original.clone()]);
    assert!(
        state
            .creates
            .lock()
            .expect("lock")
            .iter()
            .all(|body| body["hash"] != serde_json::json!(original)),
        "the original was not sent"
    );
    assert_eq!(
        settle(&coordinator(&server), &hand_off, &report.deferred, &inbox)
            .await
            .expect("settles"),
        Settled::Kept(RetainReason::NotDurable)
    );
    assert_eq!(inbox.addresses().expect("reads").len(), 1);
}

// ─── A held hand-off that can never succeed ───────────────────────────────────

/// A server that takes every create and refuses each blob's finalization with `code` at
/// `status`.
async fn refusing_server(status: u16, reason: &'static str, code: &'static str) -> MockServer {
    let sessions: std::sync::Arc<Mutex<HashMap<String, u64>>> = std::sync::Arc::default();
    MockServer::start(move |req: &MockRequest| {
        if req.method == "POST" {
            let body: serde_json::Value = serde_json::from_slice(&req.body).expect("a create");
            let hash = body["hash"].as_str().expect("a hash").to_owned();
            let id = format!("sess-{}", &hash[..16]);
            sessions
                .lock()
                .expect("lock")
                .insert(id.clone(), body["size"].as_u64().expect("a size"));
            return MockResponse::new(201, "Created")
                .header("X-Capsule-Offset", "0")
                .json_body(format!(
                    r#"{{"id":{id:?},"upload_url":"/upload/{id}","suggested_chunk_size":262144}}"#
                ));
        }
        let id = req.path.rsplit('/').next().expect("an id").to_owned();
        let size = sessions.lock().expect("lock")[&id];
        let offset: u64 = req
            .header("X-Capsule-Offset")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let next = offset + req.body.len() as u64;
        if next == size {
            return MockResponse::api_error(status, reason, code, "refused");
        }
        MockResponse::new(204, "No Content").header("X-Capsule-Offset", next.to_string())
    })
    .await
}

/// Upload the accepted hand-off through `server` and dispose of the failure.
async fn upload_and_dispose(
    fixture: &Fixture,
    inbox: &MemoryInbox,
    hand_off: &VerifiedHandOff,
    server: &MockServer,
) -> (DelegationError, HeldFailure) {
    let scheduler = StagedScheduler::new(
        capsule_library::import::UploadPolicy::Full,
        ConnectionClass::Unmetered,
    );
    let error = upload_hand_off(
        &server.client(&fixture.bundle.protocol_version),
        &scheduler,
        hand_off,
    )
    .await
    .expect_err("the server refuses the upload");
    let disposed = dispose_failure(
        inbox,
        &hand_off.address,
        hand_off.asset_id(),
        "upload",
        &error,
    )
    .expect("disposes");
    (error, disposed)
}

/// The inbox no longer retries the copy under `address`, still holds its bytes byte for byte,
/// and lists it once with a note naming `kind` and `code`.
fn assert_quarantined(
    inbox: &MemoryInbox,
    address: &str,
    bytes: &[u8],
    asset_id: Uuid,
    kind: &str,
    code: Option<&str>,
) {
    assert!(
        inbox.addresses().expect("reads").is_empty(),
        "a quarantined copy is not retried"
    );
    assert_eq!(
        inbox.load_quarantined(address).expect("reads").as_deref(),
        Some(bytes),
        "the bytes are kept"
    );
    let listed = inbox.quarantined().expect("reads");
    assert_eq!(listed.len(), 1, "listed once: {listed:?}");
    let (listed_address, note) = &listed[0];
    assert_eq!(listed_address, address);
    assert_eq!(note.asset_id, asset_id);
    assert_eq!(note.kind, kind);
    assert_eq!(note.code.as_deref(), code);
}

#[test]
fn only_a_refusal_no_retry_can_change_is_permanent() {
    let rejected = |status: u16, code: &str| {
        DelegationError::Upload(UploadError::Rejected {
            status,
            code: Some(code.to_owned()),
            message: "refused".into(),
        })
    };
    let device = Uuid::from_u128(7);
    for permanent in [
        DelegationError::Peering(PeeringError::RevokedDevice(device)),
        DelegationError::Peering(PeeringError::UnknownDevice(device)),
        DelegationError::Malformed("short".into()),
        DelegationError::WrongDelegate { addressed: device },
        DelegationError::Tampered("a member".into()),
        rejected(400, capsule_i18n::error_codes::UPLOAD_ENVELOPE_REJECTED),
        rejected(400, capsule_i18n::error_codes::UPLOAD_ENVELOPE_MISMATCH),
        rejected(409, capsule_i18n::error_codes::UPLOAD_STALE_REVIVAL),
        // The create-time answers to what finalization answers with `envelope_rejected`.
        rejected(
            400,
            capsule_i18n::error_codes::UPLOAD_TIMESTAMP_OUT_OF_RANGE,
        ),
        rejected(403, capsule_i18n::error_codes::UPLOAD_ALBUM_ACCESS_DENIED),
        rejected(403, capsule_i18n::error_codes::UPLOAD_DEVICE_NOT_AUTHORIZED),
    ] {
        assert!(permanent.is_permanent(), "{permanent:?}");
    }
    for transient in [
        DelegationError::Peering(PeeringError::DirectoryRollback {
            pinned: 4,
            highest: 5,
        }),
        DelegationError::Peering(PeeringError::ForeignIdentity),
        DelegationError::Peering(PeeringError::Io("disk".into())),
        DelegationError::UnsupportedFormat { found: 9 },
        DelegationError::ProtocolUnsupported {
            bundle: "2099-01-01".into(),
            ours: "2026-01-01".into(),
        },
        DelegationError::Inbox("disk".into()),
        DelegationError::Upload(UploadError::Transport("reset".into())),
        DelegationError::Upload(UploadError::RetriesExhausted("stuck".into())),
        rejected(403, capsule_i18n::error_codes::QUOTA_EXCEEDED),
        rejected(503, capsule_i18n::error_codes::UPLOAD_UNAVAILABLE),
        DelegationError::Upload(UploadError::Rejected {
            status: 400,
            code: None,
            message: "no code".into(),
        }),
    ] {
        assert!(!transient.is_permanent(), "{transient:?}");
    }
}

#[test]
fn a_held_hand_off_whose_origin_was_revoked_after_acceptance_is_quarantined_not_deleted() {
    let fixture = Fixture::new();
    let inbox = MemoryInbox::default();
    let hand_off = accepted(&fixture, &inbox);
    // The account revokes the origin after the delegate took the hand-off.
    let mut revoked_origin = fixture.origin_entry();
    revoked_origin.revoked_at = Some("2026-10-02T00:00:00Z".to_owned());
    let revoked =
        fixture.trust_of(fixture.directory(6, vec![revoked_origin, fixture.delegate_entry(false)]));
    let held = inbox.load(&hand_off.address).expect("reads").expect("held");
    let error = verify_hand_off(
        &held,
        &revoked,
        &MemoryDirectoryVersions::default(),
        fixture.delegate,
        &fixture.bundle.protocol_version,
    )
    .expect_err("a revoked origin's hand-off no longer verifies");
    assert!(matches!(
        error,
        DelegationError::Peering(PeeringError::RevokedDevice(_))
    ));
    // A revoked origin is often a lost device: this copy may be the only one left.
    assert_eq!(
        dispose_failure(
            &inbox,
            &hand_off.address,
            hand_off.asset_id(),
            "revoked_device",
            &error
        )
        .expect("disposes"),
        HeldFailure::Quarantined
    );
    assert_quarantined(
        &inbox,
        &hand_off.address,
        &held,
        hand_off.asset_id(),
        "revoked_device",
        None,
    );
    // Quarantining again (a crash before the move was observed) changes nothing.
    assert_eq!(
        dispose_failure(
            &inbox,
            &hand_off.address,
            hand_off.asset_id(),
            "revoked_device",
            &error
        )
        .expect("disposes"),
        HeldFailure::Quarantined
    );
    assert_eq!(inbox.quarantined().expect("reads").len(), 1);

    // Only the user deletes it, and only a quarantined copy.
    let note = inbox
        .discard(&hand_off.address)
        .expect("discards")
        .expect("it was quarantined");
    assert_eq!(note.kind, "revoked_device");
    assert!(inbox.quarantined().expect("reads").is_empty());
    assert_eq!(
        inbox.load_quarantined(&hand_off.address).expect("reads"),
        None
    );
    assert_eq!(inbox.discard(&hand_off.address).expect("discards"), None);
}

#[test]
fn a_copy_that_cannot_be_quarantined_is_reported_and_not_lost() {
    let error = DelegationError::Peering(PeeringError::RevokedDevice(Uuid::from_u128(9)));
    assert!(matches!(
        dispose_failure(&FullDisk, "ab", Uuid::nil(), "revoked_device", &error),
        Err(DelegationError::Inbox(_))
    ));
}

#[test]
fn discarding_leaves_a_held_copy_alone() {
    let fixture = Fixture::new();
    let inbox = MemoryInbox::default();
    let hand_off = accepted(&fixture, &inbox);
    assert_eq!(inbox.discard(&hand_off.address).expect("discards"), None);
    assert_eq!(inbox.addresses().expect("reads"), vec![hand_off.address]);
}

#[test]
fn a_held_hand_off_this_device_cannot_judge_now_is_kept() {
    let fixture = Fixture::new();
    let inbox = MemoryInbox::default();
    let hand_off = accepted(&fixture, &inbox);
    let held = inbox.load(&hand_off.address).expect("reads").expect("held");
    // A stale directory is this device's problem, not the hand-off's.
    let error = verify_hand_off(
        &held,
        &fixture.trust(),
        &MemoryDirectoryVersions::starting_at(6),
        fixture.delegate,
        &fixture.bundle.protocol_version,
    )
    .expect_err("a rolled-back directory is refused");
    assert_eq!(
        dispose_failure(
            &inbox,
            &hand_off.address,
            hand_off.asset_id(),
            "directory_rollback",
            &error
        )
        .expect("disposes"),
        HeldFailure::Kept
    );
    assert_eq!(inbox.addresses().expect("reads"), vec![hand_off.address]);
    assert!(inbox.quarantined().expect("reads").is_empty());
}

#[tokio::test]
async fn a_hand_off_the_server_refuses_for_good_is_quarantined() {
    let fixture = Fixture::new();
    let inbox = MemoryInbox::default();
    let hand_off = accepted(&fixture, &inbox);
    let held = inbox.load(&hand_off.address).expect("reads").expect("held");
    // The answer a stale-epoch create, a revoked signer, a closed album, withdrawn write access
    // or a conflicting role meets.
    let server = refusing_server(
        400,
        "Bad Request",
        capsule_i18n::error_codes::UPLOAD_ENVELOPE_REJECTED,
    )
    .await;
    let (error, disposed) = upload_and_dispose(&fixture, &inbox, &hand_off, &server).await;
    assert_eq!(
        error.error_code(),
        Some(capsule_i18n::error_codes::UPLOAD_ENVELOPE_REJECTED)
    );
    assert_eq!(disposed, HeldFailure::Quarantined);
    assert_quarantined(
        &inbox,
        &hand_off.address,
        &held,
        hand_off.asset_id(),
        "upload",
        Some(capsule_i18n::error_codes::UPLOAD_ENVELOPE_REJECTED),
    );
}

/// A server that refuses every `POST /v1/upload` (session creation) with `status` and `code`.
async fn refusing_create(status: u16, reason: &'static str, code: &'static str) -> MockServer {
    MockServer::start(move |_: &MockRequest| {
        MockResponse::api_error(status, reason, code, "refused at creation")
    })
    .await
}

#[tokio::test]
async fn a_create_time_refusal_no_retry_can_change_is_classified_as_its_finalize_twin() {
    // Each is what creation answers for a condition finalization answers with
    // `envelope_rejected`: the same condition is permanent whichever step meets it.
    for (status, reason, code) in [
        (
            400,
            "Bad Request",
            capsule_i18n::error_codes::UPLOAD_TIMESTAMP_OUT_OF_RANGE,
        ),
        (
            403,
            "Forbidden",
            capsule_i18n::error_codes::UPLOAD_ALBUM_ACCESS_DENIED,
        ),
        (
            403,
            "Forbidden",
            capsule_i18n::error_codes::UPLOAD_DEVICE_NOT_AUTHORIZED,
        ),
    ] {
        let fixture = Fixture::new();
        let inbox = MemoryInbox::default();
        let hand_off = accepted(&fixture, &inbox);
        let held = inbox.load(&hand_off.address).expect("reads").expect("held");
        let server = refusing_create(status, reason, code).await;
        let (error, disposed) = upload_and_dispose(&fixture, &inbox, &hand_off, &server).await;
        assert_eq!(error.error_code(), Some(code));
        assert!(error.is_permanent(), "{code}");
        assert_eq!(disposed, HeldFailure::Quarantined, "{code}");
        assert_quarantined(
            &inbox,
            &hand_off.address,
            &held,
            hand_off.asset_id(),
            "upload",
            Some(code),
        );
    }
}

#[tokio::test]
async fn a_hand_off_the_server_refuses_for_now_is_kept() {
    let fixture = Fixture::new();
    let inbox = MemoryInbox::default();
    let hand_off = accepted(&fixture, &inbox);
    let server = refusing_server(403, "Forbidden", capsule_i18n::error_codes::QUOTA_EXCEEDED).await;
    let (error, disposed) = upload_and_dispose(&fixture, &inbox, &hand_off, &server).await;
    assert_eq!(
        error.error_code(),
        Some(capsule_i18n::error_codes::QUOTA_EXCEEDED)
    );
    assert_eq!(disposed, HeldFailure::Kept);
    assert_eq!(inbox.addresses().expect("reads"), vec![hand_off.address]);
    assert!(inbox.quarantined().expect("reads").is_empty());
}

// ─── Over the peering channel ─────────────────────────────────────────────────

/// The delegate's signing key: the one `delegate_entry` publishes.
fn delegate_signer() -> HybridSigningKey {
    HybridSigningKey::from_seed64(&[0xDE; 64])
}

/// A real mutual TLS 1.3 channel over loopback between the origin (dialing, signing with the
/// library's own device signer) and the delegate (listening), both pinning `trust`. Returns
/// `(origin's channel, delegate's channel)`.
async fn channel_pair(fixture: &Fixture, trust: &PinnedTrust) -> (PeerChannel, PeerChannel) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let address = listener.local_addr().expect("an address");
    let delegate_key = delegate_signer();
    let delegate_versions = MemoryDirectoryVersions::default();
    let origin_versions = MemoryDirectoryVersions::default();
    let limiter = HandshakeLimiter::default();
    let delegate_side = async {
        let (tcp, _) = listener.accept().await.expect("accepts");
        let local = LocalPeer {
            device_id: fixture.delegate,
            signer: &delegate_key,
            trust,
            versions: &delegate_versions,
        };
        accept(tcp, local, &limiter).await
    };
    let origin_side = async {
        let tcp = TcpStream::connect(address).await.expect("dials");
        let local = LocalPeer {
            device_id: fixture.origin(),
            signer: fixture.workspace.device_signer(),
            trust,
            versions: &origin_versions,
        };
        connect(tcp, local).await
    };
    let (delegate, origin) = tokio::join!(delegate_side, origin_side);
    (
        origin.expect("the origin verifies the delegate"),
        delegate.expect("the delegate verifies the origin"),
    )
}

/// The delegate's half of a session: read the listing and take every hand-off.
async fn take(
    fixture: &Fixture,
    channel: PeerChannel,
    trust: &PinnedTrust,
    inbox: &MemoryInbox,
    partials: &mut BTreeMap<String, Vec<u8>>,
) -> TakenHandOffs {
    let client = PeerClient::open(channel).await.expect("the listing reads");
    take_hand_offs(
        &client,
        trust,
        &MemoryDirectoryVersions::default(),
        fixture.delegate,
        &fixture.bundle.protocol_version,
        inbox,
        partials,
    )
    .await
}

#[tokio::test]
async fn a_hand_off_crosses_a_verified_tls_channel_and_is_acknowledged_over_it() {
    let fixture = Fixture::new();
    let trust = fixture.trust();
    let sealed = fixture.sealed();
    let (mut origin, delegate) = channel_pair(&fixture, &trust).await;
    // Each side holds the identity the handshake proved, from the library's own device signer.
    assert_eq!(origin.peer().device_id, fixture.delegate);
    assert_eq!(delegate.peer().device_id, fixture.origin());

    let mut ledger = OriginLedger::new();
    ledger.hand_off(&sealed, BOOT, 0, 60_000);
    let inbox = MemoryInbox::default();
    let mut partials = BTreeMap::new();
    let versions = MemoryDirectoryVersions::default();
    let serving = offer_hand_offs(
        &mut origin,
        std::slice::from_ref(&sealed),
        &trust,
        &versions,
        |ack| ledger.acknowledge(ack),
    );
    let taking = take(&fixture, delegate, &trust, &inbox, &mut partials);
    let (served, taken) = tokio::join!(serving, taking);
    let served = served.expect("the session ends when the delegate is done");

    assert!(!taken.severed);
    assert_eq!(taken.taken.len(), 1);
    let ack = taken.taken[0].ack.clone().expect("taken");
    assert!(taken.taken[0].error.is_none(), "{taken:?}");
    assert_eq!(ack.hand_off, sealed.address);
    assert_eq!(ack.delegate_device, fixture.delegate);
    assert_eq!(served.acks, vec![ack]);
    assert_eq!(
        served.served,
        vec![ServedRange {
            address: sealed.address.clone(),
            start: 0,
            len: sealed.bytes.len() as u64,
        }]
    );
    // Persisted durably before the acknowledgement, byte for byte.
    assert_eq!(
        inbox.load(&sealed.address).expect("reads"),
        Some(sealed.bytes.clone())
    );
    assert!(partials.is_empty());
    assert_eq!(
        ledger.get(&sealed.asset_id).expect("recorded").state,
        DelegationState::Acknowledged
    );
}

#[tokio::test]
async fn a_severed_pull_resumes_on_a_new_channel_without_re_fetching_a_byte() {
    let fixture = Fixture::new();
    let trust = fixture.trust();
    let sealed = fixture.sealed();
    let total = sealed.bytes.len() as u64;
    let half = total / 2;
    let inbox = MemoryInbox::default();
    let mut partials = BTreeMap::new();

    // Session 1: the channel is cut after half the hand-off crossed it.
    let mut severing = Offering::new();
    severing.offer(sealed.bytes.clone()).expect("offers");
    severing.sever_after = Some(half);
    let (mut origin, delegate) = channel_pair(&fixture, &trust).await;
    let versions = MemoryDirectoryVersions::default();
    let serving = serve(&mut origin, &severing, &trust, &versions, |_| Ok(()));
    let taking = take(&fixture, delegate, &trust, &inbox, &mut partials);
    let (served, taken) = tokio::join!(serving, taking);
    assert!(served.is_err());
    assert!(taken.severed);
    assert!(taken.taken[0].ack.is_none());
    assert_eq!(
        taken.taken[0].error.as_ref().map(DelegationError::kind),
        Some("peering_io")
    );
    assert_eq!(
        partials.get(&sealed.address).map(Vec::len),
        Some(half as usize),
        "the delegate keeps what crossed before the cut"
    );
    assert!(
        inbox.addresses().expect("reads").is_empty(),
        "nothing partial is persisted"
    );

    // Session 2: a fresh handshake, and the pull resumes after what is held.
    let mut whole = Offering::new();
    whole.offer(sealed.bytes.clone()).expect("offers");
    let (mut origin, delegate) = channel_pair(&fixture, &trust).await;
    let serving = serve(&mut origin, &whole, &trust, &versions, |_| Ok(()));
    let taking = take(&fixture, delegate, &trust, &inbox, &mut partials);
    let (served, taken) = tokio::join!(serving, taking);
    let served = served.expect("the session ends cleanly");
    assert!(taken.taken[0].ack.is_some(), "{taken:?}");
    assert_eq!(
        served.served,
        vec![ServedRange {
            address: sealed.address.clone(),
            start: half,
            len: total - half,
        }],
        "zero bytes cross the channel twice"
    );
    assert!(partials.is_empty());
    assert_eq!(
        inbox.load(&sealed.address).expect("reads"),
        Some(sealed.bytes)
    );
}

#[tokio::test]
async fn a_delegate_revoked_mid_session_is_refused_at_its_next_request() {
    let fixture = Fixture::new();
    let trust = fixture.trust();
    let sealed = fixture.sealed();
    let (mut origin, delegate) = channel_pair(&fixture, &trust).await;
    let current = SharedTrust::new(trust.clone());
    let revoked = fixture.trust_of(fixture.directory(
        6,
        vec![fixture.origin_entry(), fixture.delegate_entry(true)],
    ));
    let mut offering = Offering::new();
    let address = offering.offer(sealed.bytes.clone()).expect("offers");
    let versions = MemoryDirectoryVersions::default();

    let serving = serve(&mut origin, &offering, &current, &versions, |_| Ok(()));
    let pulling = async {
        let client = PeerClient::open(delegate).await.expect("the listing reads");
        let first = client.range(&address, 0, Some(16)).await.expect("served");
        assert_eq!(first, sealed.bytes[..16]);
        // The directory refreshes on the origin between two requests of one session.
        current.replace(revoked);
        client.range(&address, 16, None).await
    };
    let (served, second) = tokio::join!(serving, pulling);
    let refusal = second.expect_err("the next request is refused");
    assert!(refusal.received.is_empty(), "not one more byte crosses");
    assert!(
        matches!(&refusal.error, PeeringError::PeerRefused { kind, .. } if kind == "revoked_device"),
        "{:?}",
        refusal.error
    );
    assert!(
        matches!(served, Err(PeeringError::RevokedDevice(id)) if id == fixture.delegate),
        "the origin ends the session: {served:?}"
    );
}

#[tokio::test]
async fn an_acknowledgement_counts_only_for_the_verified_peer_and_an_offered_hand_off() {
    let fixture = Fixture::new();
    let trust = fixture.trust();
    let sealed = fixture.sealed();
    let (mut origin, delegate) = channel_pair(&fixture, &trust).await;
    let mut seen = Vec::new();
    let versions = MemoryDirectoryVersions::default();
    let serving = offer_hand_offs(
        &mut origin,
        std::slice::from_ref(&sealed),
        &trust,
        &versions,
        |ack| {
            seen.push(ack.clone());
            Ok(())
        },
    );
    let genuine = HandOffAck {
        asset_id: sealed.asset_id,
        hand_off: sealed.address.clone(),
        delegate_device: fixture.delegate,
    };
    let acking = async {
        let client = PeerClient::open(delegate).await.expect("the listing reads");
        // Claiming to speak for another device, or for a hand-off this session did not offer,
        // or for another asset, is refused; the session goes on.
        let mut outcomes = Vec::new();
        for forged in [
            HandOffAck {
                delegate_device: fixture.origin(),
                ..genuine.clone()
            },
            HandOffAck {
                hand_off: "0".repeat(64),
                ..genuine.clone()
            },
            HandOffAck {
                asset_id: Uuid::from_u128(9),
                ..genuine.clone()
            },
        ] {
            outcomes.push(client.acknowledge(&forged).await);
        }
        outcomes.push(client.acknowledge(&genuine).await);
        client.finish().await.expect("ends");
        outcomes
    };
    let (served, outcomes) = tokio::join!(serving, acking);
    for refused in &outcomes[..3] {
        assert!(
            matches!(refused, Err(PeeringError::PeerRefused { kind, .. }) if kind == "ack_mismatch"),
            "{refused:?}"
        );
    }
    assert!(outcomes[3].is_ok());
    assert_eq!(seen, vec![genuine.clone()]);
    assert_eq!(served.expect("ends cleanly").acks, vec![genuine]);
}

#[tokio::test]
async fn the_origin_offers_only_hand_offs_sealed_for_the_verified_peer() {
    let fixture = Fixture::new();
    let trust = fixture.trust();
    let mut sealed = fixture.sealed();
    sealed.delegate = Uuid::from_u128(7);
    let (mut origin, _delegate) = channel_pair(&fixture, &trust).await;
    let offered = offer_hand_offs(
        &mut origin,
        &[sealed],
        &trust,
        &MemoryDirectoryVersions::default(),
        |_| Ok(()),
    )
    .await;
    assert!(matches!(
        offered,
        Err(DelegationError::WrongDelegate { addressed }) if addressed == Uuid::from_u128(7)
    ));
}

#[test]
fn a_hand_off_a_peer_did_not_seal_itself_is_refused_before_it_is_persisted() {
    let fixture = Fixture::new();
    let sealed = fixture.sealed();
    let inbox = MemoryInbox::default();
    let other = Uuid::from_u128(7);
    let refused = accept_hand_off_from(
        &sealed.bytes,
        other,
        &fixture.trust(),
        &MemoryDirectoryVersions::default(),
        fixture.delegate,
        &fixture.bundle.protocol_version,
        &inbox,
    );
    assert!(matches!(
        refused,
        Err(DelegationError::WrongOrigin { origin, peer }) if origin == fixture.origin() && peer == other
    ));
    assert!(inbox.addresses().expect("reads").is_empty());
    let (_, ack) = accept_hand_off_from(
        &sealed.bytes,
        fixture.origin(),
        &fixture.trust(),
        &MemoryDirectoryVersions::default(),
        fixture.delegate,
        &fixture.bundle.protocol_version,
        &inbox,
    )
    .expect("the origin's own hand-off is taken");
    assert_eq!(ack.hand_off, sealed.address);
}

#[tokio::test]
async fn a_listing_past_its_bounds_is_refused_whole() {
    let fixture = Fixture::new();
    let trust = fixture.trust();
    for artifacts in [
        vec![
            Offered {
                address: "a".repeat(64),
                len: 1,
            };
            MAX_LISTING + 1
        ],
        vec![Offered {
            address: "not an address".into(),
            len: 1,
        }],
        vec![Offered {
            address: "a".repeat(64),
            len: MAX_ARTIFACT_LEN + 1,
        }],
    ] {
        let (mut origin, delegate) = channel_pair(&fixture, &trust).await;
        origin
            .send(&Message::Listing { artifacts })
            .await
            .expect("sends");
        assert!(matches!(
            PeerClient::open(delegate).await,
            Err(PeeringError::Codec(_))
        ));
    }
}

#[tokio::test]
async fn a_frame_past_what_was_asked_breaks_the_pull_before_it_is_read() {
    let fixture = Fixture::new();
    let trust = fixture.trust();
    let address = "b".repeat(64);
    let listing = Message::Listing {
        artifacts: vec![Offered {
            address: address.clone(),
            len: 64,
        }],
    };

    // A reply longer than the request asked for.
    let (mut origin, delegate) = channel_pair(&fixture, &trust).await;
    origin.send(&listing).await.expect("sends");
    let client = PeerClient::open(delegate).await.expect("opens");
    let lying = async {
        let request = origin.recv().await.expect("a request");
        assert!(matches!(
            request,
            Message::Range {
                max_len: Some(4),
                ..
            }
        ));
        origin
            .send(&Message::RangeReply { len: 5 })
            .await
            .expect("sends");
    };
    let ((), pulled) = tokio::join!(lying, client.range(&address, 0, Some(4)));
    assert!(matches!(
        pulled,
        Err(RangeFailure {
            error: PeeringError::Codec(_),
            ..
        })
    ));
    assert!(client.is_broken());
    assert!(
        client
            .acknowledge(&HandOffAck {
                asset_id: Uuid::nil(),
                hand_off: address.clone(),
                delegate_device: fixture.delegate,
            })
            .await
            .is_err(),
        "a broken client sends nothing more"
    );

    // A data frame larger than the bytes still owed.
    let (mut origin, delegate) = channel_pair(&fixture, &trust).await;
    origin.send(&listing).await.expect("sends");
    let client = PeerClient::open(delegate).await.expect("opens");
    let flooding = async {
        origin.recv().await.expect("a request");
        origin
            .send(&Message::RangeReply { len: 4 })
            .await
            .expect("sends");
        origin.send_data(&[0; 8]).await.expect("sends");
    };
    let ((), pulled) = tokio::join!(flooding, client.range(&address, 0, Some(4)));
    assert!(matches!(
        pulled,
        Err(RangeFailure {
            error: PeeringError::Codec(_),
            ..
        })
    ));
}

#[tokio::test]
async fn a_peer_that_stops_answering_times_out() {
    let fixture = Fixture::new();
    let trust = fixture.trust();
    let address = "c".repeat(64);
    let (mut origin, delegate) = channel_pair(&fixture, &trust).await;
    origin
        .send(&Message::Listing {
            artifacts: vec![Offered {
                address: address.clone(),
                len: 64,
            }],
        })
        .await
        .expect("sends");
    let client = PeerClient::open(delegate.with_io_timeout(Duration::from_millis(100)))
        .await
        .expect("opens");
    // The origin holds its end open and never answers.
    let pulled = client.range(&address, 0, None).await;
    assert!(matches!(
        pulled,
        Err(RangeFailure {
            error: PeeringError::Timeout(_),
            ..
        })
    ));
    assert!(client.is_broken());
    drop(origin);
}
