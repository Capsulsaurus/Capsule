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
//! receipts/{asset_id}     # the asset's custody-receipt log, verbatim (when held)
//! attestations/{asset_id} # the asset's signed storage-attestation log, verbatim (when held)
//! authority/{album_id}    # the album's admin-signed epoch ledger (public; canonical CBOR)
//! library/attestation-keys.cbor # the library's pinned attestation-key history, verbatim (when held)
//! ```
//!
//! The `authority/` entries (format version 2) carry each album's **public** authority — the
//! admin verifying key and the admin-signed epoch ledger a manifest's write signature is checked
//! against — so a restored asset verifies, not merely decrypts. No signing seed rides the
//! artifact: a restored album stays read-only.
//!
//! The MANIFEST is authenticated **two ways**: an HMAC under the wrap key (catches
//! tamper/truncation before any decrypt) and a hybrid exporter signature (defeats a
//! wrap-key thief who could otherwise re-HMAC). Restore is a **chain reconciliation**, never
//! a blind overwrite, and **dry-run is the default**.
//!
//! [Backup — Backup Artifact]: https://docs/design/backup-recovery/#backup-artifact

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::sync::Arc;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

use super::{ARTIFACT_FORMAT_VERSION, BackupError};
use crate::cbor;
use crate::crypto::authority::{ReferenceAuthority, SignedEpochLedger};
use crate::crypto::encryption::{decrypt_asset_vec, recover_file_key};
use crate::crypto::hash::{self, Hash32};
use crate::crypto::keys::{Amk, HybridVerifyingKey, Signer};
use crate::crypto::primitives::{Argon2Params, CRYPTO_SUITE_ID, PROTOCOL_VERSION, info};
use crate::crypto::provenance::ProvenanceRecord;
use crate::crypto::{kdf, pwkdf, rng};

type HmacSha256 = Hmac<Sha256>;

/// The production Argon2id cost for the backup wrap key: the normal device tier.
///
/// The parameters an export actually used are recorded in the artifact's `VERSION` entry, so a
/// restore reproduces the wrap key from the artifact and never from a constant. That is why
/// this is a *default* and not a rule: [`export`] and [`export_with_salt`] — the two entry
/// points a production build has — apply it and take no cost argument, while the `*_with_params`
/// pair takes the cost from the caller and is compiled only under `cfg(test)` or the non-default
/// `test-support` feature. [`BackupArtifact::open`] reads the cost back off the artifact either
/// way.
///
/// It is one constant for every build. It used to be forked on `#[cfg(test)]`, which meant
/// `capsule-core`'s own tests were the only code in the workspace that never exercised the
/// production cost, while every downstream test paid it in full with no way to opt out.
pub const WRAP_PARAMS: Argon2Params = Argon2Params {
    mem_kib: 256 * 1024,
    t_cost: 3,
    p_cost: 1,
};

/// Where an asset's STREAM ciphertext comes from when it is written into an artifact.
///
/// A **source**, not a buffer: the export streams each original straight into the artifact's
/// writer, so exporting holds one STREAM chunk of it rather than the whole library's ciphertext.
/// The library's sources re-derive the ciphertext from the plaintext on disk
/// (`OriginalCiphertext`); an in-memory `Vec<u8>` is a
/// source too, for callers that already hold the bytes.
pub trait CiphertextSource: std::fmt::Debug + Send + Sync {
    /// The ciphertext's exact length in bytes — what the artifact's tar header declares before
    /// a byte of the body is written.
    fn size(&self) -> u64;

    /// Stream the whole ciphertext into `writer` and return its SHA-256 content address.
    fn write_to(&self, writer: &mut dyn Write) -> io::Result<Hash32>;
}

impl CiphertextSource for Vec<u8> {
    fn size(&self) -> u64 {
        self.len() as u64
    }

    fn write_to(&self, writer: &mut dyn Write) -> io::Result<Hash32> {
        writer.write_all(self)?;
        Ok(hash::hash_bytes(self))
    }
}

/// One asset to back up: its ciphertext, metadata blob, and full provenance chain.
#[derive(Debug, Clone)]
pub struct BackupAsset {
    /// Album the asset belongs to.
    pub album_id: Uuid,
    /// The asset id.
    pub asset_id: Uuid,
    /// The STREAM ciphertext blob, as a source the export streams from. Its bytes must hash to
    /// the head manifest's `ciphertext_hash`: the MANIFEST commits to that address before the
    /// body is written, and an export whose source yields anything else fails.
    pub ciphertext: Arc<dyn CiphertextSource>,
    /// Encrypted metadata blob (wire format).
    pub metadata_blob: Vec<u8>,
    /// The asset's provenance chain (oldest first).
    pub provenance: Vec<ProvenanceRecord>,
    /// The asset's custody-receipt log (`{uuid}.receipts.cbor` bytes; empty = none). Evidence,
    /// not a cache — a server destroying the record of its own liability is the adversary — so
    /// it rides the artifact verbatim (S-D4).
    pub receipts: Vec<u8>,
    /// The asset's signed storage-attestation log (`{uuid}.attestations.cbor` bytes; empty =
    /// none). Evidence for the same reason as `receipts` — a signed attestation is how a client
    /// later proves the server reported a blob lost — so it rides the artifact verbatim.
    pub attestations: Vec<u8>,
}

impl BackupAsset {
    fn head(&self) -> Result<&crate::crypto::provenance::AssetManifest, BackupError> {
        self.provenance
            .last()
            .map(|r| &r.manifest)
            .ok_or(BackupError::Auth("asset has empty provenance chain"))
    }
}

/// Which organization record a [`BackupDoc`] carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BackupDocKind {
    /// An album's `AlbumDocV1` (its labels and name), keyed by `album_id`.
    Album,
    /// A stack's `StackDocV1` (its labels), keyed by `stack_id`.
    Stack,
}

impl BackupDocKind {
    fn path_segment(self) -> &'static str {
        match self {
            Self::Album => "album",
            Self::Stack => "stack",
        }
    }

    fn from_path_segment(segment: &str) -> Option<Self> {
        match segment {
            "album" => Some(Self::Album),
            "stack" => Some(Self::Stack),
            _ => None,
        }
    }
}

/// One sealed organization record riding the artifact: `docs/{kind}/{doc_id}`.
///
/// The record stays sealed under its album's AMK exactly as it travels between members; the
/// artifact escrows that AMK in its ledger like any asset's, so a restore opens it with nothing
/// but the passphrase. An older reader skips the entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupDoc {
    /// Album or stack record.
    pub kind: BackupDocKind,
    /// The record's key: the `album_id` or `stack_id`.
    pub doc_id: Uuid,
    /// The album whose AMK seals it.
    pub album_id: Uuid,
    /// The epoch of that AMK.
    pub amk_version: u32,
    /// The sealed record (metadata-blob wire format).
    pub sealed: Vec<u8>,
}

/// The on-wire body of a `docs/` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DocEntry {
    album_id: Uuid,
    amk_version: u32,
    #[serde(with = "serde_bytes")]
    sealed: Vec<u8>,
}

/// Everything needed to assemble an artifact.
pub struct BackupInput {
    /// The assets to include.
    pub assets: Vec<BackupAsset>,
    /// The AMK bytes for every `(album_id, amk_version)` an asset references.
    pub amks: BTreeMap<(Uuid, u32), [u8; 32]>,
    /// Each album's public authority: the admin-signed epoch ledger its manifests' write
    /// signatures verify under. Only albums an included asset belongs to are written; an album
    /// with no entry here (e.g. one itself recovered from a format-1 artifact) exports without
    /// one, and restores readable but unverifiable.
    pub authorities: BTreeMap<Uuid, SignedEpochLedger>,
    /// Sealed album and stack label records. Each one's `(album_id, amk_version)` AMK must be
    /// in [`amks`](Self::amks); the ledger escrows it.
    pub docs: Vec<BackupDoc>,
    /// The library's pinned attestation-key history (`.library/attestation-keys.cbor` bytes;
    /// empty = none). It rides the artifact verbatim so a restored library keeps checking
    /// receipts under the keys it pinned, instead of trusting whatever a server publishes first
    /// after the restore.
    pub attestation_key_pins: Vec<u8>,
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

/// The artifact entry carrying the library's pinned attestation-key history.
const ATTESTATION_KEY_PINS_ENTRY: &str = "library/attestation-keys.cbor";

fn blob_role_order(role: &str) -> u8 {
    match role {
        "blobs" => 0,
        "meta" => 1,
        "provenance" => 2,
        "receipts" => 3,
        "authority" => 4,
        "docs" => 5,
        "attestations" => 6,
        "library" => 7,
        _ => 8,
    }
}

// ── tar I/O (deterministic) ─────────────────────────────────────────────────

/// The tar block size: every entry body is zero-padded to a multiple of it.
const TAR_BLOCK: u64 = 512;

fn tar_io(e: &io::Error) -> BackupError {
    BackupError::Format(format!("writing the artifact: {e}"))
}

/// The deterministic header every entry carries: a regular file, mode 0644, owned by 0:0 at
/// the epoch, declaring `size` body bytes.
fn tar_header(size: u64) -> tar::Header {
    let mut h = tar::Header::new_gnu();
    h.set_size(size);
    h.set_mode(0o644);
    h.set_mtime(0);
    h.set_uid(0);
    h.set_gid(0);
    h.set_entry_type(tar::EntryType::Regular);
    h
}

fn tar_append<W: Write>(
    builder: &mut tar::Builder<W>,
    path: &str,
    data: &[u8],
) -> Result<(), BackupError> {
    let mut h = tar_header(data.len() as u64);
    builder
        .append_data(&mut h, path, data)
        .map_err(|e| tar_io(&e))
}

/// Append an entry whose body is streamed from `source` rather than held, byte-identical to
/// [`tar_append`] over the same bytes.
///
/// `tar` takes an entry body only as a [`Read`], and a [`CiphertextSource`] pushes into a
/// writer, so the entry is written in two halves: `tar` writes the header — with the same path
/// encoding [`tar_append`] gets — over an empty body (zero bytes copied, so no padding), and the
/// body and its padding then go straight into the builder's writer. The body must be exactly
/// the `size` the header declared and hash to `expected`, the address the MANIFEST already
/// committed to; anything else fails the export, whose output must then be discarded.
fn tar_append_stream<W: Write>(
    builder: &mut tar::Builder<W>,
    path: &str,
    source: &dyn CiphertextSource,
    expected: Hash32,
) -> Result<(), BackupError> {
    let size = source.size();
    let mut h = tar_header(size);
    builder
        .append_data(&mut h, path, io::empty())
        .map_err(|e| tar_io(&e))?;
    let mut body = CountingWriter::new(builder.get_mut());
    let written_hash = source.write_to(&mut body).map_err(|e| tar_io(&e))?;
    let written = body.written;
    if written != size || written_hash != expected {
        tracing::error!(
            path,
            declared_size = size,
            written,
            expected = %expected.to_hex(),
            actual = %written_hash.to_hex(),
            "backup: a ciphertext source disagrees with its manifest"
        );
        return Err(BackupError::Corrupt(format!(
            "{path}: the source yielded {written} bytes hashing to {}, not the {size} bytes \
             hashing to {} the manifest commits to",
            written_hash.to_hex(),
            expected.to_hex()
        )));
    }
    let padding = (TAR_BLOCK - size % TAR_BLOCK) % TAR_BLOCK;
    builder
        .get_mut()
        .write_all(&[0; TAR_BLOCK as usize][..padding as usize])
        .map_err(|e| tar_io(&e))
}

/// A writer that counts what passes through it to `inner`.
struct CountingWriter<W> {
    inner: W,
    written: u64,
}

impl<W> CountingWriter<W> {
    fn new(inner: W) -> Self {
        Self { inner, written: 0 }
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
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

/// The plaintext `VERSION` entry. `params` is the cost this export actually derived under, not
/// a compiled-in default: [`parse_version`] reads it straight back, which is what makes a
/// restore reproduce the wrap key of an artifact exported at any cost.
fn version_blob(salt: &[u8; 32], params: Argon2Params) -> Vec<u8> {
    format!(
        "artifact_format={ARTIFACT_FORMAT_VERSION}\ncrypto_suite_id={CRYPTO_SUITE_ID}\nmin_protocol_version={PROTOCOL_VERSION}\nwrap_salt={}\nwrap_mem_kib={}\nwrap_t={}\nwrap_p={}\n",
        hex::encode(salt),
        params.mem_kib,
        params.t_cost,
        params.p_cost,
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

/// Assemble a backup artifact with an explicit wrap salt (deterministic), at the production
/// [`WRAP_PARAMS`] cost.
pub fn export_with_salt(
    input: &BackupInput,
    passphrase: &[u8],
    salt: [u8; 32],
    exporter: &dyn Signer,
) -> Result<Vec<u8>, BackupError> {
    let mut out = Vec::new();
    export_inner(input, passphrase, salt, WRAP_PARAMS, exporter, &mut out)?;
    Ok(out)
}

/// Assemble a backup artifact with an explicit wrap salt **and** an explicit Argon2id cost.
///
/// `params` is written into the artifact's `VERSION` entry, so whatever cost is chosen here is
/// the cost [`BackupArtifact::open`] pays to reproduce the wrap key — a caller that exports
/// cheaply gets a cheap restore, and neither side needs to be told twice.
///
/// **Not in a production build.** A weak `params` produces a brute-forceable artifact, and
/// nothing downstream of here re-checks the cost: [`pwkdf::derive_wrap_key`] accepts anything
/// `argon2::Params::new` validates. So the entry point that can do it is compiled only under
/// `cfg(test)` or the non-default `test-support` feature, and weakening a real backup therefore
/// takes a visible line in a production manifest rather than an unnoticed call. The always-
/// available [`export`] and [`export_with_salt`] cannot be given a cost at all.
///
/// This is the same guard [`escrow_master_key`](super::escrow_master_key) gets from taking a
/// closed [`DeviceTier`](crate::crypto::primitives::DeviceTier) instead of raw parameters,
/// reached differently: every `DeviceTier` arm is memory-hard by design, so the tier enum has no
/// arm a test could use as the fast path.
#[cfg(any(test, feature = "test-support"))]
pub fn export_with_salt_and_params(
    input: &BackupInput,
    passphrase: &[u8],
    salt: [u8; 32],
    params: Argon2Params,
    exporter: &dyn Signer,
) -> Result<Vec<u8>, BackupError> {
    let mut out = Vec::new();
    export_inner(input, passphrase, salt, params, exporter, &mut out)?;
    Ok(out)
}

/// One entry body awaiting its place in the tar: bytes assembled (or borrowed) up front, or a
/// ciphertext streamed from its source only when the entry is written.
enum Payload<'a> {
    Bytes(Cow<'a, [u8]>),
    Ciphertext {
        source: &'a dyn CiphertextSource,
        hash: Hash32,
    },
}

/// The one implementation every export entry point delegates to. Private, so the only way to
/// reach it with a caller-chosen cost is through the gated entry points above.
///
/// Writes the artifact into `out` and returns its length. Each original's ciphertext is
/// streamed from its [`CiphertextSource`] as its entry is written, so the export holds one
/// STREAM chunk of an original at a time, never the library's ciphertext. Its MANIFEST entry
/// is the head manifest's `ciphertext_hash` and the source's declared size, known before the
/// body is read; the write then checks the body against both. On an error `out` holds a
/// partial artifact the caller must discard.
fn export_inner(
    input: &BackupInput,
    passphrase: &[u8],
    salt: [u8; 32],
    params: Argon2Params,
    exporter: &dyn Signer,
    out: &mut dyn Write,
) -> Result<u64, BackupError> {
    let wrap_key = pwkdf::derive_wrap_key(passphrase, &salt, params)?;

    // Build the AMK ledger, asserting completeness for every referenced epoch.
    let mut ledger = AmkLedger::default();
    let mut needed: BTreeSet<(Uuid, u32)> = BTreeSet::new();
    for a in &input.assets {
        let head = a.head()?;
        needed.insert((a.album_id, head.core.amk_version.0));
    }
    for doc in &input.docs {
        needed.insert((doc.album_id, doc.amk_version));
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
    let mut payloads: Vec<(String, Payload<'_>)> = Vec::new();
    let mut provenance_heads: Vec<(Uuid, Hash32)> = Vec::new();

    entries.push(EntryRef {
        path: "keys/amk-ledger.cbor".into(),
        hash: hash::hash_bytes(&sealed_ledger),
        size: sealed_ledger.len() as u64,
    });

    // Each included album's public authority, when the exporter holds one. Public data, so it
    // rides in the clear; the MANIFEST's HMAC and exporter signature cover it like every entry.
    let albums: BTreeSet<Uuid> = needed.iter().map(|(album, _)| *album).collect();
    for album in &albums {
        let Some(ledger) = input.authorities.get(album) else {
            tracing::warn!(
                album_id = %album,
                "backup: album has no reference authority to export; it restores unverifiable"
            );
            continue;
        };
        if ledger.album_id != *album {
            return Err(BackupError::Format(format!(
                "authority ledger for album {album} speaks for album {}",
                ledger.album_id
            )));
        }
        let path = format!("authority/{album}");
        let data = cbor::to_canonical_vec(ledger).expect("authority ledger serializes");
        entries.push(EntryRef {
            path: path.clone(),
            hash: hash::hash_bytes(&data),
            size: data.len() as u64,
        });
        payloads.push((path, Payload::Bytes(Cow::Owned(data))));
    }

    // Album and stack label records, still sealed under their album's AMK.
    for doc in &input.docs {
        let path = format!("docs/{}/{}", doc.kind.path_segment(), doc.doc_id);
        let data = cbor::to_canonical_vec(&DocEntry {
            album_id: doc.album_id,
            amk_version: doc.amk_version,
            sealed: doc.sealed.clone(),
        })
        .expect("doc entry serializes");
        entries.push(EntryRef {
            path: path.clone(),
            hash: hash::hash_bytes(&data),
            size: data.len() as u64,
        });
        payloads.push((path, Payload::Bytes(Cow::Owned(data))));
    }

    // The library's pinned attestation-key history, verbatim. Public keys, so it rides in the
    // clear under the MANIFEST's HMAC and exporter signature like every entry. Additive: an
    // older reader hash-checks it as a manifest entry and otherwise ignores it.
    if !input.attestation_key_pins.is_empty() {
        let data = input.attestation_key_pins.as_slice();
        entries.push(EntryRef {
            path: ATTESTATION_KEY_PINS_ENTRY.into(),
            hash: hash::hash_bytes(data),
            size: data.len() as u64,
        });
        payloads.push((
            ATTESTATION_KEY_PINS_ENTRY.into(),
            Payload::Bytes(Cow::Borrowed(data)),
        ));
    }

    for a in &assets {
        let head = a.head()?;
        let ct_path = format!("blobs/{}", head.core.ciphertext_hash.to_hex());
        let meta_path = format!("meta/{}", a.asset_id);
        let prov_bytes = cbor::to_canonical_vec(&a.provenance).expect("provenance serializes");
        let prov_path = format!("provenance/{}", a.asset_id);

        // The original's entry is committed to from what is known without reading it: the
        // address its signed manifest names and the size its source declares. Its bytes are
        // read once, when the entry is written, and checked against both there.
        entries.push(EntryRef {
            path: ct_path.clone(),
            hash: head.core.ciphertext_hash,
            size: a.ciphertext.size(),
        });
        payloads.push((
            ct_path,
            Payload::Ciphertext {
                source: a.ciphertext.as_ref(),
                hash: head.core.ciphertext_hash,
            },
        ));

        let mut asset_entries: Vec<(String, Cow<'_, [u8]>)> = vec![
            (meta_path, Cow::Borrowed(&a.metadata_blob)),
            (prov_path, Cow::Owned(prov_bytes)),
        ];
        // The custody-receipt log rides the artifact only when present (absent = no key).
        if !a.receipts.is_empty() {
            asset_entries.push((
                format!("receipts/{}", a.asset_id),
                Cow::Borrowed(&a.receipts),
            ));
        }
        // So does the signed storage-attestation log.
        if !a.attestations.is_empty() {
            asset_entries.push((
                format!("attestations/{}", a.asset_id),
                Cow::Borrowed(&a.attestations),
            ));
        }
        for (path, data) in asset_entries {
            entries.push(EntryRef {
                path: path.clone(),
                hash: hash::hash_bytes(&data),
                size: data.len() as u64,
            });
            payloads.push((path, Payload::Bytes(data)));
        }
        let head_rec = a
            .provenance
            .last()
            .expect("provenance chain is never empty")
            .record_hash();
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
    let mut builder = tar::Builder::new(CountingWriter::new(out));
    tar_append(&mut builder, "VERSION", &version_blob(&salt, params))?;
    tar_append(&mut builder, "MANIFEST.cbor", &manifest_bytes)?;
    tar_append(&mut builder, "keys/amk-ledger.cbor", &sealed_ledger)?;
    // Re-sort payloads to match the manifest entry order.
    payloads.sort_by(|x, y| {
        x.0.split('/')
            .next()
            .cmp(&y.0.split('/').next())
            .then(x.0.cmp(&y.0))
    });
    for (path, payload) in &payloads {
        match payload {
            Payload::Bytes(data) => tar_append(&mut builder, path, data)?,
            Payload::Ciphertext { source, hash } => {
                tar_append_stream(&mut builder, path, *source, *hash)?;
            }
        }
    }
    let mut counted = builder.into_inner().map_err(|e| tar_io(&e))?;
    counted.flush().map_err(|e| tar_io(&e))?;
    tracing::debug!(
        assets = assets.len(),
        entries = payloads.len() + 3,
        bytes = counted.written,
        "backup: artifact written"
    );
    Ok(counted.written)
}

/// Assemble a backup artifact in memory, drawing a fresh random wrap salt, at the production
/// [`WRAP_PARAMS`] cost.
///
/// The whole artifact is returned as one buffer, so this suits a caller that needs the bytes
/// themselves (a peer delta it content-addresses). Exporting a library goes through
/// [`export_to`], which streams.
pub fn export(
    input: &BackupInput,
    passphrase: &[u8],
    exporter: &dyn Signer,
) -> Result<Vec<u8>, BackupError> {
    export_with_salt(input, passphrase, rng::random_array::<32>(), exporter)
}

/// Write a backup artifact into `out`, drawing a fresh random wrap salt, at the production
/// [`WRAP_PARAMS`] cost (production path), and return its length in bytes.
///
/// Each asset's ciphertext is streamed from its [`CiphertextSource`] as its entry is written,
/// so memory stays at one STREAM chunk of an original whatever the library's size. On an error
/// `out` holds a partial artifact, which the caller must discard.
///
/// This is the entry point `Workspace::export_backup`
/// runs, and it takes no cost argument, so no caller of it can produce a weak artifact.
pub fn export_to(
    input: &BackupInput,
    passphrase: &[u8],
    exporter: &dyn Signer,
    out: &mut dyn Write,
) -> Result<u64, BackupError> {
    export_inner(
        input,
        passphrase,
        rng::random_array::<32>(),
        WRAP_PARAMS,
        exporter,
        out,
    )
}

/// As [`export`] but with an explicit Argon2id cost for the wrap key — the entry point a test
/// uses instead of reaching for a build-configuration fork.
///
/// **Not in a production build**, for the reason given on [`export_with_salt_and_params`].
#[cfg(any(test, feature = "test-support"))]
pub fn export_with_params(
    input: &BackupInput,
    passphrase: &[u8],
    params: Argon2Params,
    exporter: &dyn Signer,
) -> Result<Vec<u8>, BackupError> {
    let mut out = Vec::new();
    export_with_params_to(input, passphrase, params, exporter, &mut out)?;
    Ok(out)
}

/// As [`export_to`] but with an explicit Argon2id cost for the wrap key.
///
/// **Not in a production build**, for the reason given on [`export_with_salt_and_params`].
#[cfg(any(test, feature = "test-support"))]
pub fn export_with_params_to(
    input: &BackupInput,
    passphrase: &[u8],
    params: Argon2Params,
    exporter: &dyn Signer,
    out: &mut dyn Write,
) -> Result<u64, BackupError> {
    export_inner(
        input,
        passphrase,
        rng::random_array::<32>(),
        params,
        exporter,
        out,
    )
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
    /// The asset's custody-receipt log (`{uuid}.receipts.cbor` bytes; empty if the artifact
    /// carried none) — written back beside the provenance chain on commit (S-D4).
    pub receipts: Vec<u8>,
    /// The asset's signed storage-attestation log (`{uuid}.attestations.cbor` bytes; empty if
    /// the artifact carried none) — written back beside the receipts on commit.
    pub attestations: Vec<u8>,
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
    authorities: BTreeMap<Uuid, SignedEpochLedger>,
    docs: Vec<BackupDoc>,
    attestation_key_pins: Option<Vec<u8>>,
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
        // An authenticated manifest from a newer format is refused rather than half-read.
        if manifest.core.artifact_version > ARTIFACT_FORMAT_VERSION {
            return Err(BackupError::Format(format!(
                "artifact format {} is newer than this build supports ({ARTIFACT_FORMAT_VERSION})",
                manifest.core.artifact_version
            )));
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

        // (5) Decode + re-verify every album authority the (authenticated) manifest lists.
        let authorities = load_authorities(&manifest.core, &files)?;

        // (6) Decode every album/stack label record the manifest lists; each must be sealed under
        // an AMK the ledger escrows.
        let docs = load_docs(&manifest.core, &files, &ledger)?;

        // (7) The pinned attestation-key history, when the (authenticated) manifest lists one.
        // Its bytes were hash-checked in (3); the restore decodes them.
        let attestation_key_pins = manifest
            .core
            .entries
            .iter()
            .any(|entry| entry.path == ATTESTATION_KEY_PINS_ENTRY)
            .then(|| files.get(ATTESTATION_KEY_PINS_ENTRY).cloned())
            .flatten();

        Ok(Self {
            files,
            ledger,
            authorities,
            docs,
            attestation_key_pins,
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
        // Honour the head's `key_mode`: an adopted drop carries its key wrapped, not derived.
        let file_key = recover_file_key(&Amk::from_bytes(*amk_bytes), &head.core)
            .map_err(|_| BackupError::Auth("asset file key recovery failed"))?;
        let plaintext = decrypt_asset_vec(&file_key, &head.core.nonce_prefix, ct)
            .map_err(|_| BackupError::Auth("asset decryption failed"))?;

        let metadata_blob = self
            .files
            .get(&format!("meta/{asset_id}"))
            .cloned()
            .unwrap_or_default();

        // The custody-receipt log rides the artifact when the exporter held one (S-D4).
        let receipts = self
            .files
            .get(&format!("receipts/{asset_id}"))
            .cloned()
            .unwrap_or_default();
        // And the signed storage-attestation log, likewise. An artifact from before the log
        // existed carries none, which restores as an empty log.
        let attestations = self
            .files
            .get(&format!("attestations/{asset_id}"))
            .cloned()
            .unwrap_or_default();

        Ok(RestoredAsset {
            album_id: head.core.album_id,
            asset_id: *asset_id,
            plaintext,
            metadata_blob,
            provenance,
            receipts,
            attestations,
        })
    }

    /// The escrowed AMK rows this artifact carries: `(album_id, epoch, amk)`.
    ///
    /// The same row shape the durable album keystore holds
    /// ([`AmkRow`](crate::crypto::keys::AmkRow)), so folding a restore's recovered keys into a
    /// library's keystore is a `map`, not a translation table. This is the path by which a
    /// library that lost its album keys becomes able to read its photos again — the backup path
    /// is deliberately independent of the MLS ratchet (SSoT: [Keys]).
    ///
    /// Note what is *not* here: the write-tier and admin signing keys. Those are MLS-distributed
    /// capabilities, not escrowed secrets, so an album restored into a library that never held
    /// it is readable but not writable. Its public authority rides separately
    /// ([`authority_ledgers`](Self::authority_ledgers)), which is what makes it verifiable.
    ///
    /// [Keys]: https://docs/design/cryptography/keys/#album-master-keys-amks
    pub fn amk_rows(&self) -> Vec<(Uuid, u32, [u8; 32])> {
        self.ledger
            .iter()
            .map(|((album, epoch), amk)| (*album, *epoch, *amk))
            .collect()
    }

    /// Each album's public authority the artifact carries: the admin-signed epoch ledger, keyed
    /// by album id. Every ledger here was listed in the authenticated manifest, speaks for the
    /// album it is filed under, and re-verified its admin signature chain in [`open`](Self::open).
    /// Empty for a format-1 artifact, which carried none.
    pub fn authority_ledgers(&self) -> &BTreeMap<Uuid, SignedEpochLedger> {
        &self.authorities
    }

    /// The sealed album and stack label records the artifact carries, each listed in the
    /// authenticated manifest and sealed under an AMK [`amk`](Self::amk) returns. Empty for an
    /// artifact exported before the records existed.
    pub fn docs(&self) -> &[BackupDoc] {
        &self.docs
    }

    /// The library's pinned attestation-key history the artifact carries
    /// (`.library/attestation-keys.cbor` bytes, verbatim), listed in the authenticated manifest.
    /// `None` for an artifact exported by a library that had pinned nothing, or before the pin
    /// rode the artifact; such an artifact restores exactly as it did before.
    pub fn attestation_key_pins(&self) -> Option<&[u8]> {
        self.attestation_key_pins.as_deref()
    }

    /// The escrowed AMK for `(album_id, epoch)`, if the ledger holds it.
    pub fn amk(&self, album_id: &Uuid, epoch: u32) -> Option<[u8; 32]> {
        self.ledger.get(&(*album_id, epoch)).copied()
    }

    /// The exporter device id recorded in the manifest (provenance: who exported).
    pub fn exporter_device(&self) -> Uuid {
        self.core.exporter_device
    }

    /// The `(asset_id, head_hash)` provenance heads the artifact carries — the same list
    /// [`restore`](Self::restore) reconciles against local state. Exposed so a chain-aware
    /// consumer (LAN peering, slice `S-E3`) can decide *forward-update vs. stale* per asset
    /// before restoring, rather than treating every differing head as an opaque conflict.
    pub fn provenance_heads(&self) -> &[(Uuid, Hash32)] {
        &self.core.provenance_heads
    }

    /// The full provenance chain (oldest first) the artifact carries for `asset_id`, decoded
    /// from its `provenance/{asset_id}` entry. `None` when the artifact carries no such asset.
    ///
    /// Peering walks this chain to distinguish a **forward** artifact (whose chain contains the
    /// receiver's current head as an ancestor — safe to adopt) from a **stale** one (whose head
    /// the receiver has already superseded — quarantined, never applied). The per-entry content
    /// hashes were already verified by [`open`](Self::open), so the bytes here are authentic.
    pub fn provenance_chain(
        &self,
        asset_id: &Uuid,
    ) -> Result<Option<Vec<ProvenanceRecord>>, BackupError> {
        match self.files.get(&format!("provenance/{asset_id}")) {
            Some(bytes) => {
                let chain: Vec<ProvenanceRecord> =
                    cbor::from_slice(bytes).map_err(|e| BackupError::Format(e.to_string()))?;
                Ok(Some(chain))
            }
            None => Ok(None),
        }
    }
}

/// Decode every `authority/{album_id}` entry the manifest lists (the manifest is authenticated
/// by this point, so a file the manifest does not name is ignored, never trusted), and refuse a
/// ledger filed under the wrong album or whose admin signature chain does not re-verify.
fn load_authorities(
    core: &ManifestCore,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<BTreeMap<Uuid, SignedEpochLedger>, BackupError> {
    let mut out = BTreeMap::new();
    for entry in &core.entries {
        let Some(album) = entry.path.strip_prefix("authority/") else {
            continue;
        };
        let album_id = Uuid::parse_str(album)
            .map_err(|_| BackupError::Format(format!("bad authority entry {}", entry.path)))?;
        let bytes = files
            .get(&entry.path)
            .ok_or_else(|| BackupError::Corrupt(format!("missing entry {}", entry.path)))?;
        let ledger: SignedEpochLedger =
            cbor::from_slice(bytes).map_err(|e| BackupError::Format(e.to_string()))?;
        if ledger.album_id != album_id {
            return Err(BackupError::Auth(
                "authority ledger filed under another album",
            ));
        }
        if ReferenceAuthority::from_ledger(&ledger, &BTreeSet::new()).is_none() {
            return Err(BackupError::Auth(
                "authority ledger failed admin-chain verification",
            ));
        }
        tracing::debug!(
            album_id = %album_id,
            epochs = ledger.entries.len(),
            "backup: album authority ledger verified"
        );
        out.insert(album_id, ledger);
    }
    Ok(out)
}

/// Decode every `docs/{kind}/{doc_id}` entry the (authenticated) manifest lists, refusing one
/// whose path is malformed or whose sealing AMK the ledger does not escrow.
fn load_docs(
    core: &ManifestCore,
    files: &BTreeMap<String, Vec<u8>>,
    ledger: &BTreeMap<(Uuid, u32), [u8; 32]>,
) -> Result<Vec<BackupDoc>, BackupError> {
    let mut out = Vec::new();
    for entry in &core.entries {
        let Some(rest) = entry.path.strip_prefix("docs/") else {
            continue;
        };
        let bad = || BackupError::Format(format!("bad doc entry {}", entry.path));
        let (kind, id) = rest.split_once('/').ok_or_else(bad)?;
        let kind = BackupDocKind::from_path_segment(kind).ok_or_else(bad)?;
        let doc_id = Uuid::parse_str(id).map_err(|_| bad())?;
        let bytes = files
            .get(&entry.path)
            .ok_or_else(|| BackupError::Corrupt(format!("missing entry {}", entry.path)))?;
        let body: DocEntry =
            cbor::from_slice(bytes).map_err(|e| BackupError::Format(e.to_string()))?;
        if !ledger.contains_key(&(body.album_id, body.amk_version)) {
            return Err(BackupError::AmkIncomplete(format!(
                "album {} epoch {} (doc {})",
                body.album_id, body.amk_version, entry.path
            )));
        }
        out.push(BackupDoc {
            kind,
            doc_id,
            album_id: body.album_id,
            amk_version: body.amk_version,
            sealed: body.sealed,
        });
    }
    tracing::debug!(docs = out.len(), "backup: label records loaded");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::keys::{Amk, AmkVersion, HybridSigningKey};
    use crate::crypto::primitives::PROTOCOL_VERSION;
    use crate::crypto::provenance::{
        ASSET_MANIFEST_VERSION, Action, KeyMode, ManifestCore as MCore,
    };

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
        fn wrapped_asset(
            &self,
            asset_id: u128,
            guest_key: [u8; 32],
            plaintext: &[u8],
        ) -> BackupAsset {
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

        let mut plaintexts: Vec<Vec<u8>> =
            report.applied.iter().map(|a| a.plaintext.clone()).collect();
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
        // S-D4: the custody-receipt log is evidence, so it is included in the backup artifact
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
        let bytes = export_with_params(&f.input(vec![f.asset(1, b"x")]), b"right", FAST, &f.device)
            .unwrap();
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
}
