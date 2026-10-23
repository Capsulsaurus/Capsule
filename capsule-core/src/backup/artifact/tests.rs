use super::*;
use crate::crypto::keys::{Amk, AmkVersion, HybridSigningKey};
use crate::crypto::primitives::PROTOCOL_VERSION;
use crate::crypto::provenance::{ASSET_MANIFEST_VERSION, Action, KeyMode, ManifestCore as MCore};

const ALBUM: u128 = 0xA1;

/// The wrap-key cost these tests export under. The artifact records it, so opening is just
/// as cheap; the wrap key's *strength* is orthogonal to the format, HMAC, signature and
/// reconciliation behaviour under test, and [`WRAP_PARAMS`] is exercised by the callers that
/// ship to users.
const FAST: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

struct Fix {
    device: HybridSigningKey,
    write: HybridSigningKey,
    amk: [u8; 32],
}

impl Fix {
    fn new() -> Self {
        Self {
            device: HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]),
            write: HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]),
            amk: [0x55; 32],
        }
    }

    /// Build a backup asset with a real STREAM ciphertext + create manifest.
    fn asset(&self, asset_id: u128, plaintext: &[u8]) -> BackupAsset {
        let amk = Amk::from_bytes(self.amk);
        let file_id = Uuid::from_u128(asset_id);
        // First write: draw the nonce and derive the folded file key together.
        let (enc, ct, _file_key) =
            crate::crypto::encryption::encrypt_asset_rekey(&amk, &file_id, plaintext, None)
                .unwrap();

        let core = MCore {
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
            created_by_user: Uuid::from_u128(0x05E2),
            created_by_device: Uuid::from_u128(0xD1),
            client_version: "t".into(),
            timestamp: "2026-05-31T00:00:00Z".into(),
            action: Action::Create,
            prior_provenance_hash: None,
            upgraded_from: None,
            unknown: Default::default(),
            retention_until: None,
        };
        let manifest = core.sign(&self.device, &self.write).unwrap();
        let record = ProvenanceRecord::new(file_id, manifest, None);
        BackupAsset {
            album_id: Uuid::from_u128(ALBUM),
            asset_id: file_id,
            ciphertext: Arc::new(ct),
            metadata_blob: crate::crypto::encryption::seal_metadata_blob(
                &amk,
                &file_id,
                b"{sidecar}",
                None,
            )
            .unwrap()
            .0,
            provenance: vec![record],
            receipts: Vec::new(),
            attestations: Vec::new(),
        }
    }

    /// Build a backup asset shaped like an adopted web-upload drop: STREAM-encrypted under
    /// a key a guest chose (`guest_key`), carried AMK-wrapped under `key_mode = wrapped`.
    fn wrapped_asset(&self, asset_id: u128, guest_key: [u8; 32], plaintext: &[u8]) -> BackupAsset {
        let amk = Amk::from_bytes(self.amk);
        let mut asset = self.asset(asset_id, plaintext);
        let mut core = asset.provenance[0].manifest.core.clone();
        let (enc, ct) = crate::crypto::encryption::encrypt_asset_vec_with_prefix(
            &guest_key,
            core.nonce_prefix,
            plaintext,
        );
        core.ciphertext_hash = enc.ciphertext_hash;
        core.key_mode = KeyMode::Wrapped;
        core.wrapped_file_key = Some(crate::crypto::provenance::WrappedFileKey(
            crate::crypto::encryption::seal_file_key(&amk, &core.file_id, &guest_key),
        ));
        asset.ciphertext = Arc::new(ct);
        asset.provenance[0].manifest = core.sign(&self.device, &self.write).unwrap();
        asset
    }

    /// The album's admin-signed epoch ledger attesting `self.write` for epoch 1.
    fn ledger(&self) -> SignedEpochLedger {
        let admin = HybridSigningKey::from_seed_bytes(&[5; 32], &[6; 32]);
        ReferenceAuthority::new(Uuid::from_u128(ALBUM), admin.verifying_key())
            .with_epoch(&admin, AmkVersion(1), &self.write.verifying_key(), true)
            .to_ledger()
    }

    fn input(&self, assets: Vec<BackupAsset>) -> BackupInput {
        let mut amks = BTreeMap::new();
        amks.insert((Uuid::from_u128(ALBUM), 1u32), self.amk);
        let mut authorities = BTreeMap::new();
        authorities.insert(Uuid::from_u128(ALBUM), self.ledger());
        BackupInput {
            assets,
            amks,
            authorities,
            docs: Vec::new(),
            attestation_key_pins: Vec::new(),
            exporter_device: Uuid::from_u128(0xD1),
            source_library_version: "1".into(),
            export_timestamp: "2026-05-31T00:00:00Z".into(),
        }
    }
}

#[test]
fn export_is_byte_identical_for_same_input_and_salt() {
    let f = Fix::new();
    let input = f.input(vec![f.asset(1, b"alpha"), f.asset(2, b"beta")]);
    let salt = [0x11; 32];
    let a = export_with_salt_and_params(&input, b"pw", salt, FAST, &f.device).unwrap();
    let b = export_with_salt_and_params(&input, b"pw", salt, FAST, &f.device).unwrap();
    assert_eq!(a, b, "deterministic export must be byte-identical");
}

#[test]
fn open_verifies_and_restore_to_fresh_library_recovers_plaintext() {
    let f = Fix::new();
    let input = f.input(vec![
        f.asset(1, b"hello world"),
        f.asset(2, b"second asset"),
    ]);
    let bytes = export_with_params(&input, b"pw", FAST, &f.device).unwrap();

    let art = BackupArtifact::open(&bytes, b"pw", &f.device.verifying_key()).unwrap();
    // Fresh library (no local heads) → everything applies.
    let report = art.restore(RestoreMode::Commit, &BTreeMap::new()).unwrap();
    assert_eq!(report.verified, 2);
    assert_eq!(report.would_add.len(), 2);
    assert_eq!(report.applied.len(), 2);

    let mut plaintexts: Vec<Vec<u8>> = report.applied.iter().map(|a| a.plaintext.clone()).collect();
    plaintexts.sort();
    assert_eq!(
        plaintexts,
        vec![b"hello world".to_vec(), b"second asset".to_vec()]
    );
}

/// #667: a sealed label record rides the artifact under the manifest's authentication and is
/// handed back with the escrowed AMK that opens it.
#[test]
fn label_records_ride_the_artifact_and_open_under_the_escrowed_amk() {
    let f = Fix::new();
    let album = Uuid::from_u128(ALBUM);
    let mut input = f.input(vec![f.asset(1, b"hello")]);
    let sealed = crate::crypto::encryption::seal_metadata_blob(
        &Amk::from_bytes(f.amk),
        &album,
        b"{album doc}",
        None,
    )
    .unwrap()
    .0;
    input.docs.push(BackupDoc {
        kind: BackupDocKind::Album,
        doc_id: album,
        album_id: album,
        amk_version: 1,
        sealed: sealed.clone(),
    });
    let bytes = export_with_params(&input, b"pw", FAST, &f.device).unwrap();

    let art = BackupArtifact::open(&bytes, b"pw", &f.device.verifying_key()).unwrap();
    assert_eq!(art.docs(), input.docs.as_slice());
    let amk = art.amk(&album, 1).expect("the doc's AMK is escrowed");
    let nonce = crate::crypto::encryption::blob_nonce(&sealed).unwrap();
    let key = Amk::from_bytes(amk).derive_blob_key(&album, &nonce);
    assert_eq!(
        crate::crypto::encryption::open_blob(&key, &sealed).unwrap(),
        b"{album doc}"
    );
}

/// A label record sealed under an epoch no asset uses still gets that epoch's AMK escrowed,
/// and an artifact without records opens with none.
#[test]
fn a_label_record_pulls_its_own_epoch_into_the_ledger() {
    let f = Fix::new();
    let album = Uuid::from_u128(ALBUM);
    let mut input = f.input(vec![f.asset(1, b"hello")]);
    let plain = export_with_params(&input, b"pw", FAST, &f.device).unwrap();
    let art = BackupArtifact::open(&plain, b"pw", &f.device.verifying_key()).unwrap();
    assert!(art.docs().is_empty());
    assert!(art.amk(&album, 2).is_none());

    input.amks.insert((album, 2), [0x66; 32]);
    input.docs.push(BackupDoc {
        kind: BackupDocKind::Stack,
        doc_id: Uuid::from_u128(0x51),
        album_id: album,
        amk_version: 2,
        sealed: vec![0; 40],
    });
    let bytes = export_with_params(&input, b"pw", FAST, &f.device).unwrap();
    let art = BackupArtifact::open(&bytes, b"pw", &f.device.verifying_key()).unwrap();
    assert_eq!(art.amk(&album, 2), Some([0x66; 32]));
    assert_eq!(art.docs()[0].kind, BackupDocKind::Stack);

    // A record whose epoch the exporter cannot supply refuses the export.
    input.amks.remove(&(album, 2));
    assert!(matches!(
        export_with_params(&input, b"pw", FAST, &f.device),
        Err(BackupError::AmkIncomplete(_))
    ));
}

/// A manifest core listing exactly `paths`, and the files those paths hold, each a doc entry
/// sealed (nominally) under `ALBUM`'s epoch `amk_version`.
fn doc_listing(paths: &[&str], amk_version: u32) -> (ManifestCore, BTreeMap<String, Vec<u8>>) {
    let data = cbor::to_canonical_vec(&DocEntry {
        album_id: Uuid::from_u128(ALBUM),
        amk_version,
        sealed: vec![0; 40],
    })
    .unwrap();
    let mut files = BTreeMap::new();
    let entries = paths
        .iter()
        .map(|path| {
            files.insert((*path).to_string(), data.clone());
            EntryRef {
                path: (*path).to_string(),
                hash: hash::hash_bytes(&data),
                size: data.len() as u64,
            }
        })
        .collect();
    let core = ManifestCore {
        artifact_version: ARTIFACT_FORMAT_VERSION,
        crypto_suite_id: CRYPTO_SUITE_ID,
        min_protocol_version: PROTOCOL_VERSION.into(),
        exporter_device: Uuid::from_u128(0xD1),
        export_timestamp: "2026-05-31T00:00:00Z".into(),
        source_library_version: "1".into(),
        entries,
        provenance_heads: Vec::new(),
    };
    (core, files)
}

/// `load_docs` refuses a listed `docs/` entry whose path does not name a doc id, and one
/// sealed under an epoch the ledger does not escrow; a well-formed entry
/// under an escrowed epoch loads.
#[test]
fn load_docs_refuses_malformed_paths_and_unescrowed_amks() {
    let ledger = BTreeMap::from([((Uuid::from_u128(ALBUM), 1u32), [0x55; 32])]);
    let good = format!("docs/album/{}", Uuid::from_u128(ALBUM));

    let (core, files) = doc_listing(&[&good], 1);
    let docs = load_docs(&core, &files, &ledger).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].kind, BackupDocKind::Album);

    let no_id = "docs/album".to_string();
    let bad_id = "docs/stack/not-a-uuid".to_string();
    for path in [no_id, bad_id] {
        let (core, files) = doc_listing(&[&path], 1);
        assert!(
            matches!(
                load_docs(&core, &files, &ledger),
                Err(BackupError::Format(ref m)) if m.contains(&path)
            ),
            "{path} must be refused"
        );
    }

    let (core, files) = doc_listing(&[&good], 2);
    assert!(matches!(
        load_docs(&core, &files, &ledger),
        Err(BackupError::AmkIncomplete(_))
    ));
}

/// #551: an adopted drop (`key_mode = wrapped`) restores. Its key is not
/// `HKDF(AMK, file_id ‖ nonce_prefix)`, so restore must unwrap `wrapped_file_key` under
/// the ledger AMK rather than re-derive; a derived asset alongside it still restores.
#[test]
fn a_wrapped_key_asset_round_trips_through_export_and_restore() {
    let f = Fix::new();
    let input = f.input(vec![
        f.asset(1, b"imported photo"),
        f.wrapped_asset(2, [0x77; 32], b"adopted guest photo"),
    ]);
    let bytes = export_with_params(&input, b"pw", FAST, &f.device).unwrap();
    let art = BackupArtifact::open(&bytes, b"pw", &f.device.verifying_key()).unwrap();
    let report = art.restore(RestoreMode::Commit, &BTreeMap::new()).unwrap();
    assert_eq!(report.applied.len(), 2);
    let adopted = report
        .applied
        .iter()
        .find(|a| a.asset_id == Uuid::from_u128(2))
        .unwrap();
    assert_eq!(adopted.plaintext, b"adopted guest photo");
    assert_eq!(
        adopted.provenance[0].manifest.core.key_mode,
        KeyMode::Wrapped
    );
}

/// A `wrapped` head whose key does not open under the ledger AMK fails restore closed.
#[test]
fn a_wrapped_key_sealed_under_another_amk_fails_restore() {
    let f = Fix::new();
    let mut asset = f.wrapped_asset(2, [0x77; 32], b"adopted guest photo");
    let mut core = asset.provenance[0].manifest.core.clone();
    core.wrapped_file_key = Some(crate::crypto::provenance::WrappedFileKey(
        crate::crypto::encryption::seal_file_key(
            &Amk::from_bytes([0x99; 32]),
            &core.file_id,
            &[0x77; 32],
        ),
    ));
    asset.provenance[0].manifest = core.sign(&f.device, &f.write).unwrap();
    let bytes = export_with_params(&f.input(vec![asset]), b"pw", FAST, &f.device).unwrap();
    let art = BackupArtifact::open(&bytes, b"pw", &f.device.verifying_key()).unwrap();
    assert!(matches!(
        art.restore(RestoreMode::DryRun, &BTreeMap::new()),
        Err(BackupError::Auth("asset file key recovery failed"))
    ));
}

#[test]
fn custody_receipts_ride_the_artifact_and_restore_verbatim() {
    // The custody-receipt log is evidence, so it is included in the backup artifact
    // verbatim and comes back byte-identical on restore.
    let f = Fix::new();
    let mut with_receipts = f.asset(1, b"has receipts");
    with_receipts.receipts = b"receipt-log-cbor-bytes".to_vec();
    let plain = f.asset(2, b"no receipts"); // absent = no entry emitted
    let input = f.input(vec![with_receipts, plain]);
    let bytes = export_with_params(&input, b"pw", FAST, &f.device).unwrap();

    let art = BackupArtifact::open(&bytes, b"pw", &f.device.verifying_key()).unwrap();
    let report = art.restore(RestoreMode::Commit, &BTreeMap::new()).unwrap();
    let a1 = report
        .applied
        .iter()
        .find(|a| a.asset_id == Uuid::from_u128(1))
        .unwrap();
    assert_eq!(a1.receipts, b"receipt-log-cbor-bytes");
    let a2 = report
        .applied
        .iter()
        .find(|a| a.asset_id == Uuid::from_u128(2))
        .unwrap();
    assert!(a2.receipts.is_empty(), "no receipt log → no entry");
}

#[test]
fn storage_attestations_ride_the_artifact_and_restore_verbatim() {
    // The attestation log is evidence like the receipt log: carried as its own manifest
    // entry, hash-checked on open, and returned byte for byte on restore.
    let f = Fix::new();
    let mut with_log = f.asset(1, b"has attestations");
    with_log.attestations = b"attestation-log-cbor-bytes".to_vec();
    let plain = f.asset(2, b"no attestations");
    let bytes =
        export_with_params(&f.input(vec![with_log, plain]), b"pw", FAST, &f.device).unwrap();

    let art = BackupArtifact::open(&bytes, b"pw", &f.device.verifying_key()).unwrap();
    let paths: Vec<&str> = art.core.entries.iter().map(|e| e.path.as_str()).collect();
    assert!(paths.contains(&format!("attestations/{}", Uuid::from_u128(1)).as_str()));
    assert!(
        !paths.contains(&format!("attestations/{}", Uuid::from_u128(2)).as_str()),
        "no attestation log → no entry"
    );

    let report = art.restore(RestoreMode::Commit, &BTreeMap::new()).unwrap();
    let restored = |id: u128| {
        report
            .applied
            .iter()
            .find(|a| a.asset_id == Uuid::from_u128(id))
            .unwrap()
    };
    assert_eq!(restored(1).attestations, b"attestation-log-cbor-bytes");
    assert!(restored(2).attestations.is_empty());
}

#[test]
fn a_tampered_attestation_log_fails_the_open() {
    // The log is a manifest entry, so a byte changed in transit is caught by its hash
    // before anything is restored.
    let f = Fix::new();
    let mut asset = f.asset(1, b"evidence");
    asset.attestations = b"attestation-log".to_vec();
    let bytes = export_with_params(&f.input(vec![asset]), b"pw", FAST, &f.device).unwrap();
    let path = format!("attestations/{}", Uuid::from_u128(1));
    let mut builder = tar::Builder::new(Vec::new());
    for (entry, data) in tar_read(&bytes).unwrap() {
        let data = if entry == path {
            b"attestation-loG".to_vec()
        } else {
            data
        };
        tar_append(&mut builder, &entry, &data).unwrap();
    }
    let tampered = builder.into_inner().unwrap();

    assert!(matches!(
        BackupArtifact::open(&tampered, b"pw", &f.device.verifying_key()),
        Err(BackupError::Corrupt(p)) if p == path
    ));
}

#[test]
fn the_attestation_key_pin_rides_the_artifact_verbatim() {
    let f = Fix::new();
    let mut input = f.input(vec![f.asset(1, b"pinned")]);
    input.attestation_key_pins = b"attestation-key-pin-cbor".to_vec();
    let bytes = export_with_params(&input, b"pw", FAST, &f.device).unwrap();

    let art = BackupArtifact::open(&bytes, b"pw", &f.device.verifying_key()).unwrap();

    assert!(
        art.core
            .entries
            .iter()
            .any(|e| e.path == ATTESTATION_KEY_PINS_ENTRY),
        "the pin is a manifest entry"
    );
    assert_eq!(
        art.attestation_key_pins(),
        Some(&b"attestation-key-pin-cbor"[..])
    );
    // The assets restore exactly as they would without it.
    let report = art.restore(RestoreMode::Commit, &BTreeMap::new()).unwrap();
    assert_eq!(report.applied.len(), 1);
}

#[test]
fn an_artifact_without_a_pin_carries_none() {
    let f = Fix::new();
    let bytes = export_with_params(
        &f.input(vec![f.asset(1, b"unpinned")]),
        b"pw",
        FAST,
        &f.device,
    )
    .unwrap();

    let art = BackupArtifact::open(&bytes, b"pw", &f.device.verifying_key()).unwrap();

    assert!(
        !art.core
            .entries
            .iter()
            .any(|e| e.path == ATTESTATION_KEY_PINS_ENTRY),
        "no pin → no entry"
    );
    assert_eq!(art.attestation_key_pins(), None);
    let report = art.restore(RestoreMode::Commit, &BTreeMap::new()).unwrap();
    assert_eq!(report.applied.len(), 1);
}

#[test]
fn a_pin_the_manifest_does_not_list_is_ignored() {
    // Only the authenticated manifest names what the artifact carries: a pin file slipped
    // into the tar after export is never handed to a restore.
    let f = Fix::new();
    let bytes = export_with_params(
        &f.input(vec![f.asset(1, b"unpinned")]),
        b"pw",
        FAST,
        &f.device,
    )
    .unwrap();
    let mut builder = tar::Builder::new(Vec::new());
    for (entry, data) in tar_read(&bytes).unwrap() {
        tar_append(&mut builder, &entry, &data).unwrap();
    }
    tar_append(&mut builder, ATTESTATION_KEY_PINS_ENTRY, b"planted").unwrap();
    let planted = builder.into_inner().unwrap();

    let art = BackupArtifact::open(&planted, b"pw", &f.device.verifying_key()).unwrap();

    assert_eq!(art.attestation_key_pins(), None);
}

#[test]
fn a_tampered_pin_fails_the_open() {
    let f = Fix::new();
    let mut input = f.input(vec![f.asset(1, b"pinned")]);
    input.attestation_key_pins = b"attestation-key-pin".to_vec();
    let bytes = export_with_params(&input, b"pw", FAST, &f.device).unwrap();
    let mut builder = tar::Builder::new(Vec::new());
    for (entry, data) in tar_read(&bytes).unwrap() {
        let data = if entry == ATTESTATION_KEY_PINS_ENTRY {
            b"attestation-key-piN".to_vec()
        } else {
            data
        };
        tar_append(&mut builder, &entry, &data).unwrap();
    }
    let tampered = builder.into_inner().unwrap();

    assert!(matches!(
        BackupArtifact::open(&tampered, b"pw", &f.device.verifying_key()),
        Err(BackupError::Corrupt(p)) if p == ATTESTATION_KEY_PINS_ENTRY
    ));
}

#[test]
fn wrong_passphrase_fails_to_open() {
    let f = Fix::new();
    let bytes =
        export_with_params(&f.input(vec![f.asset(1, b"x")]), b"right", FAST, &f.device).unwrap();
    assert!(BackupArtifact::open(&bytes, b"wrong", &f.device.verifying_key()).is_err());
}

#[test]
fn tampering_an_entry_is_detected() {
    let f = Fix::new();
    let bytes =
        export_with_params(&f.input(vec![f.asset(1, b"x")]), b"pw", FAST, &f.device).unwrap();
    // Flip a byte somewhere in the archive body (a blob) → entry-hash or HMAC mismatch.
    let mut t = bytes.clone();
    let mid = t.len() / 2;
    t[mid] ^= 0x01;
    assert!(BackupArtifact::open(&t, b"pw", &f.device.verifying_key()).is_err());
}

#[test]
fn wrong_exporter_key_is_rejected() {
    let f = Fix::new();
    let bytes =
        export_with_params(&f.input(vec![f.asset(1, b"x")]), b"pw", FAST, &f.device).unwrap();
    let imposter = HybridSigningKey::from_seed_bytes(&[9; 32], &[9; 32]).verifying_key();
    assert!(BackupArtifact::open(&bytes, b"pw", &imposter).is_err());
}

#[test]
fn amk_incomplete_is_detected_at_export() {
    let f = Fix::new();
    // Build input whose ledger omits the needed AMK.
    let mut input = f.input(vec![f.asset(1, b"x")]);
    input.amks.clear();
    assert!(matches!(
        export_with_params(&input, b"pw", FAST, &f.device),
        Err(BackupError::AmkIncomplete(_))
    ));
}

#[test]
fn restore_reconciliation_matrix() {
    let f = Fix::new();
    let asset = f.asset(1, b"content");
    let head = asset.provenance.last().unwrap().record_hash();
    let asset_id = asset.asset_id;
    let bytes = export_with_params(&f.input(vec![asset]), b"pw", FAST, &f.device).unwrap();
    let art = BackupArtifact::open(&bytes, b"pw", &f.device.verifying_key()).unwrap();

    // Identical local head → no-op.
    let mut heads = BTreeMap::new();
    heads.insert(asset_id, head);
    let r = art.restore(RestoreMode::DryRun, &heads).unwrap();
    assert_eq!(r.identical, vec![asset_id]);
    assert!(r.applied.is_empty());

    // Divergent local head → conflict, not applied.
    let mut heads = BTreeMap::new();
    heads.insert(asset_id, Hash32([0xEE; 32]));
    let r = art.restore(RestoreMode::Commit, &heads).unwrap();
    assert_eq!(r.conflicts, vec![asset_id]);
    assert!(
        r.applied.is_empty(),
        "never silently overwrite divergent local state"
    );

    // Absent locally → applied.
    let r = art.restore(RestoreMode::Commit, &BTreeMap::new()).unwrap();
    assert_eq!(r.would_add, vec![asset_id]);
    assert_eq!(r.applied.len(), 1);
}

/// #468: each included album's public authority rides the artifact and comes back out of
/// `open` re-verified, so a restored asset has a ledger to verify its write signature under.
#[test]
fn the_album_authority_rides_the_artifact_and_reverifies_on_open() {
    let f = Fix::new();
    let bytes =
        export_with_params(&f.input(vec![f.asset(1, b"x")]), b"pw", FAST, &f.device).unwrap();
    let art = BackupArtifact::open(&bytes, b"pw", &f.device.verifying_key()).unwrap();

    let ledgers = art.authority_ledgers();
    assert_eq!(ledgers.len(), 1);
    let ledger = &ledgers[&Uuid::from_u128(ALBUM)];
    assert_eq!(ledger.admin_pub, f.ledger().admin_pub);
    let authority = ReferenceAuthority::from_ledger(ledger, &BTreeSet::from([1])).unwrap();
    assert_eq!(
        crate::crypto::authority::AlbumAuthority::write_tier_pubkey(&authority, AmkVersion(1)),
        Some(f.write.verifying_key()),
        "the attested write-tier key is the one the manifest was signed under"
    );
}

/// An album with no authority to hand (a format-1 recovery re-exported) still exports; it
/// simply carries no `authority/` entry.
#[test]
fn an_album_without_an_authority_exports_without_one() {
    let f = Fix::new();
    let mut input = f.input(vec![f.asset(1, b"x")]);
    input.authorities.clear();
    let bytes = export_with_params(&input, b"pw", FAST, &f.device).unwrap();
    let art = BackupArtifact::open(&bytes, b"pw", &f.device.verifying_key()).unwrap();
    assert!(art.authority_ledgers().is_empty());
    assert_eq!(
        art.restore(RestoreMode::DryRun, &BTreeMap::new())
            .unwrap()
            .verified,
        1
    );
}

/// A ledger whose admin chain does not verify is refused at `open` even when the exporter
/// signed the manifest over it: the exporter vouches for the bytes, not for the album admin.
#[test]
fn a_forged_authority_ledger_is_refused_on_open() {
    let f = Fix::new();
    let mut input = f.input(vec![f.asset(1, b"x")]);
    let ledger = input.authorities.get_mut(&Uuid::from_u128(ALBUM)).unwrap();
    ledger.entries[0].write_tier_pub =
        HybridSigningKey::from_seed_bytes(&[9; 32], &[9; 32]).verifying_key();
    let bytes = export_with_params(&input, b"pw", FAST, &f.device).unwrap();
    assert!(matches!(
        BackupArtifact::open(&bytes, b"pw", &f.device.verifying_key()),
        Err(BackupError::Auth(_))
    ));
}

/// Re-export `bytes` with its manifest core and file paths edited by `edit`, then
/// re-authenticated (HMAC and exporter signature) exactly as an exporter would — so `open`
/// gets past authentication and the check under test is the one that refuses.
fn reauthenticated(
    f: &Fix,
    bytes: &[u8],
    edit: impl FnOnce(&mut ManifestCore, &mut Vec<(String, Vec<u8>)>),
) -> Vec<u8> {
    let mut files = tar_read(bytes).unwrap();
    let (salt, params) = parse_version(&files[0].1).unwrap();
    let wrap_key = pwkdf::derive_wrap_key(b"pw", &salt, params).unwrap();

    let mut manifest: Manifest = cbor::from_slice(&files[1].1).unwrap();
    edit(&mut manifest.core, &mut files);
    let core_bytes = cbor::to_canonical_vec(&manifest.core).unwrap();
    let mut mac = HmacSha256::new_from_slice(&wrap_key).unwrap();
    mac.update(&core_bytes);
    manifest.hmac = mac.finalize().into_bytes().to_vec();
    manifest.exporter_sig = f.device.sign(&core_bytes);

    let mut builder = tar::Builder::new(Vec::new());
    for (path, data) in &files {
        if path == "MANIFEST.cbor" {
            tar_append(
                &mut builder,
                path,
                &cbor::to_canonical_vec(&manifest).unwrap(),
            )
            .unwrap();
        } else {
            tar_append(&mut builder, path, data).unwrap();
        }
    }
    builder.into_inner().unwrap()
}

/// Rename the `authority/{ALBUM}` entry to `authority/{to}` in both the manifest and the
/// archive, leaving the ledger bytes (and so their hash) untouched.
fn refile_authority(core: &mut ManifestCore, files: &mut [(String, Vec<u8>)], to: &str) {
    let from = format!("authority/{}", Uuid::from_u128(ALBUM));
    let to = format!("authority/{to}");
    let entry = core.entries.iter_mut().find(|e| e.path == from).unwrap();
    entry.path.clone_from(&to);
    let file = files.iter_mut().find(|(p, _)| *p == from).unwrap();
    file.0 = to;
}

/// An authentic manifest declaring a newer format than this build is refused, not half-read.
#[test]
fn a_newer_artifact_format_is_refused() {
    let f = Fix::new();
    let bytes =
        export_with_params(&f.input(vec![f.asset(1, b"x")]), b"pw", FAST, &f.device).unwrap();
    let newer = reauthenticated(&f, &bytes, |core, _| {
        core.artifact_version = ARTIFACT_FORMAT_VERSION + 1;
    });
    assert!(matches!(
        BackupArtifact::open(&newer, b"pw", &f.device.verifying_key()),
        Err(BackupError::Format(msg)) if msg.contains("newer")
    ));
}

/// An authenticated `authority/{album}` entry whose ledger speaks for another album is refused
/// at `open`: the exporter's signature covers the bytes, not the filing.
#[test]
fn an_authenticated_ledger_filed_under_another_album_is_refused_on_open() {
    let f = Fix::new();
    let bytes =
        export_with_params(&f.input(vec![f.asset(1, b"x")]), b"pw", FAST, &f.device).unwrap();
    let misfiled = reauthenticated(&f, &bytes, |core, files| {
        refile_authority(core, files, &Uuid::from_u128(0xBAD).to_string());
    });
    assert!(matches!(
        BackupArtifact::open(&misfiled, b"pw", &f.device.verifying_key()),
        Err(BackupError::Auth(msg)) if msg.contains("another album")
    ));
}

/// An authenticated authority entry whose name is not an album UUID is refused at `open`.
#[test]
fn an_authority_entry_not_named_by_a_uuid_is_refused_on_open() {
    let f = Fix::new();
    let bytes =
        export_with_params(&f.input(vec![f.asset(1, b"x")]), b"pw", FAST, &f.device).unwrap();
    let unnamed = reauthenticated(&f, &bytes, |core, files| {
        refile_authority(core, files, "not-a-uuid");
    });
    assert!(matches!(
        BackupArtifact::open(&unnamed, b"pw", &f.device.verifying_key()),
        Err(BackupError::Format(msg)) if msg.contains("bad authority entry")
    ));
}

/// A ledger handed in under the wrong album id is refused at export.
#[test]
fn a_ledger_filed_under_another_album_is_refused_at_export() {
    let f = Fix::new();
    let mut input = f.input(vec![f.asset(1, b"x")]);
    let ledger = input.authorities.get_mut(&Uuid::from_u128(ALBUM)).unwrap();
    ledger.album_id = Uuid::from_u128(0xBAD);
    assert!(matches!(
        export_with_params(&input, b"pw", FAST, &f.device),
        Err(BackupError::Format(_))
    ));
}

#[test]
fn dry_run_writes_nothing() {
    let f = Fix::new();
    let bytes =
        export_with_params(&f.input(vec![f.asset(1, b"x")]), b"pw", FAST, &f.device).unwrap();
    let art = BackupArtifact::open(&bytes, b"pw", &f.device.verifying_key()).unwrap();
    let r = art.restore(RestoreMode::DryRun, &BTreeMap::new()).unwrap();
    // DryRun verifies (decrypts) but returns nothing to write.
    assert_eq!(r.verified, 1);
    assert!(r.applied.is_empty());
}

/// A source that pushes its bytes in small pieces and may declare a size it does not hold —
/// the shape of a streamed original, with the faults a source can have.
#[derive(Debug)]
struct Trickle {
    bytes: Vec<u8>,
    declared: u64,
}

impl Trickle {
    fn honest(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.to_vec(),
            declared: bytes.len() as u64,
        }
    }
}

impl CiphertextSource for Trickle {
    fn size(&self) -> u64 {
        self.declared
    }

    fn write_to(&self, writer: &mut dyn Write) -> io::Result<Hash32> {
        for piece in self.bytes.chunks(7) {
            writer.write_all(piece)?;
        }
        Ok(hash::hash_bytes(&self.bytes))
    }
}

/// The artifact's bytes do not depend on whether an entry body was held or streamed: a
/// streamed entry is the buffered entry, header, body and padding, at every padding
/// boundary.
#[test]
fn a_streamed_entry_is_byte_identical_to_a_buffered_one() {
    for len in [0, 1, 511, 512, 513, 1024, 70_000] {
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let path = format!("blobs/{}", hash::hash_bytes(&data).to_hex());

        let mut buffered = tar::Builder::new(Vec::new());
        tar_append(&mut buffered, &path, &data).unwrap();
        tar_append(&mut buffered, "meta/next", b"after").unwrap();

        let mut streamed = tar::Builder::new(Vec::new());
        tar_append_stream(
            &mut streamed,
            &path,
            &Trickle::honest(&data),
            hash::hash_bytes(&data),
        )
        .unwrap();
        tar_append(&mut streamed, "meta/next", b"after").unwrap();

        assert_eq!(
            streamed.into_inner().unwrap(),
            buffered.into_inner().unwrap(),
            "entry of {len} bytes"
        );
    }
}

/// Exporting from a streamed source writes the same artifact as exporting the same bytes
/// from memory, and it opens and restores.
#[test]
fn an_artifact_streamed_from_its_sources_matches_one_exported_from_memory() {
    let f = Fix::new();
    let held = vec![f.asset(1, b"alpha"), f.asset(2, &[0x5A; 200_000])];
    let streamed: Vec<BackupAsset> = held
        .iter()
        .map(|a| {
            let mut bytes = Vec::new();
            a.ciphertext.write_to(&mut bytes).unwrap();
            BackupAsset {
                ciphertext: Arc::new(Trickle::honest(&bytes)),
                ..a.clone()
            }
        })
        .collect();
    let salt = [9; 32];
    let from_memory =
        export_with_salt_and_params(&f.input(held), b"pw", salt, FAST, &f.device).unwrap();
    let mut from_sources = Vec::new();
    let written = export_inner(
        &f.input(streamed),
        b"pw",
        salt,
        FAST,
        &f.device,
        &mut from_sources,
    )
    .unwrap();

    assert_eq!(written, from_sources.len() as u64);
    // The exporter signature is hybrid and may be randomized, so compare everything it does
    // not cover byte for byte, and require both to open.
    let tar_entries = |bytes: &[u8]| {
        tar_read(bytes)
            .unwrap()
            .into_iter()
            .filter(|(path, _)| path != "MANIFEST.cbor")
            .collect::<Vec<_>>()
    };
    assert_eq!(tar_entries(&from_sources), tar_entries(&from_memory));
    let art = BackupArtifact::open(&from_sources, b"pw", &f.device.verifying_key()).unwrap();
    let r = art.restore(RestoreMode::Commit, &BTreeMap::new()).unwrap();
    assert_eq!(r.applied.len(), 2);
}

/// A source whose bytes are not the ones its manifest commits to fails the export, rather
/// than writing an artifact whose MANIFEST names an address its blob does not have.
#[test]
fn a_source_that_disagrees_with_its_manifest_fails_the_export() {
    let f = Fix::new();
    let good = f.asset(1, b"alpha");
    let mut bytes = Vec::new();
    good.ciphertext.write_to(&mut bytes).unwrap();

    let mut flipped = bytes.clone();
    flipped[0] ^= 1;
    let short = Trickle {
        bytes: bytes[..bytes.len() - 1].to_vec(),
        declared: bytes.len() as u64,
    };
    for source in [
        Arc::new(Trickle::honest(&flipped)) as Arc<dyn CiphertextSource>,
        Arc::new(short),
    ] {
        let asset = BackupAsset {
            ciphertext: source,
            ..good.clone()
        };
        assert!(matches!(
            export_with_params(&f.input(vec![asset]), b"pw", FAST, &f.device),
            Err(BackupError::Corrupt(_))
        ));
    }
}
