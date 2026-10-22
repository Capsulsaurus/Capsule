//! The rebuild's contract: drop the index, rebuild, and the rows come back; a ledger that cannot
//! prove its sequence moves the owner's epoch; damage stops only its own owner; a dry run writes
//! nothing; a row the index holds is compared, never overwritten.

use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::keys::{AmkVersion, HybridSigningKey};
use capsule_core::crypto::provenance::{ASSET_MANIFEST_VERSION, Action, KeyMode, ManifestCore};

use super::*;
use crate::attestation::{LocalAttestationKey, PublishedKey, ReceiptSigner as _};
use crate::blob::InMemoryBlobStore;
use crate::index::memory::InMemoryAssetIndex;
use crate::index::{BlobOutcome, BlobRecord, PendingAsset};
use crate::ledger::{InMemoryLedger, LedgerEvent};
use crate::quota::InMemoryQuota;
use crate::store::memory::ManualClock;

/// The album every fixture files under.
const ALBUM: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e70";

/// The instant every fixture write happens at, so a rebuilt row's times equal the original's.
fn at() -> Timestamp {
    Timestamp::from_second(1_767_225_600).expect("a valid instant")
}

/// A stable UUID asset id for a fixture name: a manifest's `file_id` is a UUID.
fn asset(name: &str) -> AssetId {
    let id = uuid::Uuid::from_u128(u128::from_be_bytes(
        hash_bytes(name.as_bytes()).0[..16]
            .try_into()
            .expect("sixteen bytes"),
    ));
    AssetId::new(id.to_string())
}

/// A really signed `create` manifest for `asset`, committing to `metadata`, in the provenance
/// record a client stores it in.
fn manifest(asset: &AssetId, metadata: &ContentAddress) -> Vec<u8> {
    let core = ManifestCore {
        version: ASSET_MANIFEST_VERSION.to_owned(),
        crypto_suite_id: 1,
        protocol_version: "2026-01-01".to_owned(),
        file_id: uuid::Uuid::parse_str(asset.as_str()).expect("a uuid"),
        album_id: uuid::Uuid::parse_str(ALBUM).expect("a uuid"),
        amk_version: AmkVersion(0),
        ciphertext_hash: hash_bytes(b"original"),
        plaintext_size: 1024,
        chunk_size: 65_536,
        nonce_prefix: [0; 7],
        key_mode: KeyMode::Derived,
        wrapped_file_key: None,
        metadata_blob_hash: Some(Hash32::from_hex(metadata.as_str()).expect("a digest")),
        created_by_user: uuid::Uuid::from_u128(7),
        created_by_device: uuid::Uuid::from_u128(8),
        client_version: "capsule-cli/0.1.0".to_owned(),
        timestamp: "2026-01-01T00:00:00Z".to_owned(),
        action: Action::Create,
        prior_provenance_hash: None,
        upgraded_from: None,
        unknown: Default::default(),
        retention_until: None,
    };
    let device = HybridSigningKey::from_seed64(&[1; 64]);
    let write_tier = HybridSigningKey::from_seed64(&[2; 64]);
    let signed = core.sign(&device, &write_tier).expect("a manifest signs");
    // What a client stores as the provenance blob: the chain head's record, not the bare
    // manifest (`UploadBundle::provenance_blob`).
    let record = capsule_core::crypto::provenance::ProvenanceRecord::new(
        uuid::Uuid::parse_str(asset.as_str()).expect("a uuid"),
        signed,
        None,
    );
    capsule_core::cbor::to_canonical_vec(&record).expect("a record encodes")
}

/// The stores a server writes to, and a rebuild reads from.
struct Server {
    index: Arc<InMemoryAssetIndex>,
    blobs: Arc<InMemoryBlobStore>,
    ledger: Arc<InMemoryLedger>,
    quotas: Arc<InMemoryQuota>,
    signer: LocalAttestationKey,
    clock: Arc<ManualClock>,
}

impl Server {
    fn new() -> Self {
        Self {
            index: Arc::new(InMemoryAssetIndex::new()),
            blobs: Arc::new(InMemoryBlobStore::new()),
            ledger: Arc::new(InMemoryLedger::new()),
            quotas: Arc::new(InMemoryQuota::new()),
            signer: LocalAttestationKey::new(
                "server.test",
                HybridSigningKey::from_seed64(&[9; 64]),
            ),
            clock: Arc::new(ManualClock::new(at())),
        }
    }

    fn keys(&self) -> LedgerKeys {
        LedgerKeys::new(vec![PublishedKey {
            key_id: self.signer.key_id(),
            public: self.signer.verifying_key(),
            active_from: Timestamp::UNIX_EPOCH,
            active_to: None,
        }])
    }

    /// A rebuild over these stores, into `index`.
    fn rebuild_into(&self, index: Arc<InMemoryAssetIndex>) -> RebuildContext {
        RebuildContext::new(
            index,
            self.blobs.clone(),
            self.ledger.clone(),
            self.quotas.clone(),
            self.keys(),
            self.clock.clone(),
        )
    }

    /// Append as a deployment does: through the anchor, so the index records each head.
    async fn append(&self, owner: &OwnerId, event: LedgerEvent) {
        crate::ledger::AnchoredLedger::new(self.ledger.clone(), self.index.clone())
            .append(owner, event, at(), &self.signer)
            .await
            .expect("an append lands");
    }

    async fn store(&self, bytes: &[u8]) -> ContentAddress {
        let address = ContentAddress::parse(&hash_bytes(bytes).to_hex()).expect("an address");
        self.blobs.put(&address, bytes).await.expect("stored");
        address
    }

    /// Finalize one blob the way `upload::finalize` orders it: custody, the index write, then
    /// the indexed entry — or, with `crash`, stop after the index write.
    async fn finalize(
        &self,
        owner: &OwnerId,
        asset: &AssetId,
        role: BlobRole,
        bytes: &[u8],
        crash: bool,
    ) -> ContentAddress {
        let address = self.store(bytes).await;
        let manifest_sha256 = (role == BlobRole::Provenance).then(|| hash_bytes(bytes));
        let upload = format!("{asset}-{}", role.as_str());
        self.append(
            owner,
            LedgerEvent::Custody {
                upload_id: upload.clone(),
                asset_id: asset.as_str().to_owned(),
                blob_role: role.as_str().to_owned(),
                content_hash: hash_bytes(bytes),
                size: bytes.len() as u64,
                manifest_sha256,
                upload_user_id: owner.as_str().to_owned(),
                uploader_device: None,
                received_at: at().to_string(),
            },
        )
        .await;
        let outcome = self
            .index
            .record_blob(
                asset,
                BlobRecord {
                    role,
                    address: address.clone(),
                    size: bytes.len() as u64,
                    manifest_sha256,
                    finalized_at: at(),
                },
            )
            .await
            .expect("the index records");
        let BlobOutcome::Recorded { minted, .. } = outcome else {
            panic!("recording answered {outcome:?}");
        };
        if !crash {
            self.append(
                owner,
                LedgerEvent::Indexed {
                    upload_id: upload,
                    asset_id: asset.as_str().to_owned(),
                    sync_seq: minted,
                    receipt: None,
                },
            )
            .await;
        }
        address
    }

    /// Reserve and publish `name` under `owner`; with `crash`, the last indexed entry is lost.
    async fn publish(&self, owner: &OwnerId, name: &str, crash: bool) -> AssetId {
        let id = asset(name);
        self.index
            .reserve(PendingAsset {
                asset_id: id.clone(),
                owner_id: owner.clone(),
                album_id: AlbumId::new(ALBUM),
                protocol_version: "2026-01-01".to_owned(),
                crypto_suite_id: 1,
                created_at: at(),
            })
            .await
            .expect("the index reserves");
        let metadata_bytes = format!("{name}-metadata").into_bytes();
        let metadata =
            ContentAddress::parse(&hash_bytes(&metadata_bytes).to_hex()).expect("an address");
        self.finalize(
            owner,
            &id,
            BlobRole::Provenance,
            &manifest(&id, &metadata),
            false,
        )
        .await;
        self.finalize(owner, &id, BlobRole::Metadata, &metadata_bytes, crash)
            .await;
        id
    }

    /// Delete `asset` the way `POST /albums/{id}/ops` orders it (#753): a lifecycle entry, the
    /// index's apply, then the entry carrying the sequence number — or, with `crash`, no second
    /// entry. `prior` overrides the predecessor the manifest names, to write one that does not
    /// chain; the index is then not asked at all, as it would refuse.
    async fn delete(&self, owner: &OwnerId, asset: &AssetId, crash: bool, prior: Option<Hash32>) {
        let row = self.index.read(asset).await.expect("read").expect("a row");
        let chains = prior.is_none();
        let prior = prior.or(row.chain_head);
        let bytes = delete_record(asset, prior.expect("a published row has a head"));
        let address = self.store(&bytes).await;
        let manifest_hash = hash_bytes(&bytes);
        let event = |sync_seq| LedgerEvent::Lifecycle {
            asset_id: asset.as_str().to_owned(),
            action: OpAction::Delete.as_str().to_owned(),
            manifest_hash: Some(manifest_hash),
            sync_seq,
        };
        self.append(owner, event(None)).await;
        if !chains {
            return;
        }
        let outcome = self
            .index
            .apply_op(crate::index::LifecycleOp {
                asset_id: asset.clone(),
                owner_id: owner.clone(),
                album_id: row.album_id.clone(),
                action: OpAction::Delete,
                manifest_hash,
                prior_provenance_hash: prior,
                amk_version: 0,
                provenance: address,
                metadata: None,
                original: None,
                retention_until: Some(retention()),
                at: at(),
            })
            .await
            .expect("the index applies");
        let crate::index::OpOutcome::Applied { sync_seq, .. } = outcome else {
            panic!("the delete answered {outcome:?}");
        };
        if !crash {
            self.append(owner, event(Some(sync_seq))).await;
        }
    }
}

/// The retention floor every fixture delete signs.
fn retention() -> Timestamp {
    Timestamp::from_second(1_769_904_000).expect("a valid instant")
}

/// A signed `delete` for `asset`, chaining onto `prior`, in its provenance record.
fn delete_record(asset: &AssetId, prior: Hash32) -> Vec<u8> {
    let metadata = ContentAddress::parse(&hash_bytes(b"unused").to_hex()).expect("an address");
    let created = manifest(asset, &metadata);
    let record: capsule_core::crypto::provenance::ProvenanceRecord =
        capsule_core::cbor::from_slice(&created).expect("the create record decodes");
    let mut core = record.manifest.core;
    core.action = Action::Delete;
    core.metadata_blob_hash = None;
    core.prior_provenance_hash = Some(prior);
    core.retention_until = Some(retention().to_string());
    let device = HybridSigningKey::from_seed64(&[1; 64]);
    let write_tier = HybridSigningKey::from_seed64(&[2; 64]);
    let signed = core.sign(&device, &write_tier).expect("a manifest signs");
    let record = capsule_core::crypto::provenance::ProvenanceRecord::new(
        uuid::Uuid::parse_str(asset.as_str()).expect("a uuid"),
        signed,
        Some(prior),
    );
    capsule_core::cbor::to_canonical_vec(&record).expect("a record encodes")
}

async fn rows(index: &InMemoryAssetIndex) -> Vec<AssetRow> {
    index.rows(None, 1000).await.expect("rows")
}

fn owner(name: &str) -> OwnerId {
    OwnerId::new(name)
}

#[tokio::test]
async fn dropping_the_index_and_rebuilding_matches_the_original_and_a_rerun_changes_nothing() {
    let server = Server::new();
    let alice = owner("alice");
    server.publish(&alice, "one", false).await;
    server.publish(&alice, "two", false).await;
    let original = rows(&server.index).await;

    let empty = Arc::new(InMemoryAssetIndex::new());
    let context = server.rebuild_into(empty.clone());
    let report = rebuild(&context, Mode::Apply).await.expect("a pass");

    assert_eq!(
        rows(&empty).await,
        original,
        "the rebuilt index is not the original"
    );
    assert_eq!(
        empty.head_seq(&alice).await.expect("head"),
        server.index.head_seq(&alice).await.expect("head")
    );
    let alice_report = &report.owners[0];
    assert_eq!(alice_report.restored.len(), 2);
    assert!(
        !alice_report.epoch_bump_needed,
        "a complete ledger proves its sequence"
    );
    assert_eq!(empty.sync_epoch(&alice).await.expect("epoch"), 0);
    assert!(!report.has_faults());
    assert!(report.orphans.is_empty());
    // Quota is recomputed for the restored blobs.
    assert!(
        server
            .quotas
            .usage(&UserId::new("alice"))
            .await
            .expect("usage")
            .used
            > 0
    );

    let again = rebuild(&context, Mode::Apply).await.expect("a second pass");
    assert!(
        again.owners[0].restored.is_empty(),
        "a rerun restored something"
    );
    assert_eq!(again.owners[0].agreed.len(), 2);
    assert_eq!(rows(&empty).await, original);
}

#[tokio::test]
async fn a_dry_run_reports_and_writes_nothing() {
    let server = Server::new();
    server.publish(&owner("alice"), "dry", false).await;

    let empty = Arc::new(InMemoryAssetIndex::new());
    let report = rebuild(&server.rebuild_into(empty.clone()), Mode::DryRun)
        .await
        .expect("a pass");
    assert!(!report.applied);
    assert_eq!(report.owners[0].restored.len(), 1);
    assert!(rows(&empty).await.is_empty(), "a dry run wrote a row");
    assert_eq!(
        server
            .quotas
            .usage(&UserId::new("alice"))
            .await
            .expect("usage")
            .used,
        0,
        "a dry run charged quota"
    );
}

#[tokio::test]
async fn a_lost_ledger_tail_bumps_the_epoch_and_numbers_the_row_above_the_head() {
    let server = Server::new();
    let alice = owner("alice");
    server.publish(&alice, "proven", false).await;
    // The process stopped after the index write and before the indexed entry.
    let lost = server.publish(&alice, "unproven", true).await;

    let empty = Arc::new(InMemoryAssetIndex::new());
    let report = rebuild(&server.rebuild_into(empty.clone()), Mode::Apply)
        .await
        .expect("a pass");

    let alice_report = &report.owners[0];
    assert_eq!(alice_report.unprovable, vec![lost.clone()]);
    assert!(alice_report.epoch_bump_needed);
    assert_eq!(alice_report.epoch, Some(1));
    assert_eq!(empty.sync_epoch(&alice).await.expect("epoch"), 1);
    let restored = empty.read(&lost).await.expect("read").expect("restored");
    assert_eq!(
        restored.sync_seq,
        Some(2),
        "an unprovable row is numbered above every number the owner has used"
    );
}

/// Cut `owner`'s ledger after its first `keep` entries, exactly at an entry boundary: what is
/// left is a valid, shorter chain.
fn cut_at_boundary(server: &Server, owner: &OwnerId, keep: usize) -> crate::ledger::LedgerHead {
    let mut bytes = server.ledger.raw(owner);
    let entries = crate::ledger::decode_frames(&bytes).entries;
    let kept: usize = entries[..keep]
        .iter()
        .map(|entry| crate::ledger::encode_frame(entry).len())
        .sum();
    bytes.truncate(kept);
    server.ledger.replace_raw(owner, bytes);
    let last = &entries[keep - 1];
    crate::ledger::LedgerHead {
        seq: last.core.seq,
        hash: last.chain_hash(),
    }
}

/// The acceptance case of #757: on a deployment whose index survived, a ledger cut at an entry
/// boundary is reported as `BehindAnchor`, moves the owner's epoch, and is re-anchored so the
/// loss is reported once.
#[tokio::test]
async fn a_boundary_cut_is_behind_the_anchor_and_is_reanchored_under_apply() {
    let server = Server::new();
    let alice = owner("alice");
    server.publish(&alice, "kept", false).await;
    server.publish(&alice, "cut", false).await;
    let anchored = server
        .index
        .ledger_head(&alice)
        .await
        .expect("read")
        .expect("the appends anchored the head");
    assert_eq!(anchored.seq, 8, "two publishes are eight entries");
    let left = cut_at_boundary(&server, &alice, 4);

    // A dry run reports the cut and moves nothing.
    let context = server.rebuild_into(server.index.clone());
    let dry = rebuild(&context, Mode::DryRun).await.expect("a pass");
    let expected = Some(ChainFault::BehindAnchor {
        found: 4,
        anchored: 8,
    });
    assert_eq!(dry.owners[0].chain_fault, expected);
    assert!(dry.has_faults());
    assert_eq!(
        server.index.ledger_head(&alice).await.expect("read"),
        Some(anchored),
        "a dry run moved the anchor"
    );

    let applied = rebuild(&context, Mode::Apply).await.expect("a pass");
    let report = &applied.owners[0];
    assert_eq!(report.chain_fault, expected);
    assert_eq!(report.verified, 4);
    assert!(report.epoch_bump_needed);
    assert_eq!(report.epoch, Some(1));
    assert_eq!(
        server.index.ledger_head(&alice).await.expect("read"),
        Some(left),
        "the acknowledged cut re-anchors at the head that is left"
    );

    // Reported once: the next pass verifies, and the next append extends the new anchor.
    let again = rebuild(&context, Mode::Apply).await.expect("a pass");
    assert_eq!(again.owners[0].chain_fault, None);
    server.publish(&alice, "after", false).await;
    assert_eq!(
        server
            .index
            .ledger_head(&alice)
            .await
            .expect("read")
            .map(|head| head.seq),
        Some(8)
    );
    let after = rebuild(&context, Mode::DryRun).await.expect("a pass");
    assert_eq!(after.owners[0].chain_fault, None);
}

/// Without the surviving index's anchor the same cut reads as a shorter ledger; a rebuild into an
/// empty schema anchors the chain it verified.
#[tokio::test]
async fn a_rebuild_into_an_empty_index_anchors_the_chain_it_verified() {
    let server = Server::new();
    let alice = owner("alice");
    server.publish(&alice, "kept", false).await;
    server.publish(&alice, "cut", false).await;
    let left = cut_at_boundary(&server, &alice, 4);

    let empty = Arc::new(InMemoryAssetIndex::new());
    let context = server.rebuild_into(empty.clone());
    let dry = rebuild(&context, Mode::DryRun).await.expect("a pass");
    assert_eq!(
        dry.owners[0].chain_fault, None,
        "no anchor: a boundary cut is a shorter ledger"
    );
    assert_eq!(empty.ledger_head(&alice).await.expect("read"), None);

    rebuild(&context, Mode::Apply).await.expect("a pass");
    assert_eq!(empty.ledger_head(&alice).await.expect("read"), Some(left));
}

/// A ledger that lost every entry is not listed by the ledger at all; its anchor is what says it
/// existed, so the rebuild still reports the cut at 0, clears the anchor under `--apply`, and the
/// server writes to the owner again.
#[tokio::test]
async fn a_ledger_that_lost_every_entry_is_reported_from_its_anchor_and_reanchored() {
    let server = Server::new();
    let alice = owner("alice");
    server.publish(&alice, "gone", false).await;
    server.ledger.replace_raw(&alice, Vec::new());
    assert!(
        server.ledger.owners().await.expect("owners").is_empty(),
        "the ledger lists an owner with no entries"
    );

    // Until a rebuild re-anchors, the server refuses to write over the loss.
    let refused = crate::ledger::AnchoredLedger::new(server.ledger.clone(), server.index.clone())
        .append(
            &alice,
            LedgerEvent::Directory {
                user_id: "alice".to_owned(),
                directory_version: 1,
                document_hash: hash_bytes(b"directory"),
            },
            at(),
            &server.signer,
        )
        .await;
    assert!(
        matches!(
            refused,
            Err(LedgerError::Damaged {
                fault: ChainFault::BehindAnchor {
                    found: 0,
                    anchored: 4
                },
                ..
            })
        ),
        "{refused:?}"
    );

    let context = server.rebuild_into(server.index.clone());
    let dry = rebuild(&context, Mode::DryRun).await.expect("a pass");
    assert_eq!(dry.owners.len(), 1, "the anchored owner was not walked");
    assert_eq!(dry.owners[0].owner, alice);
    assert_eq!(
        dry.owners[0].chain_fault,
        Some(ChainFault::BehindAnchor {
            found: 0,
            anchored: 4
        })
    );
    assert_eq!(dry.owners[0].reanchored, None, "a dry run re-anchored");

    let applied = rebuild(&context, Mode::Apply).await.expect("a pass");
    assert_eq!(applied.owners[0].reanchored, Some(0));
    assert_eq!(applied.owners[0].epoch, Some(1));
    assert_eq!(server.index.ledger_head(&alice).await.expect("read"), None);

    // The way out works: the owner's next write lands, from position 1.
    server.publish(&alice, "after", false).await;
    let again = rebuild(&context, Mode::DryRun).await.expect("a pass");
    assert_eq!(again.owners[0].chain_fault, None);
}

/// A rewrite the anchor caught keeps its evidence: `--apply` does not move the anchor down to
/// the entry before it.
#[tokio::test]
async fn an_anchor_mismatch_is_not_reanchored() {
    let server = Server::new();
    let alice = owner("alice");
    server.publish(&alice, "one", false).await;
    let anchored = server
        .index
        .ledger_head(&alice)
        .await
        .expect("read")
        .expect("anchored");
    let rival = crate::ledger::LedgerHead {
        seq: anchored.seq,
        hash: hash_bytes(b"another entry"),
    };
    server
        .index
        .reset_ledger_head(&alice, Some(rival))
        .await
        .expect("reset");

    let context = server.rebuild_into(server.index.clone());
    let report = rebuild(&context, Mode::Apply).await.expect("a pass");
    assert_eq!(
        report.owners[0].chain_fault,
        Some(ChainFault::AnchorMismatch { seq: anchored.seq })
    );
    assert_eq!(
        server.index.ledger_head(&alice).await.expect("read"),
        Some(rival),
        "a caught rewrite was re-anchored away"
    );
    assert_eq!(report.owners[0].reanchored, None);

    // The operator's way out: having read the report, accept the ledger as it stands. The pass
    // still reports the mismatch; the anchor moves to the last entry before it, and the next pass
    // verifies the ledger as it now is.
    let accepting = server
        .rebuild_into(server.index.clone())
        .accepting_rewrites();
    let dry = rebuild(&accepting, Mode::DryRun).await.expect("a pass");
    assert_eq!(dry.owners[0].reanchored, None, "a dry run re-anchored");
    let accepted = rebuild(&accepting, Mode::Apply).await.expect("a pass");
    assert_eq!(
        accepted.owners[0].chain_fault,
        Some(ChainFault::AnchorMismatch { seq: anchored.seq })
    );
    assert_eq!(accepted.owners[0].reanchored, Some(anchored.seq - 1));
    assert_eq!(
        server
            .index
            .ledger_head(&alice)
            .await
            .expect("read")
            .map(|head| head.seq),
        Some(anchored.seq - 1)
    );
    let after = rebuild(&context, Mode::Apply).await.expect("a pass");
    assert_eq!(after.owners[0].chain_fault, None);
    assert_eq!(
        server.index.ledger_head(&alice).await.expect("read"),
        Some(anchored),
        "the next pass raises the anchor to the ledger as it stands"
    );
}

#[tokio::test]
async fn a_tampered_ledger_stops_only_its_own_owner() {
    let server = Server::new();
    let alice = owner("alice");
    let bob = owner("bob");
    server.publish(&alice, "alice-one", false).await;
    let bobs_first = server.publish(&bob, "bob-one", false).await;
    let bobs_second = server.publish(&bob, "bob-two", false).await;

    // Re-sign bob's ledger from its fifth entry on under a key this server never published.
    let forger = LocalAttestationKey::new("server.test", HybridSigningKey::from_seed64(&[3; 64]));
    let scan = server.ledger.scan(&bob).await.expect("a scan");
    let forged = InMemoryLedger::new();
    for (at_entry, entry) in scan.entries.iter().enumerate() {
        let signer: &dyn crate::attestation::ReceiptSigner = if at_entry < 4 {
            &server.signer
        } else {
            &forger
        };
        forged
            .append(&bob, entry.core.event.clone(), at(), signer)
            .await
            .expect("an append lands");
    }
    server.ledger.replace_raw(&bob, forged.raw(&bob));

    let empty = Arc::new(InMemoryAssetIndex::new());
    let report = rebuild(&server.rebuild_into(empty.clone()), Mode::Apply)
        .await
        .expect("a pass");

    let alice_report = report
        .owners
        .iter()
        .find(|owner| owner.owner == alice)
        .expect("alice is reported");
    assert!(alice_report.chain_fault.is_none());
    assert_eq!(alice_report.restored.len(), 1);

    let bob_report = report
        .owners
        .iter()
        .find(|owner| owner.owner == bob)
        .expect("bob is reported");
    assert_eq!(bob_report.verified, 4);
    assert_eq!(
        bob_report.chain_fault,
        Some(ChainFault::UnknownKey { seq: 5 })
    );
    assert_eq!(bob_report.restored, vec![bobs_first]);
    assert!(
        empty.read(&bobs_second).await.expect("read").is_none(),
        "a row past the break was replayed"
    );
    assert_eq!(bob_report.epoch, Some(1), "a break moves the owner's epoch");
    assert_eq!(empty.sync_epoch(&alice).await.expect("epoch"), 0);
}

#[tokio::test]
async fn a_row_the_index_holds_is_compared_and_never_overwritten() {
    let server = Server::new();
    let alice = owner("alice");
    let id = server.publish(&alice, "held", false).await;

    // A restored backup whose row for the asset names another album.
    let backup = Arc::new(InMemoryAssetIndex::new());
    let mut rival = server.index.read(&id).await.expect("read").expect("a row");
    rival.album_id = AlbumId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5eff");
    backup.restore(rival.clone()).await.expect("seeded");

    let report = rebuild(&server.rebuild_into(backup.clone()), Mode::Apply)
        .await
        .expect("a pass");
    assert_eq!(
        report.owners[0].differs,
        vec![RowDifference {
            asset: id.clone(),
            facts: vec!["album_id"],
        }]
    );
    assert!(report.has_faults());
    assert_eq!(backup.read(&id).await.expect("read"), Some(rival));
}

#[tokio::test]
async fn a_malformed_manifest_is_quarantined_with_its_reason_and_its_entry_is_not_replayed() {
    let server = Server::new();
    let alice = owner("alice");
    let id = asset("garbled");
    server
        .index
        .reserve(PendingAsset {
            asset_id: id.clone(),
            owner_id: alice.clone(),
            album_id: AlbumId::new(ALBUM),
            protocol_version: "2026-01-01".to_owned(),
            crypto_suite_id: 1,
            created_at: at(),
        })
        .await
        .expect("the index reserves");
    let garbage = server
        .finalize(&alice, &id, BlobRole::Provenance, b"not a manifest", false)
        .await;

    let empty = Arc::new(InMemoryAssetIndex::new());
    let report = rebuild(&server.rebuild_into(empty.clone()), Mode::Apply)
        .await
        .expect("a pass");
    assert_eq!(
        report.owners[0].unusable_manifests,
        vec![(id.clone(), garbage.clone(), ManifestFault::Undecodable)]
    );
    assert_eq!(report.owners[0].incomplete, vec![id]);
    let held = server.blobs.quarantined().await.expect("the hold lists");
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].address, garbage);
    assert_eq!(held[0].reason.code, error_codes::UPLOAD_ENVELOPE_REJECTED);
}

/// Reserve `name` under `owner` and finalize `provenance` as its provenance blob.
async fn reserve_with_provenance(
    server: &Server,
    owner: &OwnerId,
    name: &str,
    provenance: &[u8],
) -> (AssetId, ContentAddress) {
    let id = asset(name);
    server
        .index
        .reserve(PendingAsset {
            asset_id: id.clone(),
            owner_id: owner.clone(),
            album_id: AlbumId::new(ALBUM),
            protocol_version: "2026-01-01".to_owned(),
            crypto_suite_id: 1,
            created_at: at(),
        })
        .await
        .expect("the index reserves");
    let address = server
        .finalize(owner, &id, BlobRole::Provenance, provenance, false)
        .await;
    (id, address)
}

#[tokio::test]
async fn a_record_from_a_newer_client_is_reported_and_never_quarantined() {
    let server = Server::new();
    let alice = owner("alice");
    let id = asset("from-the-future");
    let metadata = ContentAddress::parse(&hash_bytes(b"m").to_hex()).expect("an address");
    let mut record: capsule_core::crypto::provenance::ProvenanceRecord =
        capsule_core::cbor::from_slice(&manifest(&id, &metadata)).expect("a record decodes");
    record.version = capsule_core::crypto::provenance::PROVENANCE_RECORD_VERSION + 1;
    let newer = capsule_core::cbor::to_canonical_vec(&record).expect("a record encodes");
    let (_, address) = reserve_with_provenance(&server, &alice, "from-the-future", &newer).await;

    let report = rebuild(
        &server.rebuild_into(Arc::new(InMemoryAssetIndex::new())),
        Mode::Apply,
    )
    .await
    .expect("a pass");
    assert!(matches!(
        report.owners[0].unusable_manifests.as_slice(),
        [(_, held, ManifestFault::NewerVersion { .. })] if *held == address
    ));
    assert!(
        server
            .blobs
            .quarantined()
            .await
            .expect("the hold lists")
            .is_empty(),
        "a newer client's record was moved to quarantine"
    );
    assert!(
        server.blobs.stat(&address).await.expect("stat").is_some(),
        "a newer client's record left the store"
    );
}

#[tokio::test]
async fn a_malformed_manifest_a_row_still_references_is_left_in_place() {
    let server = Server::new();
    let alice = owner("alice");
    let (id, garbage) =
        reserve_with_provenance(&server, &alice, "still-referenced", b"not a manifest").await;

    // A restored backup whose row still points at the bytes.
    let backup = Arc::new(InMemoryAssetIndex::new());
    let mut row = server.index.read(&id).await.expect("read").expect("a row");
    row.state = AssetState::Visible;
    row.sync_seq = Some(1);
    row.first_seq = Some(1);
    backup.restore(row).await.expect("seeded");
    assert_eq!(backup.reference_count(&garbage).await.expect("count"), 1);

    let report = rebuild(&server.rebuild_into(backup), Mode::Apply)
        .await
        .expect("a pass");
    assert_eq!(
        report.owners[0].unusable_manifests,
        vec![(id, garbage.clone(), ManifestFault::Undecodable)]
    );
    assert!(
        server
            .blobs
            .quarantined()
            .await
            .expect("the hold lists")
            .is_empty(),
        "a referenced blob was moved to quarantine"
    );
    assert!(server.blobs.stat(&garbage).await.expect("stat").is_some());
}

#[tokio::test]
async fn an_unnumbered_row_is_numbered_above_every_proven_one_whatever_its_ledger_position() {
    // The unprovable asset comes first in the ledger and the proven one after it: the restore
    // still numbers the unprovable row above the proven number, through the allocator.
    let server = Server::new();
    let alice = owner("alice");
    let first = server.publish(&alice, "first-but-unproven", true).await;
    let second = server.publish(&alice, "second-and-proven", false).await;

    let empty = Arc::new(InMemoryAssetIndex::new());
    rebuild(&server.rebuild_into(empty.clone()), Mode::Apply)
        .await
        .expect("a pass");
    let proven = empty.read(&second).await.expect("read").expect("a row");
    let unproven = empty.read(&first).await.expect("read").expect("a row");
    assert_eq!(proven.sync_seq, Some(2));
    assert_eq!(unproven.sync_seq, Some(3));
    assert_eq!(empty.head_seq(&alice).await.expect("head"), 3);
}

#[tokio::test]
async fn an_entry_naming_a_missing_blob_is_dangling_and_an_unnamed_blob_is_an_orphan() {
    let server = Server::new();
    let alice = owner("alice");
    let id = server.publish(&alice, "gone", false).await;
    let metadata = server
        .index
        .read(&id)
        .await
        .expect("read")
        .expect("a row")
        .address_for(BlobRole::Metadata)
        .cloned()
        .expect("a metadata blob");
    server.blobs.remove(&metadata).await.expect("removed");
    let stray = server.store(b"nobody names me").await;

    let empty = Arc::new(InMemoryAssetIndex::new());
    let report = rebuild(&server.rebuild_into(empty), Mode::DryRun)
        .await
        .expect("a pass");
    assert_eq!(report.owners[0].dangling, vec![(id.clone(), metadata)]);
    assert_eq!(report.owners[0].incomplete, vec![id]);
    assert_eq!(report.orphans, vec![stray]);
}

#[tokio::test]
async fn without_keys_a_rebuild_verifies_links_and_says_so() {
    let server = Server::new();
    server.publish(&owner("alice"), "linked", false).await;
    let context = RebuildContext::new(
        Arc::new(InMemoryAssetIndex::new()),
        server.blobs.clone(),
        server.ledger.clone(),
        server.quotas.clone(),
        LedgerKeys::default(),
        server.clock.clone(),
    );
    let report = rebuild(&context, Mode::DryRun).await.expect("a pass");
    assert!(report.links_only);
    assert_eq!(report.owners[0].restored.len(), 1);
}

#[tokio::test]
async fn a_ledgered_delete_is_replayed_into_the_rebuilt_row() {
    let server = Server::new();
    let alice = owner("alice");
    let id = server.publish(&alice, "deleted", false).await;
    server.delete(&alice, &id, false, None).await;
    let original = rows(&server.index).await;
    assert_eq!(original[0].state, AssetState::Tombstoned);

    let empty = Arc::new(InMemoryAssetIndex::new());
    let report = rebuild(&server.rebuild_into(empty.clone()), Mode::Apply)
        .await
        .expect("a pass");
    assert!(!report.owners[0].epoch_bump_needed, "{report:?}");
    assert_eq!(
        rows(&empty).await,
        original,
        "the tombstone, its retention floor, the re-pointed head and the superseded create \
         all come back"
    );
}

#[tokio::test]
async fn a_ledgered_purge_drops_the_tombstones_references_and_keeps_the_row() {
    // The entry #767 specifies: a `purge` lifecycle entry with no manifest, before the index's
    // purge commits. The purged blobs are then collected, so without the entry a rebuild would
    // report every one of them as dangling.
    let server = Server::new();
    let alice = owner("alice");
    let id = server.publish(&alice, "purged", false).await;
    server.delete(&alice, &id, false, None).await;
    let held = server.index.read(&id).await.expect("read").expect("a row");
    server
        .append(
            &alice,
            LedgerEvent::Lifecycle {
                asset_id: id.as_str().to_owned(),
                action: "purge".to_owned(),
                manifest_hash: None,
                sync_seq: None,
            },
        )
        .await;
    assert!(matches!(
        server
            .index
            .purge(&id, retention())
            .await
            .expect("the index purges"),
        crate::index::PurgeOutcome::Purged(_)
    ));
    let original = rows(&server.index).await;

    // Before the collector runs, the manifests are still there: the purged tombstone comes back
    // exactly.
    let before_gc = Arc::new(InMemoryAssetIndex::new());
    let report = rebuild(&server.rebuild_into(before_gc.clone()), Mode::Apply)
        .await
        .expect("a pass");
    assert!(report.owners[0].dangling.is_empty(), "{report:?}");
    assert_eq!(rows(&before_gc).await, original);

    // The collector sweeps what the purge released. Nothing names the album any more, so the
    // row is the backup's alone — reported as purged, and not as damage.
    for address in held
        .blobs
        .iter()
        .map(|blob| &blob.address)
        .chain(&held.superseded)
    {
        server.blobs.remove(address).await.expect("removed");
    }
    let after_gc = Arc::new(InMemoryAssetIndex::new());
    let report = rebuild(&server.rebuild_into(after_gc.clone()), Mode::Apply)
        .await
        .expect("a pass");
    assert_eq!(report.owners[0].purged, vec![id]);
    assert!(report.owners[0].dangling.is_empty(), "{report:?}");
    assert!(!report.has_faults(), "{report:?}");
}

#[tokio::test]
async fn an_unrecorded_delete_that_chains_is_replayed_and_moves_the_epoch() {
    let server = Server::new();
    let alice = owner("alice");
    let id = server.publish(&alice, "crashed-delete", false).await;
    server.delete(&alice, &id, true, None).await;

    let empty = Arc::new(InMemoryAssetIndex::new());
    let report = rebuild(&server.rebuild_into(empty.clone()), Mode::Apply)
        .await
        .expect("a pass");
    assert_eq!(report.owners[0].unprovable, vec![id.clone()]);
    assert_eq!(report.owners[0].epoch, Some(1));
    let row = empty.read(&id).await.expect("read").expect("a row");
    assert_eq!(row.state, AssetState::Tombstoned);
}

#[tokio::test]
async fn an_unrecorded_op_that_does_not_chain_was_refused_and_is_not_replayed() {
    let server = Server::new();
    let alice = owner("alice");
    let id = server.publish(&alice, "refused-delete", false).await;
    server
        .delete(&alice, &id, true, Some(hash_bytes(b"not the head")))
        .await;
    let original = rows(&server.index).await;

    let empty = Arc::new(InMemoryAssetIndex::new());
    let report = rebuild(&server.rebuild_into(empty.clone()), Mode::Apply)
        .await
        .expect("a pass");
    assert!(report.owners[0].unprovable.is_empty(), "{report:?}");
    assert_eq!(rows(&empty).await, original);
}

#[test]
fn only_facts_a_rebuild_can_reconstruct_are_compared() {
    let row = AssetRow {
        asset_id: AssetId::new("a"),
        owner_id: OwnerId::new("o"),
        album_id: AlbumId::new("al"),
        protocol_version: "2026-01-01".to_owned(),
        crypto_suite_id: 1,
        state: AssetState::Visible,
        blobs: Vec::new(),
        first_seq: Some(1),
        sync_seq: Some(1),
        chain_head: None,
        amk_version: 0,
        superseded: Vec::new(),
        hold: None,
        retention_until: None,
        created_at: Timestamp::UNIX_EPOCH,
        updated_at: Timestamp::UNIX_EPOCH,
    };
    // A hold, a later number from an unledgered lifecycle write, and a retention floor are the
    // backup's alone.
    let mut held = row.clone();
    held.hold = Some(crate::index::ServingHold::Takedown);
    held.sync_seq = Some(9);
    held.retention_until = Some(Timestamp::UNIX_EPOCH);
    assert!(differences(&held, &row).is_empty());

    let mut other = row.clone();
    other.owner_id = OwnerId::new("p");
    other.crypto_suite_id = 2;
    assert_eq!(
        differences(&other, &row),
        vec!["owner_id", "crypto_suite_id"]
    );
}
