//! The scrub's own suite.
//!
//! Two properties carry it: a clean store yields nothing and yields the same nothing twice,
//! and each seeded corruption is reported as **its own class** — because a scrub that reports
//! "something is wrong" is a scrub an operator cannot act on.
//!
//! Every case also asserts the store and index are unchanged afterwards. That is the one
//! property this module's whole design rests on, so it is checked rather than assumed.

use std::sync::Arc;

use capsule_core::crypto::hash::{Hash32, hash_bytes};
use jiff::Timestamp;

use super::*;
use crate::blob::{InMemoryBlobStore, QuarantineReason};
use crate::index::memory::InMemoryAssetIndex;
use crate::index::{BlobRecord, PendingAsset};
use crate::store::memory::InMemoryUploadSessions;
use crate::store::{AlbumId, OwnerId, SystemClock, UploadId};

/// The three stores, assembled.
struct Harness {
    context: ScrubContext,
    index: Arc<InMemoryAssetIndex>,
    blobs: Arc<InMemoryBlobStore>,
}

/// The album every scrub fixture files under, as the manifest's `album_id` must be a UUID.
const SCRUB_ALBUM: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e70";

/// A stable UUID for a fixture's asset name.
///
/// The index keys assets by an opaque string and a manifest's `file_id` is a `Uuid`, so a
/// fixture that wants the two to agree — which check 5 requires of every clean store — has to
/// pick ids that are both.
fn asset_uuid(name: &str) -> uuid::Uuid {
    uuid::Uuid::from_u128(u128::from_be_bytes(
        hash_bytes(name.as_bytes()).0[..16]
            .try_into()
            .expect("sixteen bytes"),
    ))
}

/// The asset id `publish` files `name` under.
fn asset_id(name: &str) -> AssetId {
    AssetId::new(asset_uuid(name).to_string())
}

/// A signed manifest whose facts are the ones `publish` writes into the index.
///
/// Really signed, over freshly generated keys: the scrub never verifies the signatures — it
/// decodes — but a fixture that hand-rolled a struct with placeholder signature bytes would be
/// asserting against a shape rather than against the artifact.
fn manifest_bytes(name: &str, metadata: &ContentAddress) -> Vec<u8> {
    use capsule_core::crypto::keys::AmkVersion;
    use capsule_core::crypto::keys::hybrid_sig::HybridSigningKey;
    use capsule_core::crypto::provenance::action::Action;
    use capsule_core::crypto::provenance::manifest::{
        ASSET_MANIFEST_VERSION, KeyMode, ManifestCore,
    };

    let core = ManifestCore {
        version: ASSET_MANIFEST_VERSION.to_owned(),
        crypto_suite_id: 1,
        protocol_version: "2026-01-01".to_owned(),
        file_id: asset_uuid(name),
        album_id: uuid::Uuid::parse_str(SCRUB_ALBUM).expect("a uuid"),
        amk_version: AmkVersion(0),
        ciphertext_hash: hash_bytes(format!("{name}-original").as_bytes()),
        plaintext_size: 1024,
        chunk_size: 65_536,
        nonce_prefix: [0; 7],
        key_mode: KeyMode::Derived,
        wrapped_file_key: None,
        metadata_blob_hash: Some(
            capsule_core::crypto::hash::Hash32::from_hex(metadata.as_str()).expect("a digest"),
        ),
        created_by_user: uuid::Uuid::from_u128(7),
        created_by_device: uuid::Uuid::from_u128(8),
        client_version: "capsule-cli/0.1.0".to_owned(),
        timestamp: "2026-01-01T00:00:00Z".to_owned(),
        action: Action::Create,
        prior_provenance_hash: None,
        retention_until: None,
    };
    let device = HybridSigningKey::from_seed64(&[1; 64]);
    let write_tier = HybridSigningKey::from_seed64(&[2; 64]);
    let manifest = core.sign(&device, &write_tier).expect("a manifest signs");
    capsule_core::cbor::to_canonical_vec(&manifest).expect("a manifest encodes")
}

impl Harness {
    fn new() -> Self {
        let index = Arc::new(InMemoryAssetIndex::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let uploads = Arc::new(InMemoryUploadSessions::new(
            Arc::new(SystemClock),
            jiff::SignedDuration::from_hours(24),
        ));
        let context = ScrubContext::new(index.clone(), blobs.clone(), uploads);
        Self {
            context,
            index,
            blobs,
        }
    }

    /// Put `bytes` in the store at their own address.
    async fn store(&self, bytes: &[u8]) -> ContentAddress {
        let address = ContentAddress::parse(&hash_bytes(bytes).to_hex()).expect("an address");
        self.blobs.put(&address, bytes).await.expect("stored");
        address
    }

    /// Publish `asset` with its index tier, and return its provenance address.
    ///
    /// The provenance blob is a **real signed manifest** whose facts match the row, not a
    /// placeholder: check 5 decodes it and compares (`S-C45`), so a fixture that stored
    /// arbitrary bytes there would be seeding a corruption in every case that is supposed to be
    /// clean.
    async fn publish(&self, asset: &str) -> ContentAddress {
        let id = asset_id(asset);
        self.index
            .reserve(PendingAsset {
                asset_id: id.clone(),
                owner_id: OwnerId::new("scrub-owner"),
                album_id: AlbumId::new(SCRUB_ALBUM),
                protocol_version: "2026-01-01".to_owned(),
                crypto_suite_id: 1,
                created_at: Timestamp::UNIX_EPOCH,
            })
            .await
            .expect("the index reserves");

        let metadata = self.store(format!("{asset}-metadata").as_bytes()).await;
        let provenance = self.store(&manifest_bytes(asset, &metadata)).await;
        self.record(&id, BlobRole::Provenance, &provenance).await;
        self.record(&id, BlobRole::Metadata, &metadata).await;
        provenance
    }

    async fn record(&self, asset: &AssetId, role: BlobRole, address: &ContentAddress) {
        self.index
            .record_blob(
                asset,
                BlobRecord {
                    manifest_sha256: (role == BlobRole::Provenance)
                        .then(|| Hash32::from_hex(address.as_str()).expect("a digest")),
                    role,
                    address: address.clone(),
                    size: 32,
                    finalized_at: Timestamp::UNIX_EPOCH,
                },
            )
            .await
            .expect("the index records");
    }

    /// A snapshot of both sides, so a case can assert the scrub changed nothing.
    async fn snapshot(&self) -> (Vec<AssetRow>, Vec<ContentAddress>) {
        let rows = self.index.rows(None, 1000).await.expect("rows");
        let page = self.blobs.enumerate(None, 1000).await.expect("enumerate");
        (
            rows,
            page.entries.into_iter().map(|stat| stat.address).collect(),
        )
    }
}

use crate::index::AssetRow;

/// A deep pass with a budget nothing will reach.
fn deep() -> Depth {
    Depth::Deep {
        budget: 1024 * 1024,
    }
}

// ===========================================================================================

#[tokio::test]
async fn a_clean_store_yields_nothing_and_yields_it_twice() {
    let h = Harness::new();
    h.publish("clean").await;

    let first = scrub(&h.context, deep()).await.expect("a pass");
    assert!(first.is_clean(), "found {:?}", first.findings);
    assert_eq!(first.counts(), Vec::new());

    let second = scrub(&h.context, deep()).await.expect("a pass");
    assert_eq!(
        first, second,
        "a scrub that is not idempotent on a clean store is a scrub whose output nobody can \
         diff between runs"
    );
}

#[tokio::test]
async fn a_missing_blob_is_a_dangling_reference_and_the_row_survives() {
    let h = Harness::new();
    let provenance = h.publish("dangling").await;
    let before = h.snapshot().await;
    h.blobs
        .remove(&provenance)
        .await
        .expect("the store removes");

    let report = scrub(&h.context, Depth::Structural).await.expect("a pass");
    assert_eq!(report.count("dangling_reference"), 1);
    assert!(matches!(
        report.findings.first(),
        Some(Finding::DanglingReference {
            role: BlobRole::Provenance,
            ..
        })
    ));

    let (rows, _) = h.snapshot().await;
    assert_eq!(
        rows, before.0,
        "erasing the row would destroy the only record that the asset should exist, which is \
         why this classifies and never repairs"
    );
}

#[tokio::test]
async fn a_missing_provenance_blob_also_breaks_the_chain_head() {
    let h = Harness::new();
    let provenance = h.publish("chain").await;
    h.blobs
        .remove(&provenance)
        .await
        .expect("the store removes");

    let report = scrub(&h.context, Depth::Structural).await.expect("a pass");
    assert_eq!(
        report.count("chain_head_unresolvable"),
        1,
        "the chain head names a provenance blob, so losing it is two findings and not one — \
         and they are separate classes because they call for different investigations"
    );
    assert_eq!(report.count("dangling_reference"), 1);
}

#[tokio::test]
async fn an_unreferenced_blob_is_an_orphan_and_is_left_alone() {
    let h = Harness::new();
    h.publish("referenced").await;
    let orphan = h.store(b"a finalization-crash orphan").await;

    let report = scrub(&h.context, Depth::Structural).await.expect("a pass");
    assert_eq!(
        report.findings,
        vec![Finding::Orphan {
            address: orphan.clone()
        }]
    );
    assert!(
        h.blobs.stat(&orphan).await.expect("stat").is_some(),
        "reporting for the collector is the whole job; removing here would make the scrub the \
         deletion bug it exists to catch"
    );
}

#[tokio::test]
async fn bit_rot_is_found_only_by_a_deep_pass() {
    let h = Harness::new();
    let id = asset_id("rotten");
    h.publish("rotten").await;

    // A blob whose bytes do not hash to its name — what bit rot looks like from outside. The
    // *address* is the one the asset's manifest commits to, because rot is bytes going bad under
    // a correct name; an address nothing committed to would be a different fault, and since
    // `S-C45` the scrub would report that one too.
    let honest = hash_bytes(b"rotten-original").to_hex();
    let address = ContentAddress::parse(&honest).expect("an address");
    h.blobs
        .put(&address, b"different bytes entirely")
        .await
        .expect("stored");
    h.record(&id, BlobRole::Original, &address).await;

    let structural = scrub(&h.context, Depth::Structural).await.expect("a pass");
    assert!(
        structural.is_clean(),
        "a structural pass stats and does not read, so it cannot see rot: {:?}",
        structural.findings
    );

    let report = scrub(&h.context, deep()).await.expect("a pass");
    assert_eq!(report.count("byte_mismatch"), 1);
    assert!(report.bytes_hashed > 0);
}

#[tokio::test]
async fn a_deep_pass_that_runs_out_of_budget_is_not_clean() {
    let h = Harness::new();
    h.publish("budgeted").await;

    let report = scrub(&h.context, Depth::Deep { budget: 1 })
        .await
        .expect("a pass");
    assert!(report.findings.is_empty());
    assert!(report.budget_exhausted);
    assert!(
        !report.is_clean(),
        "a clean report from a truncated pass is not a clean store, and an operator alerting on \
         the finding count has to be able to tell the difference"
    );
}

#[tokio::test]
async fn quarantined_blobs_and_stale_stages_are_inventoried() {
    let h = Harness::new();
    let provenance = h.publish("held").await;
    h.blobs
        .quarantine(
            &provenance,
            QuarantineReason {
                code: "error.scrub.hash_mismatch".to_owned(),
                detail: "seeded".to_owned(),
                at: Timestamp::UNIX_EPOCH,
            },
        )
        .await
        .expect("the store quarantines");

    // A stage with no session behind it.
    let upload = UploadId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f");
    h.blobs.begin(&upload).await.expect("the store stages");

    let report = scrub(&h.context, Depth::Structural).await.expect("a pass");
    assert_eq!(report.count("quarantined"), 1);
    assert_eq!(report.count("stale_stage"), 1);
    assert_eq!(
        report.count("dangling_reference"),
        1,
        "quarantining takes the bytes out of the store, so the row that still references them \
         is dangling — which is the honest reading, not a double-report"
    );
}

#[tokio::test]
async fn the_report_counts_by_class() {
    let h = Harness::new();
    h.publish("counted-one").await;
    h.publish("counted-two").await;
    h.store(b"orphan one").await;
    h.store(b"orphan two").await;

    let report = scrub(&h.context, Depth::Structural).await.expect("a pass");
    assert_eq!(report.counts(), vec![("orphan", 2)]);
    assert!(!report.is_clean());
}

// ===========================================================================================
// Check 5: mirrored-fact agreement (`S-C45`)
// ===========================================================================================

#[tokio::test]
async fn a_mirrored_fact_that_disagrees_with_the_signed_manifest_is_reported() {
    // The check the maintenance doc has always listed and neither server performed, because
    // performing it means decoding signed CBOR. It is decodable *here* for a reason that turned
    // out to be simpler than the question sounded: the provenance blob is deliberately
    // server-visible signed CBOR carrying no plaintext secrets by construction, and the scrub is
    // read-only and acts on nothing it finds.
    let h = Harness::new();
    h.publish("mirrored").await;
    let id = asset_id("mirrored");

    // Move the index's copy of a fact the manifest signed. In production this is an
    // implementation bug or a hand-edited row; the scrub's job is that neither is silent.
    h.index
        .apply_op(crate::index::LifecycleOp {
            asset_id: id.clone(),
            owner_id: OwnerId::new("scrub-owner"),
            album_id: AlbumId::new(SCRUB_ALBUM),
            action: crate::index::OpAction::MetadataUpdate,
            manifest_hash: hash_bytes(b"a manifest the store does not hold"),
            prior_provenance_hash: h
                .index
                .read(&id)
                .await
                .expect("read")
                .expect("the row exists")
                .chain_head,
            amk_version: 9,
            provenance: h
                .index
                .read(&id)
                .await
                .expect("read")
                .expect("the row exists")
                .address_for(BlobRole::Provenance)
                .cloned()
                .expect("a provenance blob"),
            metadata: None,
            original: None,
            retention_until: None,
            at: Timestamp::UNIX_EPOCH,
        })
        .await
        .expect("the index applies");

    let report = scrub(&h.context, Depth::Structural).await.expect("a pass");
    let mismatches: Vec<_> = report
        .findings
        .iter()
        .filter(|finding| finding.class() == "mirrored_fact_mismatch")
        .collect();
    assert_eq!(mismatches.len(), 1, "{:?}", report.findings);
    assert!(
        matches!(
            mismatches[0],
            Finding::MirroredFactMismatch { fact: "amk_version", index, manifest, .. }
                if index == "9" && manifest == "0"
        ),
        "the report carries both sides and adjudicates neither: {:?}",
        mismatches[0]
    );
}

#[tokio::test]
async fn a_provenance_blob_that_is_not_a_manifest_is_reported_rather_than_assumed_to_agree() {
    let h = Harness::new();
    let id = asset_id("unreadable");
    h.publish("unreadable").await;

    // Re-point the provenance role at bytes that are not a manifest, the only way a lifecycle
    // write legitimately moves that role — which is what makes this reachable at all.
    let junk = h.store(b"not CBOR, not a manifest, not anything").await;
    h.index
        .apply_op(crate::index::LifecycleOp {
            asset_id: id.clone(),
            owner_id: OwnerId::new("scrub-owner"),
            album_id: AlbumId::new(SCRUB_ALBUM),
            action: crate::index::OpAction::MetadataUpdate,
            manifest_hash: hash_bytes(b"whatever"),
            prior_provenance_hash: h
                .index
                .read(&id)
                .await
                .expect("read")
                .expect("the row exists")
                .chain_head,
            amk_version: 0,
            provenance: junk.clone(),
            metadata: None,
            original: None,
            retention_until: None,
            at: Timestamp::UNIX_EPOCH,
        })
        .await
        .expect("the index applies");

    let report = scrub(&h.context, Depth::Structural).await.expect("a pass");
    assert_eq!(
        report.count("manifest_unreadable"),
        1,
        "{:?}",
        report.findings
    );
    assert_eq!(
        report.count("mirrored_fact_mismatch"),
        0,
        "a blob that does not decode has no facts to disagree with, so it is one finding rather \
         than a cascade of them"
    );
}
