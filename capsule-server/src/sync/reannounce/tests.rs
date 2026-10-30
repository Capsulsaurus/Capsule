//! Re-announce's contract: a held manifest restores its row and ledgers it so a rebuild keeps it;
//! a manifest whose blobs are missing names them and writes nothing; a manifest the caller's
//! devices did not sign, or for an album the caller cannot write, is refused and writes nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::keys::{AmkVersion, HybridSigningKey};
use capsule_core::crypto::provenance::{
    ASSET_MANIFEST_VERSION, Action, KeyMode, ManifestCore, ProvenanceRecord,
};
use jiff::Timestamp;
use uuid::Uuid;

use super::*;
use crate::attestation::{InMemoryReceipts, LocalAttestationKey, PublishedKey, ReceiptSigner as _};
use crate::blob::{BlobStore as _, InMemoryBlobStore};
use crate::gc::Mode;
use crate::index::AssetIndex as _;
use crate::index::memory::InMemoryAssetIndex;
use crate::ledger::rebuild::{RebuildContext, rebuild};
use crate::ledger::replay::LedgerKeys;
use crate::ledger::{InMemoryLedger, OwnerLedger as _};
use crate::quota::InMemoryQuota;
use crate::store::memory::{InMemoryUploadSessions, ManualClock};
use crate::upload::{AuthorityError, AuthorityFuture, UploadPolicy, WriteAuthority, WriteRole};

const ALBUM: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e70";
const OTHER_ALBUM: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e71";
const PIN: &str = "2026-01-01";

fn at() -> Timestamp {
    Timestamp::from_second(1_767_225_600).expect("a valid instant")
}

fn alice() -> Uuid {
    Uuid::from_u128(7)
}

fn bob() -> Uuid {
    Uuid::from_u128(9)
}

fn device() -> Uuid {
    Uuid::from_u128(8)
}

/// The album and directory facts the authority answers from.
#[derive(Debug, Default)]
struct Authority {
    /// `(caller, album)` pairs that may write, with the album's pin.
    writable: Mutex<BTreeMap<(String, String), String>>,
    /// `(user, device)` → added at.
    devices: Mutex<BTreeMap<(String, Uuid), Timestamp>>,
    /// Whether every question fails, as an unreachable database would.
    down: std::sync::atomic::AtomicBool,
}

impl WriteAuthority for Authority {
    fn album_write_access<'a>(
        &'a self,
        caller: &'a UserId,
        album: &'a AlbumId,
    ) -> AuthorityFuture<'a, AlbumWriteAccess> {
        Box::pin(async move {
            if self.down.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(AuthorityError::unavailable("down"));
            }
            let writable = self
                .writable
                .lock()
                .map_err(|_| AuthorityError::unavailable("poisoned"))?;
            Ok(
                match writable.get(&(caller.as_str().to_owned(), album.as_str().to_owned())) {
                    Some(pin) => AlbumWriteAccess::Writable {
                        owner_id: OwnerId::new(caller.as_str()),
                        role: WriteRole::Owner,
                        protocol_pin: pin.clone(),
                        quiescing_under: None,
                    },
                    None => AlbumWriteAccess::Denied,
                },
            )
        })
    }

    fn device_added_at<'a>(
        &'a self,
        user: &'a UserId,
        device: Uuid,
    ) -> AuthorityFuture<'a, Option<Timestamp>> {
        Box::pin(async move {
            let devices = self
                .devices
                .lock()
                .map_err(|_| AuthorityError::unavailable("poisoned"))?;
            Ok(devices.get(&(user.as_str().to_owned(), device)).copied())
        })
    }

    fn device_directory<'a>(
        &'a self,
        _user: &'a UserId,
    ) -> AuthorityFuture<'a, Option<capsule_core::crypto::keys::DeviceDirectory>> {
        // Re-announcement re-indexes manifests through its own checks and never finalizes an
        // upload, so the signer check that reads this is never reached here.
        Box::pin(async move { Ok(None) })
    }
}

/// One server's stores, with Alice's album writable and her device published.
struct Server {
    index: Arc<InMemoryAssetIndex>,
    blobs: Arc<InMemoryBlobStore>,
    ledger: Arc<InMemoryLedger>,
    quotas: Arc<InMemoryQuota>,
    authority: Arc<Authority>,
    signer: Arc<LocalAttestationKey>,
    clock: Arc<ManualClock>,
    upload: UploadContext,
    attestation: AttestationContext,
    /// Whether Alice's directory is in her ledger yet.
    directory_ledgered: std::sync::atomic::AtomicBool,
}

impl Server {
    fn new() -> Self {
        let index = Arc::new(InMemoryAssetIndex::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let ledger = Arc::new(InMemoryLedger::new());
        let quotas = Arc::new(InMemoryQuota::new());
        let authority = Arc::new(Authority::default());
        let clock = Arc::new(ManualClock::new(at()));
        let signer = Arc::new(LocalAttestationKey::new(
            "server.test",
            HybridSigningKey::from_seed64(&[9; 64]),
        ));
        let upload = UploadContext::new(
            Arc::new(InMemoryUploadSessions::new(
                clock.clone(),
                jiff::SignedDuration::from_hours(24),
            )),
            blobs.clone(),
            index.clone(),
            authority.clone(),
            quotas.clone(),
            clock.clone(),
            UploadPolicy::default(),
        );
        let attestation = AttestationContext::new(
            Arc::new(InMemoryReceipts::new()),
            signer.clone(),
            Timestamp::UNIX_EPOCH,
        )
        .with_ledger(ledger.clone());
        let server = Self {
            index,
            blobs,
            ledger,
            quotas,
            authority,
            signer,
            clock,
            upload,
            attestation,
            directory_ledgered: std::sync::atomic::AtomicBool::new(false),
        };
        server.writable(alice(), ALBUM);
        server.publish_device(alice(), device(), Timestamp::UNIX_EPOCH);
        server
    }

    fn writable(&self, user: Uuid, album: &str) {
        self.authority
            .writable
            .lock()
            .expect("unpoisoned")
            .insert((user.to_string(), album.to_owned()), PIN.to_owned());
    }

    fn publish_device(&self, user: Uuid, device: Uuid, added_at: Timestamp) {
        self.authority
            .devices
            .lock()
            .expect("unpoisoned")
            .insert((user.to_string(), device), added_at);
    }

    /// Ledger Alice's published directory once, before anything of hers, as her publish did: a
    /// rebuild verifies each manifest against the directory version in force at its entry.
    async fn ledger_alices_directory(&self) {
        if self
            .directory_ledgered
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let ik = HybridSigningKey::from_seed64(&[3; 64]);
        let devices = vec![crate::ledger::directory::tests::device(
            device(),
            &HybridSigningKey::from_seed64(&[1; 64]),
            "2025-12-01T00:00:00Z",
            None,
        )];
        let document = crate::ledger::directory::tests::document(&ik, alice(), 1, devices);
        crate::ledger::directory::tests::publish(
            self.ledger.as_ref(),
            self.signer.as_ref(),
            &ik,
            alice(),
            1,
            document,
            at(),
        )
        .await;
    }

    fn context(&self) -> Reannounce<'_> {
        Reannounce {
            upload: &self.upload,
            attestation: &self.attestation,
            quotas: self.quotas.as_ref(),
        }
    }

    async fn reannounce(&self, caller: Uuid, bytes: &[u8]) -> Answer {
        self.ledger_alices_directory().await;
        reannounce(self.context(), &UserId::new(caller.to_string()), bytes)
            .await
            .expect("decided")
    }

    async fn store(&self, bytes: &[u8]) -> ContentAddress {
        let address = ContentAddress::parse(&hash_bytes(bytes).to_hex()).expect("an address");
        self.blobs.put(&address, bytes).await.expect("stored");
        address
    }

    async fn entries(&self, owner: Uuid) -> Vec<LedgerEvent> {
        self.ledger
            .scan(&OwnerId::new(owner.to_string()))
            .await
            .expect("a scan")
            .entries
            .into_iter()
            .map(|entry| entry.core.event)
            // The fixture's own directory publish is not what these cases are about.
            .filter(|event| !matches!(event, LedgerEvent::Directory { .. }))
            .collect()
    }

    fn keys(&self) -> LedgerKeys {
        LedgerKeys::new(vec![PublishedKey {
            key_id: self.signer.key_id(),
            public: self.signer.verifying_key(),
            active_from: Timestamp::UNIX_EPOCH,
            active_to: None,
        }])
    }

    /// Rebuild an empty index from this server's ledger and blobs.
    async fn rebuilt(&self) -> Arc<InMemoryAssetIndex> {
        let index = Arc::new(InMemoryAssetIndex::new());
        let context = RebuildContext::new(
            index.clone(),
            self.blobs.clone(),
            self.ledger.clone(),
            self.quotas.clone(),
            self.keys(),
            self.clock.clone(),
        );
        rebuild(&context, Mode::Apply).await.expect("a pass");
        index
    }
}

/// An asset's held bytes: its original and metadata blobs, and the manifests the client signed.
struct Asset {
    id: Uuid,
    original: Vec<u8>,
    metadata: Vec<u8>,
}

impl Asset {
    fn new(name: &str) -> Self {
        Self {
            id: Uuid::from_u128(u128::from_be_bytes(
                hash_bytes(name.as_bytes()).0[..16]
                    .try_into()
                    .expect("sixteen bytes"),
            )),
            original: format!("{name}-original").into_bytes(),
            metadata: format!("{name}-metadata").into_bytes(),
        }
    }

    fn asset_id(&self) -> AssetId {
        AssetId::new(self.id.to_string())
    }

    fn core(&self, action: Action, prior: Option<Hash32>) -> ManifestCore {
        let binds = action.binds_metadata_blob();
        ManifestCore {
            version: ASSET_MANIFEST_VERSION.to_owned(),
            crypto_suite_id: 1,
            protocol_version: PIN.to_owned(),
            file_id: self.id,
            album_id: Uuid::parse_str(ALBUM).expect("a uuid"),
            amk_version: AmkVersion(0),
            ciphertext_hash: hash_bytes(&self.original),
            plaintext_size: 1024,
            chunk_size: 65_536,
            nonce_prefix: [0; 7],
            key_mode: KeyMode::Derived,
            wrapped_file_key: None,
            metadata_blob_hash: binds.then(|| hash_bytes(&self.metadata)),
            created_by_user: alice(),
            created_by_device: device(),
            client_version: "capsule-cli/0.1.0".to_owned(),
            timestamp: "2026-01-01T00:00:00Z".to_owned(),
            action,
            prior_provenance_hash: prior,
            upgraded_from: None,
            unknown: Default::default(),
            retention_until: (action == Action::Delete).then(|| "2026-02-01T00:00:00Z".to_owned()),
        }
    }

    /// A really signed record over `core`.
    fn record(&self, core: ManifestCore) -> Vec<u8> {
        let prior = core.prior_provenance_hash;
        let signed = core
            .sign(
                &HybridSigningKey::from_seed64(&[1; 64]),
                &HybridSigningKey::from_seed64(&[2; 64]),
            )
            .expect("a manifest signs");
        capsule_core::cbor::to_canonical_vec(&ProvenanceRecord::new(self.id, signed, prior))
            .expect("a record encodes")
    }

    fn create(&self) -> Vec<u8> {
        self.record(self.core(Action::Create, None))
    }

    /// A manifest of `action` chaining onto `prior`'s record.
    fn after(&self, prior: &[u8], action: Action) -> Vec<u8> {
        self.record(self.core(action, Some(hash_bytes(prior))))
    }

    /// A `metadata-update` chaining onto `prior`'s record, committing to `metadata`.
    fn update(&self, prior: &[u8], metadata: &[u8]) -> Vec<u8> {
        let mut core = self.core(Action::MetadataUpdate, Some(hash_bytes(prior)));
        core.metadata_blob_hash = Some(hash_bytes(metadata));
        self.record(core)
    }

    /// Put the original and metadata blobs in the store, as a rebuild leaves them.
    async fn hold(&self, server: &Server) {
        server.store(&self.original).await;
        server.store(&self.metadata).await;
    }
}

fn address(bytes: &[u8]) -> ContentAddress {
    ContentAddress::parse(&hash_bytes(bytes).to_hex()).expect("an address")
}

fn roles(row: &AssetRow) -> BTreeSet<(BlobRole, String)> {
    row.blobs
        .iter()
        .map(|blob| (blob.role, blob.address.as_str().to_owned()))
        .collect()
}

#[tokio::test]
async fn a_held_create_restores_its_row_and_a_second_announce_writes_nothing() {
    let server = Server::new();
    let asset = Asset::new("one");
    asset.hold(&server).await;
    let create = asset.create();
    server.store(&create).await;

    let answer = server.reannounce(alice(), &create).await;
    let Answer::Restored {
        asset: restored,
        sync_seq,
    } = answer
    else {
        panic!("a held create answered {answer:?}");
    };
    assert_eq!(restored, asset.asset_id());
    assert_eq!(
        sync_seq,
        Some(1),
        "the restore is numbered by the owner's allocator"
    );

    let row = server
        .index
        .read(&asset.asset_id())
        .await
        .expect("read")
        .expect("a row");
    assert_eq!(row.owner_id, OwnerId::new(alice().to_string()));
    assert_eq!(row.album_id, AlbumId::new(ALBUM));
    assert_eq!(row.state, AssetState::Visible);
    assert_eq!(row.chain_head, Some(hash_bytes(&create)));
    assert_eq!(
        roles(&row),
        BTreeSet::from([
            (BlobRole::Provenance, address(&create).as_str().to_owned()),
            (
                BlobRole::Metadata,
                address(&asset.metadata).as_str().to_owned()
            ),
            (
                BlobRole::Original,
                address(&asset.original).as_str().to_owned()
            ),
        ])
    );
    assert!(row.superseded.is_empty());

    let entries = server.entries(alice()).await;
    let kinds: Vec<&str> = entries.iter().map(LedgerEvent::kind).collect();
    assert_eq!(kinds, ["custody", "custody", "custody", "indexed"]);
    assert!(matches!(
        &entries[3],
        LedgerEvent::Indexed {
            sync_seq: Some(1),
            ..
        }
    ));
    let usage =
        crate::quota::QuotaStore::usage(server.quotas.as_ref(), &UserId::new(alice().to_string()))
            .await
            .expect("usage");
    assert!(
        usage.used > 0,
        "the restored blobs are attributed to the caller"
    );

    let again = server.reannounce(alice(), &create).await;
    assert_eq!(
        again,
        Answer::AlreadyIndexed {
            asset: asset.asset_id()
        }
    );
    assert_eq!(
        server.entries(alice()).await.len(),
        4,
        "a second announce ledgers nothing"
    );
}

#[tokio::test]
async fn a_rebuild_after_a_restore_keeps_the_restored_row() {
    let server = Server::new();
    let asset = Asset::new("kept");
    asset.hold(&server).await;
    let create = asset.create();
    server.store(&create).await;
    server.reannounce(alice(), &create).await;

    let rebuilt = server.rebuilt().await;
    let original = server
        .index
        .read(&asset.asset_id())
        .await
        .expect("read")
        .expect("a row");
    let again = rebuilt
        .read(&asset.asset_id())
        .await
        .expect("read")
        .expect("a rebuilt row");
    assert_eq!(roles(&again), roles(&original));
    assert_eq!(
        again.sync_seq, original.sync_seq,
        "the indexed entry proves the number"
    );
    assert_eq!(again.chain_head, original.chain_head);
}

#[tokio::test]
async fn a_derivative_head_walks_back_to_the_manifest_naming_the_metadata() {
    let server = Server::new();
    let asset = Asset::new("walked");
    asset.hold(&server).await;
    let create = asset.create();
    let derivative = asset.after(&create, Action::DerivativeAdd);
    server.store(&create).await;
    server.store(&derivative).await;

    let answer = server.reannounce(alice(), &derivative).await;
    assert!(matches!(answer, Answer::Restored { .. }), "got {answer:?}");
    let row = server
        .index
        .read(&asset.asset_id())
        .await
        .expect("read")
        .expect("a row");
    assert_eq!(row.chain_head, Some(hash_bytes(&derivative)));
    assert_eq!(
        row.address_for(BlobRole::Provenance),
        Some(&address(&derivative))
    );
    assert_eq!(
        row.address_for(BlobRole::Metadata),
        Some(&address(&asset.metadata))
    );
    assert_eq!(row.superseded, vec![address(&create)]);

    let kinds: Vec<&str> = server
        .entries(alice())
        .await
        .iter()
        .map(LedgerEvent::kind)
        .collect();
    assert_eq!(
        kinds,
        [
            "custody",
            "custody",
            "custody",
            "lifecycle",
            "indexed",
            "lifecycle"
        ]
    );

    let rebuilt = server.rebuilt().await;
    let again = rebuilt
        .read(&asset.asset_id())
        .await
        .expect("read")
        .expect("a rebuilt row");
    assert_eq!(roles(&again), roles(&row));
    assert_eq!(again.chain_head, row.chain_head);
    assert_eq!(again.superseded, row.superseded);
}

/// The scrub's findings over `server`'s index and store, by class.
async fn scrub_classes(server: &Server) -> Vec<&'static str> {
    use crate::scrub::{Depth, ScrubContext, scrub};
    let context = ScrubContext::new(
        server.index.clone(),
        server.blobs.clone(),
        Arc::new(InMemoryUploadSessions::new(
            server.clock.clone(),
            jiff::SignedDuration::from_hours(24),
        )),
        Arc::new(crate::share::InMemoryShares::default()),
        server.clock.clone(),
    );
    scrub(&context, Depth::Structural)
        .await
        .expect("a scrub pass")
        .findings
        .iter()
        .map(crate::scrub::Finding::class)
        .collect()
}

/// Re-announce a head whose chain runs `create` → `metadata-update` → `extra`, and check the
/// restored row keeps the whole chain: the scrub walks it back to the `create`, the collector
/// counts every earlier manifest as referenced, and a rebuild reproduces it.
async fn reannounce_after_an_update(extra: Option<Action>) {
    let server = Server::new();
    let asset = Asset::new(&format!("updated-{extra:?}"));
    let edited = b"edited-metadata".to_vec();
    server.store(&asset.original).await;
    server.store(&edited).await;
    let create = asset.create();
    let update = asset.update(&create, &edited);
    server.store(&create).await;
    server.store(&update).await;
    let head = match extra {
        Some(action) => {
            let head = asset.after(&update, action);
            server.store(&head).await;
            head
        }
        None => update.clone(),
    };

    let answer = server.reannounce(alice(), &head).await;
    assert!(matches!(answer, Answer::Restored { .. }), "got {answer:?}");
    let row = server
        .index
        .read(&asset.asset_id())
        .await
        .expect("read")
        .expect("a row");
    let mut chain = vec![address(&create)];
    if extra.is_some() {
        chain.push(address(&update));
    }
    assert_eq!(
        row.superseded, chain,
        "every manifest before the head, oldest first"
    );
    assert_eq!(row.address_for(BlobRole::Provenance), Some(&address(&head)));
    assert_eq!(row.address_for(BlobRole::Metadata), Some(&address(&edited)));

    let classes = scrub_classes(&server).await;
    assert!(
        !classes.contains(&"chain_broken"),
        "the chain walks back to its create: {classes:?}"
    );
    assert!(
        !classes.contains(&"orphan"),
        "nothing restored is left for the collector: {classes:?}"
    );
    for manifest in [&create, &update, &head] {
        assert!(
            server
                .index
                .reference_count(&address(manifest))
                .await
                .expect("a count")
                > 0,
            "the collector counts every signed manifest of the chain as referenced"
        );
    }

    let rebuilt = server.rebuilt().await;
    let again = rebuilt
        .read(&asset.asset_id())
        .await
        .expect("read")
        .expect("a rebuilt row");
    assert_eq!(again.superseded, row.superseded);
    assert_eq!(roles(&again), roles(&row));
    assert_eq!(again.chain_head, row.chain_head);
}

#[tokio::test]
async fn a_metadata_update_head_restores_its_whole_chain() {
    reannounce_after_an_update(None).await;
}

#[tokio::test]
async fn a_derivative_after_an_update_restores_its_whole_chain() {
    reannounce_after_an_update(Some(Action::DerivativeAdd)).await;
}

#[tokio::test]
async fn a_manifest_before_the_metadata_that_the_store_lacks_is_named() {
    let server = Server::new();
    let asset = Asset::new("early-gap");
    let edited = b"early-gap-edited".to_vec();
    server.store(&asset.original).await;
    server.store(&edited).await;
    let create = asset.create();
    let update = asset.update(&create, &edited);
    server.store(&update).await;

    let answer = server.reannounce(alice(), &update).await;
    assert_eq!(
        answer,
        Answer::MissingBlobs {
            asset: asset.asset_id(),
            missing: vec![address(&create)],
        }
    );
    assert!(
        server.entries(alice()).await.is_empty(),
        "nothing is ledgered"
    );
}

#[tokio::test]
async fn missing_blobs_are_named_and_nothing_is_restored() {
    let server = Server::new();
    let asset = Asset::new("gone");
    let create = asset.create();
    server.store(&create).await;
    server.store(&asset.metadata).await;

    let answer = server.reannounce(alice(), &create).await;
    assert_eq!(
        answer,
        Answer::MissingBlobs {
            asset: asset.asset_id(),
            missing: vec![address(&asset.original)],
        }
    );
    assert!(
        server
            .index
            .read(&asset.asset_id())
            .await
            .expect("read")
            .is_none()
    );
    assert!(
        server.entries(alice()).await.is_empty(),
        "nothing is ledgered"
    );
}

#[tokio::test]
async fn a_head_whose_predecessor_the_store_lacks_names_it() {
    let server = Server::new();
    let asset = Asset::new("broken-chain");
    asset.hold(&server).await;
    let create = asset.create();
    let derivative = asset.after(&create, Action::DerivativeAdd);
    server.store(&derivative).await;

    let answer = server.reannounce(alice(), &derivative).await;
    assert_eq!(
        answer,
        Answer::MissingBlobs {
            asset: asset.asset_id(),
            missing: vec![address(&create)],
        }
    );
    assert!(
        server
            .index
            .read(&asset.asset_id())
            .await
            .expect("read")
            .is_none()
    );
}

#[tokio::test]
async fn a_manifest_another_account_signed_is_refused() {
    let server = Server::new();
    server.writable(bob(), ALBUM);
    server.publish_device(bob(), device(), Timestamp::UNIX_EPOCH);
    let asset = Asset::new("alices");
    asset.hold(&server).await;
    let create = asset.create();
    server.store(&create).await;

    let answer = server.reannounce(bob(), &create).await;
    assert_eq!(
        answer,
        Answer::Refused {
            asset: Some(asset.asset_id()),
            refusal: Refusal::Denied
        }
    );
    assert!(
        server
            .index
            .read(&asset.asset_id())
            .await
            .expect("read")
            .is_none()
    );
    assert!(server.entries(bob()).await.is_empty());
    assert!(server.entries(alice()).await.is_empty());
}

#[tokio::test]
async fn write_authority_is_decided_as_an_upload_decides_it() {
    let asset = Asset::new("authority");
    let create = asset.create();

    // No write access to the album.
    let server = Server::new();
    server
        .authority
        .writable
        .lock()
        .expect("unpoisoned")
        .clear();
    assert!(matches!(
        server.reannounce(alice(), &create).await,
        Answer::Refused {
            refusal: Refusal::Denied,
            ..
        }
    ));

    // The album's pin is not the manifest's.
    let server = Server::new();
    server
        .authority
        .writable
        .lock()
        .expect("unpoisoned")
        .insert(
            (alice().to_string(), ALBUM.to_owned()),
            "2027-01-01".to_owned(),
        );
    assert!(matches!(
        server.reannounce(alice(), &create).await,
        Answer::Refused {
            refusal: Refusal::Denied,
            ..
        }
    ));

    // The device is not in the caller's directory.
    let server = Server::new();
    server.authority.devices.lock().expect("unpoisoned").clear();
    assert!(matches!(
        server.reannounce(alice(), &create).await,
        Answer::Refused {
            refusal: Refusal::Denied,
            ..
        }
    ));

    // The device was added after the manifest was signed.
    let server = Server::new();
    server.publish_device(
        alice(),
        device(),
        at() + jiff::SignedDuration::from_hours(1),
    );
    assert!(matches!(
        server.reannounce(alice(), &create).await,
        Answer::Refused {
            refusal: Refusal::Denied,
            ..
        }
    ));
}

#[tokio::test]
async fn an_asset_id_filed_under_another_album_is_refused() {
    let server = Server::new();
    server.writable(alice(), OTHER_ALBUM);
    let asset = Asset::new("elsewhere");
    asset.hold(&server).await;
    server
        .index
        .reserve(crate::index::PendingAsset {
            asset_id: asset.asset_id(),
            owner_id: OwnerId::new(alice().to_string()),
            album_id: AlbumId::new(OTHER_ALBUM),
            protocol_version: PIN.to_owned(),
            crypto_suite_id: 1,
            created_at: at(),
        })
        .await
        .expect("the index reserves");

    let answer = server.reannounce(alice(), &asset.create()).await;
    assert!(
        matches!(
            answer,
            Answer::Refused {
                refusal: Refusal::Denied,
                ..
            }
        ),
        "got {answer:?}"
    );
}

#[tokio::test]
async fn bytes_that_are_not_a_manifest_are_malformed() {
    let server = Server::new();
    let answer = server.reannounce(alice(), b"not a manifest").await;
    assert_eq!(
        answer,
        Answer::Refused {
            asset: None,
            refusal: Refusal::Malformed
        }
    );
}

#[tokio::test]
async fn a_deletion_is_not_reannounced() {
    let server = Server::new();
    let asset = Asset::new("deleted");
    asset.hold(&server).await;
    let create = asset.create();
    server.store(&create).await;
    let delete = asset.after(&create, Action::Delete);
    server.store(&delete).await;

    let answer = server.reannounce(alice(), &delete).await;
    assert_eq!(
        answer,
        Answer::Refused {
            asset: Some(asset.asset_id()),
            refusal: Refusal::Deleted
        }
    );
    assert!(
        server
            .index
            .read(&asset.asset_id())
            .await
            .expect("read")
            .is_none()
    );
}

#[tokio::test]
async fn an_unanswering_authority_is_unavailable_not_a_refusal() {
    let server = Server::new();
    let asset = Asset::new("outage");
    asset.hold(&server).await;
    let create = asset.create();
    server.store(&create).await;
    server
        .authority
        .down
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let outcome = reannounce(server.context(), &UserId::new(alice().to_string()), &create).await;
    assert!(outcome.is_err(), "got {outcome:?}");
}

#[test]
fn each_refusal_has_its_own_code() {
    let codes: BTreeSet<&str> = [Refusal::Malformed, Refusal::Denied, Refusal::Deleted]
        .into_iter()
        .map(Refusal::code)
        .collect();
    assert_eq!(codes.len(), 3);
}
