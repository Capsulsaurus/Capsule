//! The backup artifact: a deterministic, self-describing, signed tar container
//! (SSoT: [Backup — Backup Artifact]).
//!
//! Layout (entries after the header are sorted by `(album_id, asset_id, blob_role)`):
//! ```text
//! VERSION                 # plaintext: format version, crypto_suite_id, wrap salt + params
//! MANIFEST.cbor           # entry list/hashes + provenance heads; HMAC + hybrid exporter sig
//! keys/amk-ledger.cbor    # the AMKs needed, sealed under the passphrase-derived wrap key
//! blobs/{ciphertext_hash} # encrypted asset ciphertext
//! meta/{asset_id}         # encrypted metadata blob
//! provenance/{asset_id}   # the full per-asset provenance chain (canonical CBOR)
//! ```
//!
//! The MANIFEST is authenticated **two ways**: an HMAC under the wrap key (catches
//! tamper/truncation before any decrypt) and a hybrid exporter signature (defeats a
//! wrap-key thief who could otherwise re-HMAC). Restore is a **chain reconciliation**, never
//! a blind overwrite, and **dry-run is the default**.
//!
//! [Backup — Backup Artifact]: https://docs/design/backup-recovery/#backup-artifact

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

use super::{ARTIFACT_FORMAT_VERSION, BackupError};
use crate::cbor;
use crate::crypto::encryption::stream;
use crate::crypto::hash::{self, Hash32};
use crate::crypto::keys::{Amk, HybridVerifyingKey, Signer};
use crate::crypto::primitives::{Argon2Params, CRYPTO_SUITE_ID, PROTOCOL_VERSION, info};
use crate::crypto::provenance::ProvenanceRecord;
use crate::crypto::{kdf, pwkdf, rng};

type HmacSha256 = Hmac<Sha256>;

/// Argon2id params for the backup wrap key, recorded in VERSION so restore reproduces the
/// key. Production uses the normal-tier cost; tests use a trivially-fast cost (the wrap-key
/// strength is orthogonal to the format/round-trip correctness the tests exercise).
#[cfg(not(test))]
const WRAP_PARAMS: Argon2Params = Argon2Params {
    mem_kib: 256 * 1024,
    t_cost: 3,
    p_cost: 1,
};
#[cfg(test)]
const WRAP_PARAMS: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// One asset to back up: its ciphertext, metadata blob, and full provenance chain.
#[derive(Debug, Clone)]
pub struct BackupAsset {
    /// Album the asset belongs to.
    pub album_id: Uuid,
    /// The asset id.
    pub asset_id: Uuid,
    /// STREAM ciphertext blob.
    pub ciphertext: Vec<u8>,
    /// Encrypted metadata blob (wire format).
    pub metadata_blob: Vec<u8>,
    /// The asset's provenance chain (oldest first).
    pub provenance: Vec<ProvenanceRecord>,
}

impl BackupAsset {
    fn head(&self) -> Result<&crate::crypto::provenance::AssetManifest, BackupError> {
        self.provenance
            .last()
            .map(|r| &r.manifest)
            .ok_or(BackupError::Auth("asset has empty provenance chain"))
    }
}

/// Everything needed to assemble an artifact.
pub struct BackupInput {
    /// The assets to include.
    pub assets: Vec<BackupAsset>,
    /// The AMK bytes for every `(album_id, amk_version)` an asset references.
    pub amks: BTreeMap<(Uuid, u32), [u8; 32]>,
    /// The exporting device id.
    pub exporter_device: Uuid,
    /// Source library version string.
    pub source_library_version: String,
    /// RFC3339 export timestamp.
    pub export_timestamp: String,
}

/// A manifest entry: a path and the SHA-256 + size of its content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EntryRef {
    path: String,
    hash: Hash32,
    size: u64,
}

/// The signed core of the backup MANIFEST.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ManifestCore {
    artifact_version: u16,
    crypto_suite_id: u16,
    min_protocol_version: String,
    exporter_device: Uuid,
    export_timestamp: String,
    source_library_version: String,
    entries: Vec<EntryRef>,
    provenance_heads: Vec<(Uuid, Hash32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    core: ManifestCore,
    #[serde(with = "serde_bytes")]
    hmac: Vec<u8>,
    exporter_sig: crate::crypto::keys::HybridSignature,
}

/// The AMK ledger plaintext: every AMK needed to decrypt the included assets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
struct AmkLedger {
    /// `(album_id, amk_version) -> amk bytes`.
    entries: Vec<(Uuid, u32, [u8; 32])>,
}

fn blob_role_order(role: &str) -> u8 {
    match role {
        "blobs" => 0,
        "meta" => 1,
        "provenance" => 2,
        _ => 3,
    }
}

// ── tar I/O (deterministic) ─────────────────────────────────────────────────

fn tar_append(builder: &mut tar::Builder<Vec<u8>>, path: &str, data: &[u8]) {
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_mtime(0);
    h.set_uid(0);
    h.set_gid(0);
    h.set_entry_type(tar::EntryType::Regular);
    builder
        .append_data(&mut h, path, data)
        .expect("tar append is infallible for an in-memory writer");
}

fn tar_read(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, BackupError> {
    let mut archive = tar::Archive::new(bytes);
    let mut out = Vec::new();
    for entry in archive
        .entries()
        .map_err(|e| BackupError::Format(e.to_string()))?
    {
        let mut e = entry.map_err(|e| BackupError::Format(e.to_string()))?;
        let path = e
            .path()
            .map_err(|e| BackupError::Format(e.to_string()))?
            .to_string_lossy()
            .into_owned();
        let mut buf = Vec::new();
        e.read_to_end(&mut buf)
            .map_err(|e| BackupError::Format(e.to_string()))?;
        out.push((path, buf));
    }
    Ok(out)
}

fn version_blob(salt: &[u8; 32]) -> Vec<u8> {
    format!(
        "artifact_format={ARTIFACT_FORMAT_VERSION}\ncrypto_suite_id={CRYPTO_SUITE_ID}\nmin_protocol_version={PROTOCOL_VERSION}\nwrap_salt={}\nwrap_mem_kib={}\nwrap_t={}\nwrap_p={}\n",
        hex::encode(salt),
        WRAP_PARAMS.mem_kib,
        WRAP_PARAMS.t_cost,
        WRAP_PARAMS.p_cost,
    )
    .into_bytes()
}

fn parse_version(blob: &[u8]) -> Result<([u8; 32], Argon2Params), BackupError> {
    let text =
        std::str::from_utf8(blob).map_err(|_| BackupError::Format("VERSION not utf8".into()))?;
    let mut salt = None;
    let (mut mem, mut t, mut p) = (None, None, None);
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            match k {
                "wrap_salt" => {
                    salt = Hash32::from_hex(v).ok().map(|h| h.0);
                }
                "wrap_mem_kib" => mem = v.parse().ok(),
                "wrap_t" => t = v.parse().ok(),
                "wrap_p" => p = v.parse().ok(),
                _ => {}
            }
        }
    }
    Ok((
        salt.ok_or(BackupError::Format("VERSION missing wrap_salt".into()))?,
        Argon2Params {
            mem_kib: mem.ok_or(BackupError::Format("VERSION missing wrap_mem_kib".into()))?,
            t_cost: t.ok_or(BackupError::Format("VERSION missing wrap_t".into()))?,
            p_cost: p.ok_or(BackupError::Format("VERSION missing wrap_p".into()))?,
        },
    ))
}

/// Seal the AMK ledger under the wrap key with a deterministic (key-derived) nonce, so a
/// re-export with the same salt+content is byte-identical. Production uses a fresh random
/// salt per export, so the (key, nonce) pair never repeats across distinct plaintexts.
fn seal_ledger(wrap_key: &[u8; 32], ledger: &AmkLedger) -> Vec<u8> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Key, Nonce};
    let plaintext = cbor::to_canonical_vec(ledger).expect("ledger serializes");
    let nonce_bytes = kdf::derive_key32(wrap_key, b"amk-ledger-nonce", info::METADATA_BLOB_V1);
    let nonce = &nonce_bytes[..12];
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(wrap_key));
    cipher
        .encrypt(Nonce::from_slice(nonce), plaintext.as_slice())
        .expect("ledger seal")
}

fn open_ledger(wrap_key: &[u8; 32], sealed: &[u8]) -> Result<AmkLedger, BackupError> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Key, Nonce};
    let nonce_bytes = kdf::derive_key32(wrap_key, b"amk-ledger-nonce", info::METADATA_BLOB_V1);
    let nonce = &nonce_bytes[..12];
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(wrap_key));
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), sealed)
        .map_err(|_| BackupError::Auth("AMK ledger decryption failed"))?;
    cbor::from_slice(&plaintext).map_err(|e| BackupError::Format(e.to_string()))
}

// ── Export ──────────────────────────────────────────────────────────────────

/// Assemble a backup artifact with an explicit wrap salt (deterministic; used by tests).
pub fn export_with_salt(
    input: &BackupInput,
    passphrase: &[u8],
    salt: [u8; 32],
    exporter: &dyn Signer,
) -> Result<Vec<u8>, BackupError> {
    let wrap_key = pwkdf::derive_wrap_key(passphrase, &salt, WRAP_PARAMS)?;

    // Build the AMK ledger, asserting completeness for every referenced epoch.
    let mut ledger = AmkLedger::default();
    let mut needed: BTreeSet<(Uuid, u32)> = BTreeSet::new();
    for a in &input.assets {
        let head = a.head()?;
        needed.insert((a.album_id, head.core.amk_version.0));
    }
    for (album, epoch) in &needed {
        let amk = input
            .amks
            .get(&(*album, *epoch))
            .ok_or_else(|| BackupError::AmkIncomplete(format!("album {album} epoch {epoch}")))?;
        ledger.entries.push((*album, *epoch, *amk));
    }
    ledger.entries.sort();
    let sealed_ledger = seal_ledger(&wrap_key, &ledger);

    // Sort assets deterministically by (album_id, asset_id).
    let mut assets: Vec<&BackupAsset> = input.assets.iter().collect();
    assets.sort_by_key(|a| (a.album_id, a.asset_id));

    // Collect entries (sorted by album, asset, then blob role) and provenance heads.
    let mut entries: Vec<EntryRef> = Vec::new();
    let mut payloads: Vec<(String, Vec<u8>)> = Vec::new();
    let mut provenance_heads: Vec<(Uuid, Hash32)> = Vec::new();

    entries.push(EntryRef {
        path: "keys/amk-ledger.cbor".into(),
        hash: hash::hash_bytes(&sealed_ledger),
        size: sealed_ledger.len() as u64,
    });

    for a in &assets {
        let head = a.head()?;
        let ct_path = format!("blobs/{}", head.core.ciphertext_hash.to_hex());
        let meta_path = format!("meta/{}", a.asset_id);
        let prov_bytes = cbor::to_canonical_vec(&a.provenance).expect("provenance serializes");
        let prov_path = format!("provenance/{}", a.asset_id);

        for (path, data) in [
            (ct_path, &a.ciphertext),
            (meta_path, &a.metadata_blob),
            (prov_path, &prov_bytes),
        ] {
            entries.push(EntryRef {
                path: path.clone(),
                hash: hash::hash_bytes(data),
                size: data.len() as u64,
            });
            payloads.push((path, data.clone()));
        }
        let head_rec = a.provenance.last().unwrap().record_hash();
        provenance_heads.push((a.asset_id, head_rec));
    }
    entries.sort_by(|x, y| {
        let rank = |p: &str| {
            let role = p.split('/').next().unwrap_or("");
            blob_role_order(role)
        };
        x.path
            .split('/')
            .next()
            .cmp(&y.path.split('/').next())
            .then(rank(&x.path).cmp(&rank(&y.path)))
            .then(x.path.cmp(&y.path))
    });

    let core = ManifestCore {
        artifact_version: ARTIFACT_FORMAT_VERSION,
        crypto_suite_id: CRYPTO_SUITE_ID,
        min_protocol_version: PROTOCOL_VERSION.into(),
        exporter_device: input.exporter_device,
        export_timestamp: input.export_timestamp.clone(),
        source_library_version: input.source_library_version.clone(),
        entries,
        provenance_heads,
    };
    let core_bytes = cbor::to_canonical_vec(&core).expect("manifest core serializes");
    let mut mac = HmacSha256::new_from_slice(&wrap_key).expect("hmac key");
    mac.update(&core_bytes);
    let hmac = mac.finalize().into_bytes().to_vec();
    let exporter_sig = exporter.sign(&core_bytes)?;
    let manifest = Manifest {
        core,
        hmac,
        exporter_sig,
    };
    let manifest_bytes = cbor::to_canonical_vec(&manifest).expect("manifest serializes");

    // Write the tar: VERSION, MANIFEST, ledger, then sorted payloads.
    let mut builder = tar::Builder::new(Vec::new());
    tar_append(&mut builder, "VERSION", &version_blob(&salt));
    tar_append(&mut builder, "MANIFEST.cbor", &manifest_bytes);
    tar_append(&mut builder, "keys/amk-ledger.cbor", &sealed_ledger);
    // Re-sort payloads to match the manifest entry order.
    payloads.sort_by(|x, y| {
        x.0.split('/')
            .next()
            .cmp(&y.0.split('/').next())
            .then(x.0.cmp(&y.0))
    });
    for (path, data) in &payloads {
        tar_append(&mut builder, path, data);
    }
    builder
        .into_inner()
        .map_err(|e| BackupError::Format(e.to_string()))
}

/// Assemble a backup artifact, drawing a fresh random wrap salt (production path).
pub fn export(
    input: &BackupInput,
    passphrase: &[u8],
    exporter: &dyn Signer,
) -> Result<Vec<u8>, BackupError> {
    export_with_salt(input, passphrase, rng::random_array::<32>(), exporter)
}

// ── Restore ─────────────────────────────────────────────────────────────────

/// How aggressively a restore acts. Dry-run is the safe default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreMode {
    /// Verify structure only; no decrypt, no write.
    Preview,
    /// Verify decryption + hashes and compute the diff; **no write** (default).
    DryRun,
    /// Apply (the caller writes the returned assets); never a silent overwrite.
    Commit,
}

/// A decrypted asset ready to write into a library (returned only on `Commit`).
#[derive(Debug, Clone)]
pub struct RestoredAsset {
    /// Album id.
    pub album_id: Uuid,
    /// Asset id.
    pub asset_id: Uuid,
    /// Decrypted plaintext bytes.
    pub plaintext: Vec<u8>,
    /// The metadata blob (still encrypted; written verbatim).
    pub metadata_blob: Vec<u8>,
    /// The provenance chain.
    pub provenance: Vec<ProvenanceRecord>,
}

/// The outcome of a restore (chain-reconciliation; newer local state always wins).
#[derive(Debug, Default)]
pub struct RestoreReport {
    /// Entries whose content hash (and, past preview, decryption) verified.
    pub verified: usize,
    /// Assets absent locally — applied on commit.
    pub would_add: Vec<Uuid>,
    /// Assets already current locally — no-op.
    pub identical: Vec<Uuid>,
    /// Assets whose local head differs — not applied (quarantined for explicit merge).
    pub conflicts: Vec<Uuid>,
    /// Decrypted assets to write (populated only on `Commit`).
    pub applied: Vec<RestoredAsset>,
}

/// A verified, opened backup artifact, ready to restore.
pub struct BackupArtifact {
    files: BTreeMap<String, Vec<u8>>,
    ledger: BTreeMap<(Uuid, u32), [u8; 32]>,
    core: ManifestCore,
}

impl BackupArtifact {
    /// Open and fully verify an artifact: HMAC (wrap key) + exporter signature + per-entry
    /// content hashes + AMK-ledger completeness. The exporter key is looked up by the caller
    /// in the user's device directory.
    pub fn open(
        bytes: &[u8],
        passphrase: &[u8],
        exporter_pub: &HybridVerifyingKey,
    ) -> Result<Self, BackupError> {
        let files: BTreeMap<String, Vec<u8>> = tar_read(bytes)?.into_iter().collect();
        let version = files
            .get("VERSION")
            .ok_or(BackupError::Format("missing VERSION".into()))?;
        let (salt, params) = parse_version(version)?;
        let wrap_key = pwkdf::derive_wrap_key(passphrase, &salt, params)?;

        let manifest_bytes = files
            .get("MANIFEST.cbor")
            .ok_or(BackupError::Format("missing MANIFEST.cbor".into()))?;
        let manifest: Manifest =
            cbor::from_slice(manifest_bytes).map_err(|e| BackupError::Format(e.to_string()))?;
        let core_bytes = cbor::to_canonical_vec(&manifest.core).expect("manifest core serializes");

        // (1) HMAC under the wrap key (catches tamper before any decrypt).
        let mut mac = HmacSha256::new_from_slice(&wrap_key).expect("hmac key");
        mac.update(&core_bytes);
        mac.verify_slice(&manifest.hmac).map_err(|_| {
            BackupError::Auth("MANIFEST HMAC mismatch (tamper or wrong passphrase)")
        })?;
        // (2) Exporter hybrid signature (defeats a wrap-key thief).
        if !exporter_pub.verify(&core_bytes, &manifest.exporter_sig) {
            return Err(BackupError::Auth("MANIFEST exporter signature invalid"));
        }

        // (3) Per-entry content hashes.
        for entry in &manifest.core.entries {
            let data = files
                .get(&entry.path)
                .ok_or_else(|| BackupError::Corrupt(format!("missing entry {}", entry.path)))?;
            if hash::hash_bytes(data) != entry.hash || data.len() as u64 != entry.size {
                return Err(BackupError::Corrupt(entry.path.clone()));
            }
        }

        // (4) Unseal + load the AMK ledger.
        let sealed = files
            .get("keys/amk-ledger.cbor")
            .ok_or(BackupError::Format("missing AMK ledger".into()))?;
        let ledger = open_ledger(&wrap_key, sealed)?;
        let ledger: BTreeMap<(Uuid, u32), [u8; 32]> = ledger
            .entries
            .into_iter()
            .map(|(album, epoch, amk)| ((album, epoch), amk))
            .collect();

        Ok(Self {
            files,
            ledger,
            core: manifest.core,
        })
    }

    /// Restore against a target library's current provenance heads (`asset_id -> head hash`).
    /// `Preview` checks structure only; `DryRun` (default) also decrypts to verify; `Commit`
    /// returns the decrypted assets to write. Never overwrites newer local state.
    pub fn restore(
        &self,
        mode: RestoreMode,
        local_heads: &BTreeMap<Uuid, Hash32>,
    ) -> Result<RestoreReport, BackupError> {
        let mut report = RestoreReport::default();

        for (asset_id, head_hash) in &self.core.provenance_heads {
            report.verified += 1;

            // Reconcile against local state (newer local always wins; no silent overwrite).
            match local_heads.get(asset_id) {
                Some(local) if local == head_hash => {
                    report.identical.push(*asset_id);
                    continue;
                }
                Some(_) => {
                    report.conflicts.push(*asset_id);
                    continue;
                }
                None => report.would_add.push(*asset_id),
            }

            if mode == RestoreMode::Preview {
                continue;
            }

            // Decrypt to verify (DryRun) / to return for writing (Commit).
            let restored = self.decrypt_asset(asset_id)?;
            if mode == RestoreMode::Commit {
                report.applied.push(restored);
            }
        }
        Ok(report)
    }

    /// Decrypt one asset from the artifact using the ledger AMK + its head manifest.
    fn decrypt_asset(&self, asset_id: &Uuid) -> Result<RestoredAsset, BackupError> {
        let prov_bytes = self
            .files
            .get(&format!("provenance/{asset_id}"))
            .ok_or_else(|| BackupError::Corrupt(format!("missing provenance {asset_id}")))?;
        let provenance: Vec<ProvenanceRecord> =
            cbor::from_slice(prov_bytes).map_err(|e| BackupError::Format(e.to_string()))?;
        let head = provenance
            .last()
            .ok_or(BackupError::Auth("empty provenance chain"))?
            .manifest
            .clone();

        let ct = self
            .files
            .get(&format!("blobs/{}", head.core.ciphertext_hash.to_hex()))
            .ok_or_else(|| BackupError::Corrupt(format!("missing ciphertext for {asset_id}")))?;
        // Confirm the content hash before decrypting.
        if hash::hash_bytes(ct) != head.core.ciphertext_hash {
            return Err(BackupError::Corrupt(format!(
                "ciphertext hash for {asset_id}"
            )));
        }

        let amk_bytes = self
            .ledger
            .get(&(head.core.album_id, head.core.amk_version.0))
            .ok_or_else(|| {
                BackupError::AmkIncomplete(format!(
                    "album {} epoch {}",
                    head.core.album_id, head.core.amk_version.0
                ))
            })?;
        let amk = Amk::from_bytes(*amk_bytes);
        let file_key = amk.derive_file_key(&head.core.file_id);
        let plaintext = stream::decrypt_asset_vec(&file_key, &head.core.nonce_prefix, ct)
            .map_err(|_| BackupError::Auth("asset decryption failed"))?;

        let metadata_blob = self
            .files
            .get(&format!("meta/{asset_id}"))
            .cloned()
            .unwrap_or_default();

        Ok(RestoredAsset {
            album_id: head.core.album_id,
            asset_id: *asset_id,
            plaintext,
            metadata_blob,
            provenance,
        })
    }

    /// The exporter device id recorded in the manifest (provenance: who exported).
    pub fn exporter_device(&self) -> Uuid {
        self.core.exporter_device
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::keys::{Amk, AmkVersion, HybridSigningKey};
    use crate::crypto::primitives::PROTOCOL_VERSION;
    use crate::crypto::provenance::action::Action;
    use crate::crypto::provenance::manifest::{ASSET_MANIFEST_VERSION, ManifestCore as MCore};

    const ALBUM: u128 = 0xA1;

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
            let file_key = amk.derive_file_key(&file_id);
            let (enc, ct) = stream::encrypt_asset_vec_full(&file_key, plaintext);

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
                created_by_user: Uuid::from_u128(0x05E2),
                created_by_device: Uuid::from_u128(0xD1),
                client_version: "t".into(),
                timestamp: "2026-05-31T00:00:00Z".into(),
                action: Action::Create,
                prior_provenance_hash: None,
                retention_until: None,
            };
            let manifest = core.sign(&self.device, &self.write).unwrap();
            let record = ProvenanceRecord {
                asset_id: file_id,
                manifest,
                prior_provenance_hash: None,
            };
            BackupAsset {
                album_id: Uuid::from_u128(ALBUM),
                asset_id: file_id,
                ciphertext: ct,
                metadata_blob: crate::crypto::encryption::seal_blob(
                    &amk.derive_blob_key(&file_id),
                    b"{sidecar}",
                ),
                provenance: vec![record],
            }
        }

        fn input(&self, assets: Vec<BackupAsset>) -> BackupInput {
            let mut amks = BTreeMap::new();
            amks.insert((Uuid::from_u128(ALBUM), 1u32), self.amk);
            BackupInput {
                assets,
                amks,
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
        let a = export_with_salt(&input, b"pw", salt, &f.device).unwrap();
        let b = export_with_salt(&input, b"pw", salt, &f.device).unwrap();
        assert_eq!(a, b, "deterministic export must be byte-identical");
    }

    #[test]
    fn open_verifies_and_restore_to_fresh_library_recovers_plaintext() {
        let f = Fix::new();
        let input = f.input(vec![
            f.asset(1, b"hello world"),
            f.asset(2, b"second asset"),
        ]);
        let bytes = export(&input, b"pw", &f.device).unwrap();

        let art = BackupArtifact::open(&bytes, b"pw", &f.device.verifying_key()).unwrap();
        // Fresh library (no local heads) → everything applies.
        let report = art.restore(RestoreMode::Commit, &BTreeMap::new()).unwrap();
        assert_eq!(report.verified, 2);
        assert_eq!(report.would_add.len(), 2);
        assert_eq!(report.applied.len(), 2);

        let mut plaintexts: Vec<Vec<u8>> =
            report.applied.iter().map(|a| a.plaintext.clone()).collect();
        plaintexts.sort();
        assert_eq!(
            plaintexts,
            vec![b"hello world".to_vec(), b"second asset".to_vec()]
        );
    }

    #[test]
    fn wrong_passphrase_fails_to_open() {
        let f = Fix::new();
        let bytes = export(&f.input(vec![f.asset(1, b"x")]), b"right", &f.device).unwrap();
        assert!(BackupArtifact::open(&bytes, b"wrong", &f.device.verifying_key()).is_err());
    }

    #[test]
    fn tampering_an_entry_is_detected() {
        let f = Fix::new();
        let bytes = export(&f.input(vec![f.asset(1, b"x")]), b"pw", &f.device).unwrap();
        // Flip a byte somewhere in the archive body (a blob) → entry-hash or HMAC mismatch.
        let mut t = bytes.clone();
        let mid = t.len() / 2;
        t[mid] ^= 0x01;
        assert!(BackupArtifact::open(&t, b"pw", &f.device.verifying_key()).is_err());
    }

    #[test]
    fn wrong_exporter_key_is_rejected() {
        let f = Fix::new();
        let bytes = export(&f.input(vec![f.asset(1, b"x")]), b"pw", &f.device).unwrap();
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
            export(&input, b"pw", &f.device),
            Err(BackupError::AmkIncomplete(_))
        ));
    }

    #[test]
    fn restore_reconciliation_matrix() {
        let f = Fix::new();
        let asset = f.asset(1, b"content");
        let head = asset.provenance.last().unwrap().record_hash();
        let asset_id = asset.asset_id;
        let bytes = export(&f.input(vec![asset]), b"pw", &f.device).unwrap();
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

    #[test]
    fn dry_run_writes_nothing() {
        let f = Fix::new();
        let bytes = export(&f.input(vec![f.asset(1, b"x")]), b"pw", &f.device).unwrap();
        let art = BackupArtifact::open(&bytes, b"pw", &f.device.verifying_key()).unwrap();
        let r = art.restore(RestoreMode::DryRun, &BTreeMap::new()).unwrap();
        // DryRun verifies (decrypts) but returns nothing to write.
        assert_eq!(r.verified, 1);
        assert!(r.applied.is_empty());
    }
}
