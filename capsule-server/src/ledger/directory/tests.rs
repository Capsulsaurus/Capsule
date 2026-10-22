//! The directory history's contract: a manifest verifies against the version in force at its
//! position, the store's refusals are re-run, and a version with no usable copy vouches for
//! nothing.
//!
//! The fixtures are `pub(crate)` so the rebuild's own suite publishes directories the same way.

use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::keys::{AmkVersion, DeviceEntry, DirectoryCore, HybridSigningKey};
use capsule_core::crypto::provenance::{ASSET_MANIFEST_VERSION, Action, KeyMode, ManifestCore};
use uuid::Uuid;

use super::*;
use crate::attestation::{LocalAttestationKey, ReceiptSigner};
use crate::ledger::{InMemoryLedger, LedgerEvent};

/// A directory entry for `device`, signing with `key`.
pub(crate) fn device(
    device: Uuid,
    key: &HybridSigningKey,
    added_at: &str,
    revoked_at: Option<&str>,
) -> DeviceEntry {
    DeviceEntry {
        device_id: device,
        dsk_public: key.verifying_key(),
        dek_public: None,
        added_at: added_at.to_owned(),
        revoked_at: revoked_at.map(str::to_owned),
        unknown: Default::default(),
    }
}

/// `user`'s directory at `version`, signed by `ik`, as the bytes a client publishes.
pub(crate) fn document(
    ik: &HybridSigningKey,
    user: Uuid,
    version: u64,
    devices: Vec<DeviceEntry>,
) -> Vec<u8> {
    let core = DirectoryCore {
        user_id: user,
        directory_version: version,
        updated_at: "2026-01-01T00:00:00Z".to_owned(),
        devices,
        unknown: Default::default(),
    };
    capsule_core::cbor::to_canonical_vec(&core.sign(ik)).expect("a directory serializes")
}

/// Publish `document` to `user`'s ledger as the route does: the copy, then the entry.
pub(crate) async fn publish(
    ledger: &dyn OwnerLedger,
    signer: &dyn ReceiptSigner,
    ik: &HybridSigningKey,
    user: Uuid,
    version: u64,
    document: Vec<u8>,
    at: Timestamp,
) {
    let owner = OwnerId::new(user.to_string());
    let document_hash = hash_bytes(&document);
    ledger
        .attach(
            &owner,
            document_hash,
            DirectoryCopy {
                identity_key: ik.verifying_key().to_bytes(),
                document,
            }
            .encode(),
        )
        .await
        .expect("the copy is kept");
    ledger
        .append(
            &owner,
            LedgerEvent::Directory {
                user_id: user.to_string(),
                directory_version: version,
                document_hash,
            },
            at,
            signer,
        )
        .await
        .expect("the entry lands");
}

/// A `create` manifest `device` of `user` signed with `key`, timestamped `timestamp`.
fn manifest(user: Uuid, device: Uuid, key: &HybridSigningKey, timestamp: &str) -> AssetManifest {
    let core = ManifestCore {
        version: ASSET_MANIFEST_VERSION.to_owned(),
        crypto_suite_id: 1,
        protocol_version: "2026-01-01".to_owned(),
        file_id: Uuid::from_u128(1),
        album_id: Uuid::from_u128(2),
        amk_version: AmkVersion(0),
        ciphertext_hash: hash_bytes(b"original"),
        plaintext_size: 1024,
        chunk_size: 65_536,
        nonce_prefix: [0; 7],
        key_mode: KeyMode::Derived,
        wrapped_file_key: None,
        metadata_blob_hash: Some(hash_bytes(b"metadata")),
        created_by_user: user,
        created_by_device: device,
        client_version: "capsule-cli/0.1.0".to_owned(),
        timestamp: timestamp.to_owned(),
        action: Action::Create,
        prior_provenance_hash: None,
        upgraded_from: None,
        unknown: Default::default(),
        retention_until: None,
    };
    core.sign(key, &HybridSigningKey::from_seed64(&[2; 64]))
        .expect("a manifest signs")
}

const USER: Uuid = Uuid::from_u128(0x0193_7b7c_0000_7000_8000_0000_0000_0001);
const ADDED: &str = "2026-01-01T00:00:00Z";
const SIGNED: &str = "2026-02-01T00:00:00Z";

fn at(second: i64) -> Timestamp {
    Timestamp::from_second(1_767_225_600 + second).expect("a valid instant")
}

fn signer() -> LocalAttestationKey {
    LocalAttestationKey::new("server.test", HybridSigningKey::from_seed64(&[9; 64]))
}

fn owner_of(user: Uuid) -> OwnerId {
    OwnerId::new(user.to_string())
}

/// A ledger holding `user`'s directory offers, and the history read back from it.
struct Fixture {
    ledger: InMemoryLedger,
    signer: LocalAttestationKey,
}

impl Fixture {
    fn new() -> Self {
        Self {
            ledger: InMemoryLedger::new(),
            signer: signer(),
        }
    }

    async fn publish(&self, ik: &HybridSigningKey, version: u64, devices: Vec<DeviceEntry>) {
        self.publish_for(USER, ik, version, devices, at(0)).await;
    }

    async fn publish_for(
        &self,
        user: Uuid,
        ik: &HybridSigningKey,
        version: u64,
        devices: Vec<DeviceEntry>,
        when: Timestamp,
    ) {
        let bytes = document(ik, user, version, devices);
        publish(&self.ledger, &self.signer, ik, user, version, bytes, when).await;
    }

    /// Append something that is not a directory, to give a manifest a position.
    async fn other(&self, user: Uuid) -> u64 {
        self.ledger
            .append(
                &owner_of(user),
                LedgerEvent::Refused {
                    upload_id: "u".to_owned(),
                    asset_id: "a".to_owned(),
                    reason: "fixture".to_owned(),
                },
                at(0),
                &self.signer,
            )
            .await
            .expect("an append")
            .core
            .seq
    }

    async fn history(&self, users: &[Uuid]) -> DirectoryHistory {
        let mut history = DirectoryHistory::new();
        for user in users {
            let owner = owner_of(*user);
            let scan = self.ledger.scan(&owner).await.expect("a scan");
            history
                .record(&self.ledger, &owner, &scan.entries)
                .await
                .expect("the history reads");
        }
        history
    }
}

fn position(owner: &OwnerId, seq: u64) -> Position<'_> {
    Position {
        owner,
        seq,
        recorded_at: Some(at(0)),
    }
}

#[test]
fn a_copy_round_trips_and_refuses_what_it_cannot_read() {
    let copy = DirectoryCopy {
        identity_key: vec![1, 2, 3],
        document: vec![4, 5],
    };
    assert_eq!(DirectoryCopy::decode(&copy.encode()), Ok(copy));
    assert_eq!(
        DirectoryCopy::decode(b"not cbor"),
        Err(DirectoryFault::Undecodable)
    );

    let newer = capsule_core::cbor::to_canonical_vec(&Wire {
        version: DIRECTORY_COPY_VERSION + 1,
        identity_key: Bytes(Vec::new()),
        document: Bytes(Vec::new()),
    })
    .expect("encodes");
    assert_eq!(
        DirectoryCopy::decode(&newer),
        Err(DirectoryFault::NewerVersion {
            found: DIRECTORY_COPY_VERSION + 1
        })
    );
}

#[tokio::test]
async fn a_manifest_signed_by_a_device_added_after_it_in_the_ledger_is_refused() {
    let fixture = Fixture::new();
    let ik = HybridSigningKey::generate();
    let first = HybridSigningKey::generate();
    let later = HybridSigningKey::generate();
    let (a, b) = (Uuid::from_u128(0xA), Uuid::from_u128(0xB));
    fixture
        .publish(&ik, 1, vec![device(a, &first, ADDED, None)])
        .await;
    let before = fixture.other(USER).await;
    fixture
        .publish(
            &ik,
            2,
            vec![
                device(a, &first, ADDED, None),
                device(b, &later, ADDED, None),
            ],
        )
        .await;
    let after = fixture.other(USER).await;

    let history = fixture.history(&[USER]).await;
    let owner = owner_of(USER);
    let signed = manifest(USER, b, &later, SIGNED);
    assert_eq!(
        history.verify(&signed, position(&owner, before)),
        Err(Refusal::Device(RejectReason::UnknownDevice)),
        "the device was not in the directory in force when the manifest was accepted"
    );
    assert_eq!(history.verify(&signed, position(&owner, after)), Ok(()));
}

#[tokio::test]
async fn a_manifest_signed_by_a_device_the_directory_later_revoked_still_verifies() {
    let fixture = Fixture::new();
    let ik = HybridSigningKey::generate();
    let key = HybridSigningKey::generate();
    let a = Uuid::from_u128(0xA);
    fixture
        .publish(&ik, 1, vec![device(a, &key, ADDED, None)])
        .await;
    let before = fixture.other(USER).await;
    fixture
        .publish(
            &ik,
            2,
            vec![device(a, &key, ADDED, Some("2026-03-01T00:00:00Z"))],
        )
        .await;
    let after = fixture.other(USER).await;

    let history = fixture.history(&[USER]).await;
    let owner = owner_of(USER);
    let signed = manifest(USER, a, &key, SIGNED);
    assert_eq!(history.verify(&signed, position(&owner, before)), Ok(()));
    assert_eq!(
        history.verify(&signed, position(&owner, after)),
        Err(Refusal::Revoked),
        "a revoked device signs nothing the server accepts after the revocation"
    );
}

#[tokio::test]
async fn a_forged_signature_and_a_device_added_after_its_timestamp_are_refused() {
    let fixture = Fixture::new();
    let ik = HybridSigningKey::generate();
    let key = HybridSigningKey::generate();
    let a = Uuid::from_u128(0xA);
    fixture
        .publish(&ik, 1, vec![device(a, &key, ADDED, None)])
        .await;
    let seq = fixture.other(USER).await;
    let history = fixture.history(&[USER]).await;
    let owner = owner_of(USER);

    let forged = manifest(USER, a, &HybridSigningKey::generate(), SIGNED);
    assert_eq!(
        history.verify(&forged, position(&owner, seq)),
        Err(Refusal::Device(RejectReason::BadDeviceSig))
    );
    let backdated = manifest(USER, a, &key, "2025-12-01T00:00:00Z");
    assert_eq!(
        history.verify(&backdated, position(&owner, seq)),
        Err(Refusal::Device(RejectReason::DeviceAddedAfter))
    );
}

#[tokio::test]
async fn no_directory_before_the_manifest_is_a_refusal() {
    let fixture = Fixture::new();
    let ik = HybridSigningKey::generate();
    let key = HybridSigningKey::generate();
    let a = Uuid::from_u128(0xA);
    let seq = fixture.other(USER).await;
    fixture
        .publish(&ik, 1, vec![device(a, &key, ADDED, None)])
        .await;

    let history = fixture.history(&[USER]).await;
    let owner = owner_of(USER);
    assert_eq!(
        history.verify(&manifest(USER, a, &key, SIGNED), position(&owner, seq)),
        Err(Refusal::NoDirectory)
    );
    assert_eq!(
        DirectoryHistory::new().verify(&manifest(USER, a, &key, SIGNED), position(&owner, seq)),
        Err(Refusal::NoDirectory)
    );
}

#[tokio::test]
async fn a_stale_offer_and_one_under_another_identity_key_are_not_in_force() {
    let fixture = Fixture::new();
    let ik = HybridSigningKey::generate();
    let key = HybridSigningKey::generate();
    let other = HybridSigningKey::generate();
    let (a, b) = (Uuid::from_u128(0xA), Uuid::from_u128(0xB));
    fixture
        .publish(&ik, 2, vec![device(a, &key, ADDED, None)])
        .await;
    // Stale: the store refused it, so device B never entered the directory.
    fixture
        .publish(&ik, 1, vec![device(b, &other, ADDED, None)])
        .await;
    // Another identity key: the store refused it too.
    fixture
        .publish(
            &HybridSigningKey::generate(),
            3,
            vec![device(b, &other, ADDED, None)],
        )
        .await;
    let seq = fixture.other(USER).await;

    let history = fixture.history(&[USER]).await;
    let owner = owner_of(USER);
    assert_eq!(
        history.verify(&manifest(USER, b, &other, SIGNED), position(&owner, seq)),
        Err(Refusal::Device(RejectReason::UnknownDevice))
    );
    assert_eq!(
        history.verify(&manifest(USER, a, &key, SIGNED), position(&owner, seq)),
        Ok(()),
        "the version the store accepted is still the one in force"
    );
    assert!(history.faults(&owner).is_empty());
}

#[tokio::test]
async fn a_version_with_no_usable_copy_vouches_for_nothing_and_is_reported() {
    let fixture = Fixture::new();
    let ik = HybridSigningKey::generate();
    let key = HybridSigningKey::generate();
    let a = Uuid::from_u128(0xA);
    let owner = owner_of(USER);
    fixture
        .publish(&ik, 1, vec![device(a, &key, ADDED, None)])
        .await;
    // Version 2's entry names a document whose copy was never kept.
    let missing = fixture
        .ledger
        .append(
            &owner,
            LedgerEvent::Directory {
                user_id: USER.to_string(),
                directory_version: 2,
                document_hash: Hash32::from_bytes([7; 32]),
            },
            at(0),
            &fixture.signer,
        )
        .await
        .expect("an append")
        .core
        .seq;
    let seq = fixture.other(USER).await;
    // Version 3's copy holds another document than the entry recorded.
    let recorded = document(&ik, USER, 3, vec![device(a, &key, ADDED, None)]);
    fixture
        .ledger
        .attach(
            &owner,
            hash_bytes(&recorded),
            DirectoryCopy {
                identity_key: ik.verifying_key().to_bytes(),
                document: document(&ik, USER, 3, Vec::new()),
            }
            .encode(),
        )
        .await
        .expect("attached");
    let swapped = fixture
        .ledger
        .append(
            &owner,
            LedgerEvent::Directory {
                user_id: USER.to_string(),
                directory_version: 3,
                document_hash: hash_bytes(&recorded),
            },
            at(0),
            &fixture.signer,
        )
        .await
        .expect("an append")
        .core
        .seq;

    let history = fixture.history(&[USER]).await;
    assert_eq!(
        history.faults(&owner),
        &[
            (missing, DirectoryFault::Missing),
            (swapped, DirectoryFault::HashMismatch)
        ]
    );
    assert_eq!(
        history.verify(&manifest(USER, a, &key, SIGNED), position(&owner, seq)),
        Err(Refusal::DirectoryUnavailable {
            directory_seq: missing
        })
    );
}

/// An oversized copy on disk is damage the history reports, not a failure that stops the walk.
#[tokio::test]
async fn an_oversized_copy_is_reported_not_read() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let ledger = crate::ledger::FilesystemLedger::open(root.path())
        .await
        .expect("the ledger opens");
    let signer = signer();
    let ik = HybridSigningKey::generate();
    let owner = owner_of(USER);
    let bytes = document(&ik, USER, 1, Vec::new());
    let document_hash = hash_bytes(&bytes);
    publish(&ledger, &signer, &ik, USER, 1, bytes, at(0)).await;
    let path = ledger
        .owner_dir(&owner)
        .join(crate::ledger::fs::ATTACHMENT_DIR)
        .join(document_hash.to_hex());
    std::fs::write(&path, vec![0; crate::ledger::MAX_ATTACHMENT_BYTES + 1]).expect("overwritten");

    let scan = ledger.scan(&owner).await.expect("a scan");
    let mut history = DirectoryHistory::new();
    history
        .record(&ledger, &owner, &scan.entries)
        .await
        .expect("the walk goes on");
    assert_eq!(history.faults(&owner), &[(1, DirectoryFault::TooLarge)]);
}

#[tokio::test]
async fn a_manifest_in_another_ledger_is_judged_by_the_servers_clock() {
    let fixture = Fixture::new();
    let ik = HybridSigningKey::generate();
    let key = HybridSigningKey::generate();
    let a = Uuid::from_u128(0xA);
    fixture
        .publish_for(USER, &ik, 1, vec![device(a, &key, ADDED, None)], at(10))
        .await;
    let history = fixture.history(&[USER]).await;
    let album_owner = OwnerId::new("album-owner");
    let signed = manifest(USER, a, &key, SIGNED);
    let accepted = |second| Position {
        owner: &album_owner,
        seq: 1,
        recorded_at: Some(at(second)),
    };
    assert_eq!(history.verify(&signed, accepted(10)), Ok(()));
    assert_eq!(
        history.verify(&signed, accepted(9)),
        Err(Refusal::NoDirectory),
        "a manifest accepted before the directory was published is not vouched for by it"
    );
}

#[test]
fn every_refusal_reads_as_a_sentence() {
    for refusal in [
        Refusal::NoDirectory,
        Refusal::DirectoryUnavailable { directory_seq: 4 },
        Refusal::Revoked,
        Refusal::Device(RejectReason::UnknownDevice),
        Refusal::Device(RejectReason::DeviceAddedAfter),
        Refusal::Device(RejectReason::BadTimestamp),
        Refusal::Device(RejectReason::BadDeviceSig),
        Refusal::Device(RejectReason::WrongEpoch),
    ] {
        assert!(!refusal.to_string().is_empty());
    }
}
