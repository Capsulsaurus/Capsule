use capsule_core::cbor;
use tempfile::TempDir;

use super::*;
use crate::lifecycle::fast_workspace;

/// The three byte strings a feed entry carries for one asset, exactly as `upload_bundle`
/// puts them on the wire.
struct Wire {
    /// The **provenance blob**: the canonical CBOR of the chain head record, taken
    /// straight off `UploadBundle::provenance_blob` so these fixtures cannot drift from
    /// what the push ladder actually uploads.
    manifest_cbor: Vec<u8>,
    metadata_blob: Vec<u8>,
    ciphertext: Vec<u8>,
}

/// A workspace holding one imported asset, plus that asset's wire bytes.
fn seeded(lib: &TempDir, src: &TempDir) -> (Workspace, Uuid, Uuid, Wire) {
    let img = src.path().join("photo.jpg");
    std::fs::write(&img, b"\xFF\xD8\xFF sync-apply bytes").unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    let asset = ws.import_asset(album, &img).unwrap();

    let bundle = ws.upload_bundle(&asset).unwrap();
    let wire = Wire {
        manifest_cbor: bundle.provenance_blob.clone(),
        metadata_blob: bundle.metadata_blob.clone(),
        ciphertext: bundle.original.to_vec().unwrap(),
    };
    (ws, album, asset, wire)
}

fn entry(album: Uuid, wire: &Wire, head: Option<Hash32>) -> RemoteEntry<'_> {
    RemoteEntry {
        album_id: album,
        manifest_cbor: &wire.manifest_cbor,
        metadata_blob: &wire.metadata_blob,
        original_ciphertext: &wire.ciphertext,
        local_chain_head: head,
    }
}

/// The happy path: a signed create entry arriving at a catalog that has never seen the
/// asset verifies through the chokepoint and yields facts the caller can upsert — the
/// decrypted sidecar included, and a chain head to feed back next time.
#[test]
fn unseen_entry_verifies_and_yields_facts() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (ws, album, asset, wire) = seeded(&lib, &src);

    let outcome = ws.apply_remote_entry(entry(album, &wire, None)).unwrap();
    let facts = outcome
        .facts()
        .expect("a create into an unseen catalog applies");
    assert_eq!(facts.asset_id, asset);
    assert_eq!(facts.album_id, album);
    assert_eq!(facts.amk_version, 1);
    assert_eq!(facts.action, Action::Create);
    assert_eq!(facts.created_by_device, ws.device_id());
    // The metadata blob really was opened: the sidecar's own identity fields survived.
    let sidecar = facts.sidecar.as_ref().expect("a create carries metadata");
    assert_eq!(sidecar.uuid, asset);
    assert_eq!(sidecar.content_type, "image/jpeg");
    assert!(sidecar.signature.is_some());
}

/// A **tombstone** — a `delete`, which mints no metadata blob — is a fully-verified entry
/// that applies with no sidecar. Treating its absent blob as a binding failure would
/// quarantine every deletion the feed delivers, so this pins the branch.
#[test]
fn a_delete_tombstone_applies_without_a_sidecar() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (mut ws, album, asset, _create) = seeded(&lib, &src);

    // Author the deletion locally, then replay its head manifest as a feed entry.
    ws.soft_delete(&asset, 30).unwrap();
    let records = ws.asset(&asset).unwrap().chain.records();
    let create_head = records[0].record_hash();
    let bundle = ws.upload_bundle(&asset).unwrap();
    let wire = Wire {
        manifest_cbor: bundle.provenance_blob.clone(),
        // A tombstone carries no metadata blob on the wire.
        metadata_blob: Vec::new(),
        ciphertext: bundle.original.to_vec().unwrap(),
    };

    let outcome = ws
        .apply_remote_entry(entry(album, &wire, Some(create_head)))
        .unwrap();
    let facts = outcome.facts().expect("a tombstone applies");
    assert_eq!(facts.action, Action::Delete);
    assert!(
        facts.sidecar.is_none(),
        "an action that mints no metadata blob yields no sidecar"
    );
    // It still chains: the head it establishes is what the catalog persists next.
    assert_ne!(facts.provenance_head, create_head);
}

/// The same entry replayed against a catalog that already holds the asset is refused as a
/// replay — the chain-head parameter is load-bearing, not decorative.
#[test]
fn replayed_create_is_quarantined() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (ws, album, _asset, wire) = seeded(&lib, &src);

    let head = ws
        .apply_remote_entry(entry(album, &wire, None))
        .unwrap()
        .facts()
        .expect("first application succeeds")
        .provenance_head;

    assert_eq!(
        ws.apply_remote_entry(entry(album, &wire, Some(head)))
            .unwrap(),
        SyncApplyOutcome::Quarantined(QuarantineReason::Rejected(RejectReason::Replayed))
    );
}

/// Tampering with any of the byte strings quarantines with a *named* reason, and never with
/// an `Err` — one hostile row must not abort a sync.
#[test]
fn tampered_entries_quarantine_with_named_reasons() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (ws, album, _asset, wire) = seeded(&lib, &src);

    // Garbage manifest bytes.
    let garbage = Wire {
        manifest_cbor: b"not cbor at all".to_vec(),
        metadata_blob: wire.metadata_blob.clone(),
        ciphertext: wire.ciphertext.clone(),
    };
    assert!(matches!(
        ws.apply_remote_entry(entry(album, &garbage, None)).unwrap(),
        SyncApplyOutcome::Quarantined(QuarantineReason::MalformedManifest(_))
    ));

    // Flipped ciphertext: the chokepoint's content-integrity step refuses it.
    let mut flipped = wire.ciphertext.clone();
    flipped[0] ^= 0xFF;
    let tampered = Wire {
        manifest_cbor: wire.manifest_cbor.clone(),
        metadata_blob: wire.metadata_blob.clone(),
        ciphertext: flipped,
    };
    assert_eq!(
        ws.apply_remote_entry(entry(album, &tampered, None))
            .unwrap(),
        SyncApplyOutcome::Quarantined(QuarantineReason::Rejected(
            RejectReason::CiphertextHashMismatch
        ))
    );

    // Flipped metadata blob: the binding's content-address half refuses it.
    let mut blob = wire.metadata_blob.clone();
    let last = blob.len() - 1;
    blob[last] ^= 0xFF;
    let unbound = Wire {
        manifest_cbor: wire.manifest_cbor.clone(),
        metadata_blob: blob,
        ciphertext: wire.ciphertext.clone(),
    };
    assert_eq!(
        ws.apply_remote_entry(entry(album, &unbound, None)).unwrap(),
        SyncApplyOutcome::Quarantined(QuarantineReason::Binding(BindingReject::BlobHashMismatch))
    );

    // An album this workspace holds no keys for is "cannot adjudicate", not "rejected".
    let foreign = Uuid::now_v7();
    assert_eq!(
        ws.apply_remote_entry(entry(foreign, &wire, None)).unwrap(),
        SyncApplyOutcome::Quarantined(QuarantineReason::UnknownAlbum(foreign))
    );
}

/// **The encoding the whole chain rests on.** The bytes the push ladder uploads as the
/// `provenance` blob — and therefore the bytes the feed serves back as `manifest_cbor` —
/// are the canonical CBOR of the head record, whose digest is by definition
/// `record_hash()`.
///
/// That equality is the entire reason the encoding is not a free choice: the server's chain
/// head is the SHA-256 of the blob's bytes, and a client's next `prior_provenance_hash` is
/// `record_hash()`. Encode the bare manifest instead and the two are different numbers, so
/// no lifecycle op can ever chain onto a synced asset.
#[test]
fn the_provenance_blob_is_the_canonical_record_and_hashes_to_the_chain_head() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (ws, _album, asset, wire) = seeded(&lib, &src);

    let head = ws
        .asset(&asset)
        .unwrap()
        .chain
        .records()
        .last()
        .unwrap()
        .clone();
    assert_eq!(
        capsule_core::crypto::hash::hash_bytes(&wire.manifest_cbor),
        head.record_hash(),
        "the blob's digest is the value a later op's prior_provenance_hash must equal"
    );

    let decoded: ProvenanceRecord = cbor::from_slice(&wire.manifest_cbor).unwrap();
    assert_eq!(
        decoded, head,
        "the wire bytes round-trip to the head record"
    );
    // "The signed bytes are the served bytes" survives the wrapper: canonical CBOR is
    // deterministic, so the manifest that comes back out of the record encodes to exactly
    // the bytes the manifest encodes to on its own — the wrapper carries it, it does not
    // re-author it. That the signatures over those bytes still verify is
    // `unseen_entry_verifies_and_yields_facts`, through `verify_asset`.
    assert_eq!(
        cbor::to_canonical_vec(&decoded.manifest).unwrap(),
        cbor::to_canonical_vec(&head.manifest).unwrap(),
    );
}

/// The record's `prior_provenance_hash` and its manifest's must agree — provenance.md calls
/// them "a checked invariant, not trusted redundancy". Only the manifest's copy is signed,
/// so a divergence is refused rather than resolved in either direction.
#[test]
fn a_record_whose_mirror_diverges_is_quarantined() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (ws, album, _asset, wire) = seeded(&lib, &src);

    let mut record: ProvenanceRecord = cbor::from_slice(&wire.manifest_cbor).unwrap();
    assert_eq!(record.prior_provenance_hash, None, "a create has no prior");
    // A prior the signed manifest does not carry: the unsigned copy claims a chain
    // position the signed one denies.
    record.prior_provenance_hash = Some(Hash32::from_bytes([0x11; 32]));

    let forged = Wire {
        manifest_cbor: cbor::to_canonical_vec(&record).unwrap(),
        metadata_blob: wire.metadata_blob.clone(),
        ciphertext: wire.ciphertext.clone(),
    };
    assert!(
        matches!(
            ws.apply_remote_entry(entry(album, &forged, None)).unwrap(),
            SyncApplyOutcome::Quarantined(QuarantineReason::MalformedManifest(_))
        ),
        "a divergent mirror is malformed, not applied"
    );
}

/// Publish a second device of the same account and make it this workspace's signer — the
/// laptop to the creator's phone.
fn become_second_device(ws: &mut Workspace) -> Uuid {
    use capsule_core::crypto::keys::{DeviceEntry, HybridSigningKey};

    let second = Uuid::from_u128(0xD2);
    let dsk = HybridSigningKey::from_seed_bytes(&[31; 32], &[32; 32]);
    let mut core = ws.directory.core.clone();
    core.directory_version += 1;
    core.devices.push(DeviceEntry {
        device_id: second,
        dsk_public: dsk.verifying_key(),
        dek_public: None,
        added_at: "2020-01-01T00:00:00Z".into(),
        revoked_at: None,
        unknown: Default::default(),
    });
    ws.directory = core.sign(&ws.account.user_ik);
    ws.account.device.device_id = second;
    ws.device_signer = Box::new(dsk);
    second
}

/// Sign `core` with this workspace's current device and the album's write-tier key, and
/// wrap it as the record the feed would serve.
fn signed_record(
    ws: &Workspace,
    album: Uuid,
    core: capsule_core::crypto::provenance::ManifestCore,
) -> ProvenanceRecord {
    let keys = ws.album(&album).unwrap();
    let manifest = core
        .sign(ws.device_signer.as_ref(), keys.write_tier_signer().unwrap())
        .unwrap();
    let (asset_id, prior) = (manifest.core.file_id, manifest.core.prior_provenance_hash);
    ProvenanceRecord::new(asset_id, manifest, prior)
}

/// **A chain an old client already wrote** (issue #475): the creator's device imports, a
/// second device of the same account deletes, and the pre-fix `sign_lifecycle` attributed
/// that delete to the creator while signing with the second device.
///
/// The decision this pins (provenance.md § Legacy Inherited-Device Records): the record is
/// quarantined in place — never accepted, never re-signed — under a signal distinct from a
/// forgery, and the rest of the chain stalls behind it as `ForgedChain` whose
/// `prior_provenance_hash` names the stalled record.
#[test]
fn a_legacy_inherited_device_chain_stalls_with_a_distinct_signal() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (mut ws, album, asset, create) = seeded(&lib, &src);
    let creator = ws.device_id();

    // The receiving catalog holds the create.
    let create_head = ws
        .apply_remote_entry(entry(album, &create, None))
        .unwrap()
        .facts()
        .expect("the create applies")
        .provenance_head;

    // The second device deletes; a fixed client names itself. Rewrite that record as the
    // old client wrote it: the creator inherited as author, the second device still signing.
    let second = become_second_device(&mut ws);
    ws.soft_delete(&asset, 30).unwrap();
    let mut legacy_core = ws
        .asset(&asset)
        .unwrap()
        .chain
        .records()
        .last()
        .unwrap()
        .manifest
        .core
        .clone();
    assert_eq!(legacy_core.prior_provenance_hash, Some(create_head));
    legacy_core.created_by_device = creator;
    let legacy = signed_record(&ws, album, legacy_core.clone());
    let legacy_hash = legacy.record_hash();
    let legacy_wire = Wire {
        manifest_cbor: cbor::to_canonical_vec(&legacy).unwrap(),
        metadata_blob: Vec::new(),
        ciphertext: create.ciphertext.clone(),
    };

    assert_eq!(
        ws.apply_remote_entry(entry(album, &legacy_wire, Some(create_head)))
            .unwrap(),
        SyncApplyOutcome::Quarantined(QuarantineReason::LegacyInheritedDevice {
            named_device: creator,
            signing_device: second,
            record_hash: legacy_hash,
        }),
        "the legacy record is refused, and named as the legacy defect rather than a forgery"
    );

    // The tail: a correctly attributed restore chained onto the legacy record. The catalog's
    // head never advanced past the create, so it stalls — and it points at the stall.
    let mut tail_core = legacy_core;
    tail_core.action = Action::TrashRestore;
    tail_core.retention_until = None;
    tail_core.prior_provenance_hash = Some(legacy_hash);
    tail_core.created_by_device = second;
    let tail = signed_record(&ws, album, tail_core);
    let tail_wire = Wire {
        manifest_cbor: cbor::to_canonical_vec(&tail).unwrap(),
        metadata_blob: Vec::new(),
        ciphertext: create.ciphertext.clone(),
    };
    assert_eq!(
        ws.apply_remote_entry(entry(album, &tail_wire, Some(create_head)))
            .unwrap(),
        SyncApplyOutcome::Quarantined(QuarantineReason::Rejected(RejectReason::ForgedChain)),
        "nothing behind the stalled record applies"
    );
    assert_eq!(tail.prior_provenance_hash, Some(legacy_hash));

    // A device signature under no published key is still the plain forgery verdict.
    let mut forged = legacy.clone();
    forged.manifest.device_sig =
        capsule_core::crypto::keys::HybridSigningKey::from_seed_bytes(&[66; 32], &[67; 32])
            .sign(&forged.manifest.signing_bytes());
    let forged_wire = Wire {
        manifest_cbor: cbor::to_canonical_vec(&forged).unwrap(),
        metadata_blob: Vec::new(),
        ciphertext: create.ciphertext.clone(),
    };
    assert_eq!(
        ws.apply_remote_entry(entry(album, &forged_wire, Some(create_head)))
            .unwrap(),
        SyncApplyOutcome::Quarantined(QuarantineReason::Rejected(RejectReason::BadDeviceSig)),
    );
}

/// A record whose `asset_id` is not its manifest's `file_id` is a splice of two assets'
/// history. The manifest still verifies — it is genuinely signed — so nothing downstream
/// would catch it.
#[test]
fn a_record_naming_another_asset_is_quarantined() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (ws, album, _asset, wire) = seeded(&lib, &src);

    let mut record: ProvenanceRecord = cbor::from_slice(&wire.manifest_cbor).unwrap();
    record.asset_id = Uuid::now_v7();

    let spliced = Wire {
        manifest_cbor: cbor::to_canonical_vec(&record).unwrap(),
        metadata_blob: wire.metadata_blob.clone(),
        ciphertext: wire.ciphertext.clone(),
    };
    assert!(
        matches!(
            ws.apply_remote_entry(entry(album, &spliced, None)).unwrap(),
            SyncApplyOutcome::Quarantined(QuarantineReason::MalformedManifest(_))
        ),
        "a record must name the asset its manifest names"
    );
}

/// The restore read path: a verified entry opens to exactly the bytes that were imported.
#[test]
fn an_applied_entry_opens_to_the_imported_bytes() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (ws, album, asset, wire) = seeded(&lib, &src);

    match ws.open_remote_original(entry(album, &wire, None)).unwrap() {
        RemoteOriginal::Opened { facts, plaintext } => {
            assert_eq!(facts.asset_id, asset);
            assert_eq!(plaintext, ws.read_plaintext(&asset).unwrap());
        }
        other => panic!("expected the original to open, got {other:?}"),
    }
}

/// An entry that does not apply is handed back as its verdict, and nothing is decrypted:
/// bytes that are not the signed ciphertext, and an album this workspace holds no keys for.
#[test]
fn an_entry_that_does_not_apply_is_refused_without_decrypting() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (ws, album, _asset, wire) = seeded(&lib, &src);

    let mut flipped = wire.ciphertext.clone();
    flipped[0] ^= 0x01;
    let tampered = Wire {
        manifest_cbor: wire.manifest_cbor.clone(),
        metadata_blob: wire.metadata_blob.clone(),
        ciphertext: flipped,
    };
    assert_eq!(
        ws.open_remote_original(entry(album, &tampered, None))
            .unwrap(),
        RemoteOriginal::Refused(SyncApplyOutcome::Quarantined(QuarantineReason::Rejected(
            RejectReason::CiphertextHashMismatch
        ))),
    );

    let other_lib = TempDir::new().unwrap();
    let keyless = fast_workspace(other_lib.path());
    assert_eq!(
        keyless
            .open_remote_original(entry(album, &wire, None))
            .unwrap(),
        RemoteOriginal::Refused(SyncApplyOutcome::Quarantined(
            QuarantineReason::UnknownAlbum(album)
        )),
    );
}

/// A workspace holding one imported asset of `len` bytes — several STREAM chunks — plus its
/// wire bytes.
fn seeded_large(lib: &TempDir, src: &TempDir, len: usize) -> (Workspace, Uuid, Uuid, Wire) {
    let img = src.path().join("large.jpg");
    let mut bytes = b"\xFF\xD8\xFF".to_vec();
    bytes.extend((0..len).map(|i| (i as u8).wrapping_mul(13)));
    std::fs::write(&img, bytes).unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    let asset = ws.import_asset(album, &img).unwrap();
    let bundle = ws.upload_bundle(&asset).unwrap();
    let wire = Wire {
        manifest_cbor: bundle.provenance_blob.clone(),
        metadata_blob: bundle.metadata_blob.clone(),
        ciphertext: bundle.original.to_vec().unwrap(),
    };
    (ws, album, asset, wire)
}

/// A reader that records the largest read it was asked for.
struct WidestRead<R> {
    inner: R,
    widest: usize,
}

impl<R: Read> Read for WidestRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.widest = self.widest.max(buf.len());
        self.inner.read(buf)
    }
}

/// A writer that records the largest write it was handed.
#[derive(Debug, Default, PartialEq)]
struct WidestWrite {
    bytes: Vec<u8>,
    widest: usize,
}

impl Write for WidestWrite {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.widest = self.widest.max(buf.len());
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The streamed open: an original spanning several STREAM chunks is read and written one
/// chunk at a time, and the writer comes back holding exactly the imported bytes.
#[test]
fn a_streamed_original_opens_one_chunk_at_a_time() {
    use capsule_core::crypto::encryption::PLAINTEXT_CHUNK;

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (ws, album, asset, wire) = seeded_large(&lib, &src, 4 * PLAINTEXT_CHUNK + 17);
    let mut reader = WidestRead {
        inner: wire.ciphertext.as_slice(),
        widest: 0,
    };

    let opened = ws
        .open_remote_original_from(
            entry(album, &wire, None).head(),
            &mut reader,
            WidestWrite::default(),
        )
        .unwrap();
    let RemoteOriginal::Opened { facts, plaintext } = opened else {
        panic!("expected the original to open, got {opened:?}");
    };
    assert_eq!(facts.asset_id, asset);
    assert_eq!(plaintext.bytes, ws.read_plaintext(&asset).unwrap());
    assert!(
        reader.widest <= CIPHERTEXT_CHUNK,
        "read {} bytes at once",
        reader.widest
    );
    assert!(
        plaintext.widest <= PLAINTEXT_CHUNK,
        "wrote {} bytes at once",
        plaintext.widest
    );
}

/// The head's digest is a claim the stream must honour: bytes other than the committed
/// ciphertext are refused with the chokepoint's own verdict, whether a chunk fails
/// authentication first (a flipped byte) or the stream simply ends early (truncation).
#[test]
fn a_stream_that_is_not_the_named_ciphertext_is_refused() {
    use capsule_core::crypto::encryption::PLAINTEXT_CHUNK;

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (ws, album, _asset, wire) = seeded_large(&lib, &src, 3 * PLAINTEXT_CHUNK);
    let head = entry(album, &wire, None).head();
    let mismatch = RemoteOriginal::Refused(SyncApplyOutcome::Quarantined(
        QuarantineReason::Rejected(RejectReason::CiphertextHashMismatch),
    ));

    let mut flipped = wire.ciphertext.clone();
    flipped[CIPHERTEXT_CHUNK + 5] ^= 0x01;
    let truncated = &wire.ciphertext[..wire.ciphertext.len() - CIPHERTEXT_CHUNK];
    for (what, stream) in [("flipped", flipped.as_slice()), ("truncated", truncated)] {
        assert_eq!(
            ws.open_remote_original_from(head, stream, Vec::new())
                .unwrap(),
            mismatch,
            "{what}"
        );
    }
}

/// A head whose digest is not the manifest's is refused at the chokepoint, before a byte of
/// the stream is read.
#[test]
fn a_head_naming_other_bytes_is_refused_before_reading() {
    struct Unread;
    impl Read for Unread {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("a refused head reads nothing");
        }
    }

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (ws, album, _asset, wire) = seeded(&lib, &src);
    let mut head = entry(album, &wire, None).head();
    head.original_ciphertext_hash = hash_bytes(b"some other blob");

    assert_eq!(
        ws.open_remote_original_from(head, Unread, Vec::new())
            .unwrap(),
        RemoteOriginal::Refused(SyncApplyOutcome::Quarantined(QuarantineReason::Rejected(
            RejectReason::CiphertextHashMismatch
        ))),
    );
}

/// A record from a newer writer is quarantined under its own signal — never applied, never
/// reported as malformed, never an `Err` (versioning invariants 3 and 5).
#[test]
fn a_record_of_an_unknown_version_is_quarantined_not_applied() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (ws, album, _asset, wire) = seeded(&lib, &src);

    let value: ciborium::Value = cbor::from_slice(&wire.manifest_cbor).unwrap();
    let entries = value
        .into_map()
        .unwrap()
        .into_iter()
        .map(|(k, v)| {
            if k.as_text() == Some("version") {
                (k, ciborium::Value::from(2u16))
            } else {
                (k, v)
            }
        })
        .collect();
    let future = Wire {
        manifest_cbor: cbor::value_to_canonical_vec(&ciborium::Value::Map(entries)),
        metadata_blob: wire.metadata_blob.clone(),
        ciphertext: wire.ciphertext.clone(),
    };
    assert_eq!(
        ws.apply_remote_entry(entry(album, &future, None)).unwrap(),
        SyncApplyOutcome::Quarantined(QuarantineReason::UnsupportedRecordVersion {
            found: 2,
            max_known: capsule_core::crypto::provenance::PROVENANCE_RECORD_VERSION,
        })
    );
}

/// A newer writer's manifest that only adds a field applies here: the field is kept, signed
/// over, and the record's hash covers it, so the head a later op chains onto is the served
/// blob's digest.
#[test]
fn a_manifest_with_an_unknown_field_applies_and_keeps_it() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (ws, album, _asset, wire) = seeded(&lib, &src);

    let record = ProvenanceRecord::decode(&wire.manifest_cbor).unwrap();
    let mut core = record.manifest.core;
    core.unknown.insert(
        "future_field".into(),
        ciborium::Value::from("from a newer writer"),
    );
    let future = signed_record(&ws, album, core);
    let future_wire = Wire {
        manifest_cbor: cbor::to_canonical_vec(&future).unwrap(),
        metadata_blob: wire.metadata_blob.clone(),
        ciphertext: wire.ciphertext.clone(),
    };
    let facts = ws
        .apply_remote_entry(entry(album, &future_wire, None))
        .unwrap();
    let facts = facts
        .facts()
        .expect("an added field does not refuse the entry");
    assert_eq!(
        facts.provenance_head,
        capsule_core::crypto::hash::hash_bytes(&future_wire.manifest_cbor),
        "the head covers the preserved field"
    );
}

/// A head hashed from a stream is the head of the bytes: one spanning several read windows
/// hashes to what the bytes do and applies as the entry does, and a flipped byte refuses it
/// at the chokepoint's content-integrity step.
#[test]
fn a_head_hashed_from_a_stream_applies_as_the_entry_does() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (ws, album, _asset, wire) = seeded(&lib, &src);

    let long: Vec<u8> = (0..3 * CIPHERTEXT_CHUNK + 7)
        .map(|i| (i % 251) as u8)
        .collect();
    assert_eq!(
        hash_ciphertext_from(long.as_slice()).unwrap(),
        (hash_bytes(&long), long.len() as u64)
    );

    let (digest, len) = hash_ciphertext_from(wire.ciphertext.as_slice()).unwrap();
    assert_eq!(len, wire.ciphertext.len() as u64);
    let streamed = RemoteHead {
        original_ciphertext_hash: digest,
        ..entry(album, &wire, None).head()
    };
    assert_eq!(
        ws.apply_remote_head(streamed).unwrap(),
        ws.apply_remote_entry(entry(album, &wire, None)).unwrap()
    );

    let mut tampered = wire.ciphertext.clone();
    tampered[0] ^= 0xFF;
    let (digest, _) = hash_ciphertext_from(tampered.as_slice()).unwrap();
    assert_eq!(
        ws.apply_remote_head(RemoteHead {
            original_ciphertext_hash: digest,
            ..entry(album, &wire, None).head()
        })
        .unwrap(),
        SyncApplyOutcome::Quarantined(QuarantineReason::Rejected(
            RejectReason::CiphertextHashMismatch
        ))
    );
}
