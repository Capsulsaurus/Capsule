//! The per-asset **upload bundle** — everything a network layer needs to push one managed
//! asset to a server, and nothing of the [`Workspace`] itself.
//!
//! Clients store plaintext locally; the ciphertext that crosses a boundary is re-derived on
//! demand from the manifest's recorded `nonce_prefix` (the same trick
//! [`export_backup`](Workspace::export_backup) has always used, and which now runs through
//! this one accessor so there is exactly one copy of that crypto).
//!
//! **This is a post-import pass, not an implementation of
//! [`AssetUploader`](crate::import::AssetUploader).** That trait cannot be
//! implemented: `stream_candidate` holds `&mut Workspace` across the `uploader.upload(...)`
//! call, so an implementor can never borrow the workspace back to read the bytes it is
//! supposed to send. Do not try again — push reads a *committed* asset out of an
//! immutably-borrowed workspace, which is why it is a second command
//! (`capsule push`) rather than a flag on import.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;

use capsule_core::cbor;
use capsule_core::crypto::encryption::{
    AssetEncryption, CIPHERTEXT_CHUNK, CiphertextReader, NONCE_PREFIX_LEN, StreamError, TAG_LEN,
    blob_ciphertext_hash, ciphertext_len, encrypt_asset_vec_with_prefix, encrypt_asset_with_prefix,
};
use capsule_core::crypto::hash::{self, Hash32};
use capsule_core::crypto::provenance::{
    Action, DerivativeManifest, DerivativeRole, KeyMode, ManifestCore,
};
use capsule_core::derivative_format::{DerivativeFormat, verify_still_format};
use uuid::Uuid;

use super::{AlbumKeys, AssetState, LifecycleError, Result, Workspace, media_dir};
use crate::db::upload_guard::UploadGuardRow;

/// The original media types the upload protocol's closed `content_type` enum names for this
/// protocol version (server-side validation, invariant 5). Every other blob — and every
/// original whose type is not here — declares [`OPAQUE_CONTENT_TYPE`], which the enum also
/// names.
///
/// The enum is closed per protocol version, so it cannot name every format a user owns. An
/// original is never refused for its type (SSoT: thumbnails § Media Is Rawshift's): a RAW, an
/// AVI, a Matroska video or a file of no known kind uploads as opaque bytes, and its true type
/// travels inside the encrypted sidecar, where a restore reads it back. The E2E suite fails if
/// this list names a type the server's default enum does not.
pub const WIRE_ORIGINAL_CONTENT_TYPES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/heic",
    "image/heif",
    "image/webp",
    "image/avif",
    "image/jxl",
    "image/gif",
    "image/tiff",
    "video/mp4",
    "video/quicktime",
    "video/webm",
];

/// The `content_type` of a blob the server is not told the kind of.
pub const OPAQUE_CONTENT_TYPE: &str = "application/octet-stream";

/// The `content_type` an original of media type `content_type` declares on the wire: itself
/// when the protocol's closed enum names it, [`OPAQUE_CONTENT_TYPE`] otherwise.
#[must_use]
pub fn wire_content_type(content_type: &str) -> &str {
    if WIRE_ORIGINAL_CONTENT_TYPES.contains(&content_type) {
        content_type
    } else {
        OPAQUE_CONTENT_TYPE
    }
}

/// One derivative blob of an asset bundle: the **ciphertext** plus the content address its
/// signed [`DerivativeManifest`] committed to.
#[derive(Debug, Clone)]
pub struct DerivativeBlob {
    /// Which derivative this is (`thumbnail` / `preview` / `embedding`).
    pub role: DerivativeRole,
    /// The derivative's MIME/format string, e.g. `image/avif`.
    pub format: String,
    /// The AMK epoch the derivative manifest was authorized under, when it recorded one.
    pub amk_version: Option<u32>,
    /// The derivative's transferable bytes — **ciphertext**, re-derived from the plaintext the
    /// library holds using the nonce prefix the manifest recorded. Derivative bytes are
    /// encrypted client-side exactly like the original
    /// ([Encryption](https://docs/design/cryptography/encryption/)).
    pub bytes: Vec<u8>,
    /// The content address the signed derivative manifest committed to.
    pub ciphertext_hash: Hash32,
}

/// Everything needed to push one managed asset: its identity, the re-derived original
/// ciphertext, the exact sealed metadata blob, any derivative blobs, and the manifest-envelope
/// projection fields the server validates. Carries no [`Workspace`] internals and no album key
/// material — the AMK never leaves the workspace. The one secret it holds is the original's
/// per-file key, privately inside [`OriginalCiphertext`], which can open that one file and
/// nothing else.
///
/// Every field maps one-for-one onto the upload protocol's `POST /upload` body
/// (`capsule_sdk::push` owns that mapping).
#[derive(Debug, Clone)]
pub struct UploadBundle {
    /// The asset id — equal to the manifest's `file_id`.
    pub asset_id: Uuid,
    /// The album the asset belongs to.
    pub album_id: Uuid,
    /// The primitive bundle the manifest was produced under.
    pub crypto_suite_id: u16,
    /// The date-based wire protocol version the manifest is pinned to.
    pub protocol_version: String,
    /// The AMK epoch the asset is sealed under.
    pub amk_version: u32,
    /// The original's ciphertext, re-derived on demand from the plaintext on disk and the
    /// manifest's recorded `nonce_prefix`. It is never held whole: see [`OriginalCiphertext`].
    pub original: OriginalCiphertext,
    /// The original ciphertext's content address (equal to the manifest's `ciphertext_hash`).
    pub ciphertext_hash: Hash32,
    /// Total plaintext byte length of the original.
    pub plaintext_size: u64,
    /// Plaintext bytes per STREAM chunk.
    pub chunk_size: u32,
    /// The original's `content_type` **as declared on the wire**: the signed sidecar's MIME
    /// type when the upload protocol's closed enum names it, otherwise the opaque
    /// `application/octet-stream` ([`wire_content_type`]). The sidecar keeps the true type;
    /// this is only what the server is told.
    pub content_type: String,
    /// How the file key is obtained (`derived` / `wrapped`).
    pub key_mode: KeyMode,
    /// The **exact** sealed metadata-blob wire bytes the manifest commits to.
    pub metadata_blob: Vec<u8>,
    /// The **exact** provenance-blob wire bytes: the canonical CBOR of the chain's `create`
    /// [`ProvenanceRecord`](capsule_core::crypto::provenance::ProvenanceRecord), its genesis.
    /// The records after it are not uploaded: they reach the server as lifecycle ops once it
    /// has indexed this one.
    ///
    /// This is the asset's *envelope object* — the `provenance` blob of
    /// [provenance.md § Physical Storage](https://docs/design/cryptography/provenance/#physical-storage),
    /// stored by the server verbatim and served back unchanged on the sync feed. It is the
    /// record and not the bare manifest because the server's chain head is the SHA-256 of these
    /// bytes, while a client's next `prior_provenance_hash` is `record_hash()` — the digest of
    /// the canonical record — so no other encoding lets a later lifecycle op chain onto what
    /// this upload established. The manifest travels inside it, canonically encoded and
    /// therefore byte-identical to its own signed bytes: "the signed bytes are the served
    /// bytes" holds through the wrapper.
    ///
    /// Always present, unlike [`metadata_blob_hash`](Self::metadata_blob_hash): a managed asset
    /// always has a create record, even when it binds no metadata blob.
    pub provenance_blob: Vec<u8>,
    /// The content address of the sealed metadata blob, when the create binds one.
    pub metadata_blob_hash: Option<Hash32>,
    /// The asset's derivative blobs, if any were generated and persisted.
    pub derivatives: Vec<DerivativeBlob>,
    /// The **exact** `derivative_manifest`-blob wire bytes: the canonical CBOR of each signed
    /// [`DerivativeManifest`] a receiver needs to open this bundle's derivative tiers (#556).
    ///
    /// One per shipped [`DerivativeBlob`], plus one per `format = "original"` sentinel tier,
    /// which has no blob of its own and is *only* this manifest: without it a receiver could
    /// not tell "this tier is the original" from "this tier is missing". In the order the
    /// library recorded them. A manifest whose derivative is skipped is skipped with it, so
    /// nothing here names a blob the bundle does not carry.
    ///
    /// Signed CBOR, not ciphertext: like [`Self::provenance_blob`] it is deliberately
    /// server-visible (it holds no plaintext secret by construction), and it is served back
    /// unchanged so the receiver verifies the bytes the producer signed.
    pub derivative_manifests: Vec<Vec<u8>>,
    /// The authoring user.
    pub created_by_user: Uuid,
    /// The authoring device.
    pub created_by_device: Uuid,
    /// The exact client build that authored the create manifest.
    pub client_version: String,
    /// RFC3339 authoring timestamp of the create manifest.
    pub timestamp: String,
    /// The create manifest's lifecycle action: `create`.
    pub action: Action,
    /// The create manifest's prior-provenance link: null, as for every `create`.
    pub prior_provenance_hash: Option<Hash32>,
    /// The create manifest's retention floor: unset, as it is set only for `delete`.
    pub retention_until: Option<String>,
}

impl UploadBundle {
    /// The original ciphertext's size in bytes — what `POST /upload` declares as `size` for
    /// the original blob.
    #[must_use]
    pub fn ciphertext_size(&self) -> u64 {
        self.original.size()
    }
}

/// The original's ciphertext as a **source**, not a buffer: the plaintext's path on disk, the
/// per-file key and the nonce prefix the signed manifest recorded.
///
/// Every read re-derives the requested ciphertext window from the plaintext
/// ([`CiphertextReader`]), so pushing an original holds one transfer window of it, never the
/// file — an original may be as large as the server accepts (4 GiB) and the device it is pushed
/// from may be a phone. The bytes it yields are the bytes [`Workspace::upload_bundle`] verified
/// against the manifest's `ciphertext_hash` when it built the bundle.
///
/// It carries the two source guards of
/// [Encryption — Encryption Workflow](https://docs/design/cryptography/encryption/#encryption-workflow),
/// both recorded by the hashing pass that verified it:
///
/// - **Source identity**, best effort: the plaintext file's length, modification time and, on
///   Unix, inode and status-change time as they stood when that pass began, so
///   [`check_unchanged`](Self::check_unchanged) answers cheaply whether the file is still the one
///   verified.
/// - **Chunk tags**, authoritative: every STREAM chunk's authentication tag.
///   [`read_at`](Self::read_at) re-derives every whole chunk a window touches and refuses the
///   read when any tag differs from the recorded one, so a window it returns is made only of
///   chunks byte-identical to the verified ciphertext's, and re-sending it puts no second
///   ciphertext under a nonce, whatever happened to the file.
#[derive(Clone)]
pub struct OriginalCiphertext {
    path: PathBuf,
    file_key: [u8; 32],
    nonce_prefix: [u8; NONCE_PREFIX_LEN],
    plaintext_size: u64,
    identity: FileIdentity,
    /// Every STREAM chunk's tag, in chunk order, as the verifying pass sealed it.
    tags: Arc<[ChunkTag]>,
}

/// One STREAM chunk's authentication tag.
type ChunkTag = [u8; TAG_LEN];

/// A writer that passes a STREAM ciphertext through to `inner` and records the tag that ends
/// each chunk: the last [`TAG_LEN`] bytes of every [`CIPHERTEXT_CHUNK`], and of the shorter
/// final chunk.
struct TagRecorder<W> {
    inner: W,
    /// Ciphertext bytes seen so far.
    offset: u64,
    /// The last (up to [`TAG_LEN`]) bytes of the chunk being written.
    tail: Vec<u8>,
    tags: Vec<ChunkTag>,
}

impl<W: Write> TagRecorder<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            offset: 0,
            tail: Vec::with_capacity(TAG_LEN),
            tags: Vec::new(),
        }
    }

    fn record(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let in_chunk = (self.offset % CIPHERTEXT_CHUNK as u64) as usize;
            let room = CIPHERTEXT_CHUNK - in_chunk;
            let (piece, rest) = bytes.split_at(room.min(bytes.len()));
            if piece.len() >= TAG_LEN {
                self.tail.clear();
                self.tail.extend_from_slice(&piece[piece.len() - TAG_LEN..]);
            } else {
                self.tail.extend_from_slice(piece);
                let excess = self.tail.len().saturating_sub(TAG_LEN);
                self.tail.drain(..excess);
            }
            self.offset += piece.len() as u64;
            if piece.len() == room {
                self.close_chunk();
            }
            bytes = rest;
        }
    }

    fn close_chunk(&mut self) {
        let mut tag = [0u8; TAG_LEN];
        tag.copy_from_slice(&self.tail);
        self.tags.push(tag);
        self.tail.clear();
    }

    /// The tags of every chunk written, closing a final chunk shorter than a whole one.
    fn finish(mut self) -> io::Result<Vec<ChunkTag>> {
        match self.tail.len() {
            0 => {}
            TAG_LEN => self.close_chunk(),
            short => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("a STREAM chunk ended {short} bytes in, shorter than its tag"),
                ));
            }
        }
        Ok(self.tags)
    }
}

impl<W: Write> Write for TagRecorder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.record(&buf[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The plaintext on disk is no longer the file an [`OriginalCiphertext`] was verified from.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the original changed on disk since its ciphertext was verified: {reason}")]
pub struct OriginalChanged {
    /// What differs, in English, for logs.
    pub reason: String,
}

/// What a plaintext file looked like on disk: its length, its modification time and, on Unix,
/// its device and inode (a file replaced by a rename is another inode) and its status-change
/// time (which a write bumps even when the modification time is set back).
///
/// A cheap stand-in for the content: an edit that keeps every one of these is not seen. The
/// per-chunk authentication tags recorded beside it close that gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    len: u64,
    modified: Option<std::time::SystemTime>,
    file_id: Option<(u64, u64)>,
    status_changed: Option<(i64, i64)>,
}

impl FileIdentity {
    /// The identity of the file at `path` now.
    fn of(path: &std::path::Path) -> io::Result<Self> {
        let meta = fs::metadata(path)?;
        #[cfg(unix)]
        let (file_id, status_changed) = {
            use std::os::unix::fs::MetadataExt as _;
            (
                Some((meta.dev(), meta.ino())),
                Some((meta.ctime(), meta.ctime_nsec())),
            )
        };
        #[cfg(not(unix))]
        let (file_id, status_changed) = (None, None);
        Ok(Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            file_id,
            status_changed,
        })
    }

    /// What differs between `self` (as recorded) and `now`, or `None` when nothing does.
    fn difference(&self, now: &Self) -> Option<String> {
        if self.len != now.len {
            return Some(format!("its length is {} bytes, not {}", now.len, self.len));
        }
        if self.file_id != now.file_id {
            return Some("it was replaced by another file".to_owned());
        }
        if self.modified != now.modified {
            return Some("its modification time changed".to_owned());
        }
        if self.status_changed != now.status_changed {
            return Some("its status-change time changed".to_owned());
        }
        None
    }

    /// The modification time in nanoseconds since the Unix epoch, when it is representable.
    fn modified_ns(&self) -> Option<i64> {
        self.modified
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|since| i64::try_from(since.as_nanos()).ok())
    }

    /// The identity a persisted guard recorded. A modification time the row could not
    /// represent reads back as unknown.
    fn of_row(row: &UploadGuardRow) -> Self {
        Self {
            len: row.source_len as u64,
            modified: row
                .modified_ns
                .and_then(|ns| u64::try_from(ns).ok())
                .map(|ns| std::time::UNIX_EPOCH + std::time::Duration::from_nanos(ns)),
            file_id: row
                .file_dev
                .zip(row.file_ino)
                .map(|(dev, ino)| (dev as u64, ino as u64)),
            status_changed: row.ctime_secs.zip(row.ctime_nanos),
        }
    }
}

impl std::fmt::Debug for OriginalCiphertext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OriginalCiphertext")
            .field("path", &self.path)
            .field("file_key", &"<redacted>")
            .field("plaintext_size", &self.plaintext_size)
            .field("chunk_tags", &self.tags.len())
            .finish_non_exhaustive()
    }
}

impl OriginalCiphertext {
    /// The source over the plaintext at `path`, after one hashing pass over it: the file's
    /// identity is recorded first, so a change made during the pass differs from it, then the
    /// ciphertext is derived once, recording every chunk's tag. Returns what that pass hashed
    /// to, for the caller to check against the signed manifest; the guards it recorded are
    /// those of the bytes it hashed.
    fn seal(
        path: PathBuf,
        file_key: [u8; 32],
        nonce_prefix: [u8; NONCE_PREFIX_LEN],
        plaintext_size: u64,
    ) -> io::Result<(Self, AssetEncryption)> {
        let identity = FileIdentity::of(&path)?;
        let mut recorder = TagRecorder::new(io::sink());
        let sealed = encrypt_with(&path, &file_key, nonce_prefix, &mut recorder)?;
        let tags = recorder.finish()?;
        let original = Self {
            path,
            file_key,
            nonce_prefix,
            plaintext_size,
            identity,
            tags: tags.into(),
        };
        Ok((original, sealed))
    }

    /// The guard row this original persists for `asset_id`, recorded at `recorded_at`.
    fn guard_row(&self, asset_id: &Uuid, recorded_at: i64) -> UploadGuardRow {
        UploadGuardRow {
            asset_id: asset_id.to_string(),
            nonce_prefix: self.nonce_prefix.to_vec(),
            plaintext_size: self.plaintext_size as i64,
            source_len: self.identity.len as i64,
            modified_ns: self.identity.modified_ns(),
            file_dev: self.identity.file_id.map(|(dev, _)| dev as i64),
            file_ino: self.identity.file_id.map(|(_, ino)| ino as i64),
            ctime_secs: self.identity.status_changed.map(|(secs, _)| secs),
            ctime_nanos: self.identity.status_changed.map(|(_, nanos)| nanos),
            chunk_tags: self.tags.iter().flatten().copied().collect(),
            recorded_at,
        }
    }

    /// Whether the plaintext on disk is still the file this ciphertext was verified from: the
    /// same length, modification time and (on Unix) inode and status-change time. One `stat`,
    /// cheap enough to run before every window is read and again after.
    ///
    /// `Ok` is a positive claim that every window read while it holds is the window read
    /// before, up to one limit: an edit that keeps every one of those attributes is not seen.
    ///
    /// # Errors
    ///
    /// [`OriginalChanged`] naming what differs, or that the file can no longer be read.
    pub fn check_unchanged(&self) -> std::result::Result<(), OriginalChanged> {
        let now = FileIdentity::of(&self.path).map_err(|error| OriginalChanged {
            reason: format!("it can no longer be read: {error}"),
        })?;
        match self.identity.difference(&now) {
            None => Ok(()),
            Some(reason) => {
                tracing::warn!(path = %self.path.display(), %reason, "the original changed on disk since its ciphertext was verified");
                Err(OriginalChanged { reason })
            }
        }
    }

    /// The ciphertext's length in bytes — what `POST /upload` declares as `size`.
    #[must_use]
    pub fn size(&self) -> u64 {
        ciphertext_len(self.plaintext_size)
    }

    /// The ciphertext bytes `[offset, offset + len)`, clamped to the end, cut from whole STREAM
    /// chunks each checked against the tag the verifying pass recorded. Memory is the window
    /// plus up to three STREAM chunks.
    ///
    /// # Errors
    ///
    /// An [`io::Error`] carrying [`OriginalChanged`] (`io::Error::other`) when a chunk the window
    /// touches re-derives to a tag other than the recorded one: the plaintext under it changed,
    /// and none of the window is returned. Any other read failure as it occurred.
    #[tracing::instrument(level = "trace", skip(self), fields(path = %self.path.display()))]
    pub fn read_at(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let total = self.size();
        let end = offset.saturating_add(len as u64).min(total);
        if offset >= end {
            return Ok(Vec::new());
        }
        let chunk = CIPHERTEXT_CHUNK as u64;
        let first = offset / chunk;
        let last = (end - 1) / chunk;
        let from = first * chunk;
        let to = ((last + 1) * chunk).min(total);
        let file = fs::File::open(&self.path)?;
        let mut whole =
            CiphertextReader::new(self.file_key, self.nonce_prefix, file, self.plaintext_size)
                .read_at(from, (to - from) as usize)?;
        if whole.len() as u64 != to - from {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the original re-derived to fewer bytes than its chunks hold",
            ));
        }
        for index in first..=last {
            let at = (((index + 1) * chunk).min(total) - from) as usize;
            let sealed = &whole[at - TAG_LEN..at];
            let recorded = usize::try_from(index)
                .ok()
                .and_then(|index| self.tags.get(index));
            if recorded.map(<[u8; TAG_LEN]>::as_slice) != Some(sealed) {
                let reason =
                    format!("STREAM chunk {index} re-derived to another authentication tag");
                tracing::warn!(path = %self.path.display(), offset, len, chunk = index, "the original changed on disk since its ciphertext was verified: a re-derived chunk's tag differs; refusing the read");
                return Err(io::Error::other(OriginalChanged { reason }));
            }
        }
        let start = (offset - from) as usize;
        whole.truncate(start + (end - offset) as usize);
        whole.drain(..start);
        tracing::trace!(
            offset,
            len = whole.len(),
            chunks = last - first + 1,
            "re-derived window checked against its chunk tags"
        );
        Ok(whole)
    }

    /// How many STREAM chunk tags this original's guard holds: one per chunk of the verified
    /// ciphertext. For logs and diagnostics.
    #[must_use]
    pub fn chunk_tags(&self) -> usize {
        self.tags.len()
    }

    /// Stream the whole ciphertext into `writer`, one STREAM chunk at a time, and return its
    /// content address.
    pub fn write_to<W: Write>(&self, writer: W) -> io::Result<hash::Hash32> {
        let rederived = self.encrypt_into(writer)?;
        if rederived.plaintext_size != self.plaintext_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the original changed size since its manifest was signed",
            ));
        }
        Ok(rederived.ciphertext_hash)
    }

    /// The whole ciphertext in memory. For small originals and tests only: it is exactly the
    /// buffer this type exists to avoid.
    pub fn to_vec(&self) -> io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(usize::try_from(self.size()).unwrap_or(0));
        self.write_to(&mut out)?;
        Ok(out)
    }

    fn encrypt_into<W: Write>(&self, writer: W) -> io::Result<AssetEncryption> {
        encrypt_with(&self.path, &self.file_key, self.nonce_prefix, writer)
    }
}

/// STREAM-encrypt the plaintext at `path` into `writer` under `file_key` and `nonce_prefix`.
fn encrypt_with<W: Write>(
    path: &std::path::Path,
    file_key: &[u8; 32],
    nonce_prefix: [u8; NONCE_PREFIX_LEN],
    writer: W,
) -> io::Result<AssetEncryption> {
    let file = io::BufReader::new(fs::File::open(path)?);
    encrypt_asset_with_prefix(file_key, nonce_prefix, file, writer).map_err(|error| match error {
        StreamError::Io(io) => io,
        other @ StreamError::Auth(_) => io::Error::other(other),
    })
}

/// A backup export streams the original into the artifact through [`OriginalCiphertext::write_to`],
/// one STREAM chunk at a time, exactly as a push reads it.
impl capsule_core::backup::CiphertextSource for OriginalCiphertext {
    fn size(&self) -> u64 {
        OriginalCiphertext::size(self)
    }

    fn write_to(&self, writer: &mut dyn Write) -> io::Result<hash::Hash32> {
        OriginalCiphertext::write_to(self, writer)
    }
}

impl Workspace {
    /// Build the [`UploadBundle`] for one managed asset: its **`create` record** and the bytes
    /// that record commits to.
    ///
    /// An upload establishes an asset on the server, and the server takes nothing but a
    /// `create` through it. So the bundle carries the chain's genesis whatever the chain holds
    /// now: an asset deleted and restored, or edited, before its first upload is uploaded as the
    /// create it began as, and every record after it reaches the server as a lifecycle op once
    /// the create is indexed (Organization — Deletion State Machine, "Held records").
    ///
    /// Re-derives the original ciphertext from the create's recorded `nonce_prefix` (clients
    /// hold plaintext; ciphertext is regenerated deterministically) and gates it on the
    /// manifest's own content address, so a bundle that reaches the network is one the signed
    /// manifest vouches for. The sealed metadata blob the create binds is carried verbatim — it
    /// cannot be regenerated, because `seal_metadata_blob` draws a fresh nonce per call — so an
    /// asset edited since its import carries the copy its first edit kept, not its current blob.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::CreateMetadataSuperseded`] when the create binds a metadata blob the
    /// library no longer holds: an edit made before the library kept that blob replaced it.
    /// [`LifecycleError::CiphertextMismatch`] when the plaintext on disk no longer re-derives to
    /// the signed ciphertext. [`LifecycleError::Db`] when the original's source guard cannot be
    /// persisted.
    ///
    /// The hashing pass that verifies the original records its source guards (identity and
    /// every STREAM chunk's tag), and the bundle persists them in the catalog until
    /// [`release_upload_guard`](Self::release_upload_guard) or a purge forgets them.
    #[tracing::instrument(skip(self), fields(asset_id = %asset_id))]
    pub fn upload_bundle(&self, asset_id: &Uuid) -> Result<UploadBundle> {
        let asset = self
            .assets
            .get(asset_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("asset {asset_id}")))?;
        let album = self.album(&asset.album_id)?;
        let create_record = asset
            .chain
            .records()
            .first()
            .expect("provenance chain is never empty");
        let create = &create_record.manifest.core;
        // The envelope object, encoded once here so the SDK ladder and the FFI hand the wire
        // the same bytes. Canonical by construction: `record_hash()` is defined as the digest
        // of exactly this encoding, which is what makes the server's chain head and a client's
        // next `prior_provenance_hash` the same value.
        let provenance_blob = cbor::to_canonical_vec(create_record)
            .map_err(|e| LifecycleError::Cbor(format!("encoding the provenance record: {e}")))?;

        let metadata_blob = self.create_metadata_blob(asset, create.metadata_blob_hash)?;

        let epoch = create.amk_version.0;
        let original = self.verified_original(asset, album, create)?;
        self.record_upload_guard(&asset.asset_id, &original)?;
        let ciphertext_hash = create.ciphertext_hash;

        let DerivativeSet {
            blobs: derivatives,
            manifests: derivative_manifests,
        } = self.derivative_blobs(asset, album, epoch);
        tracing::debug!(
            album_id = %asset.album_id,
            amk_version = epoch,
            ciphertext_bytes = original.size(),
            metadata_blob_bytes = metadata_blob.len(),
            provenance_blob_bytes = provenance_blob.len(),
            derivatives = derivatives.len(),
            derivative_manifests = derivative_manifests.len(),
            action = ?create.action,
            later_records = asset.chain.records().len() - 1,
            "upload bundle built"
        );

        Ok(UploadBundle {
            asset_id: asset.asset_id,
            album_id: asset.album_id,
            crypto_suite_id: create.crypto_suite_id,
            protocol_version: create.protocol_version.clone(),
            amk_version: epoch,
            original,
            ciphertext_hash,
            plaintext_size: create.plaintext_size,
            chunk_size: create.chunk_size,
            content_type: wire_content_type(&asset.sidecar.content_type).to_owned(),
            key_mode: create.key_mode,
            metadata_blob,
            metadata_blob_hash: create.metadata_blob_hash,
            provenance_blob,
            derivatives,
            derivative_manifests,
            created_by_user: create.created_by_user,
            created_by_device: create.created_by_device,
            client_version: create.client_version.clone(),
            timestamp: create.timestamp.clone(),
            action: create.action,
            prior_provenance_hash: create.prior_provenance_hash,
            retention_until: create.retention_until.clone(),
        })
    }

    /// The exact sealed metadata blob a create binding `bound` uploads with: the asset's current
    /// blob while no edit has replaced it, otherwise the copy the first edit kept
    /// ([`create_metadata_blob_path`](Self::create_metadata_blob_path)). A create that binds no
    /// blob carries the current one, as it always has.
    ///
    /// Either candidate is checked against `bound` before it is used, so the bytes returned are
    /// the ones the server accepts under that address.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::CreateMetadataSuperseded`] when neither answers `bound`: an edit made
    /// before the library kept the create's blob replaced it, and it is gone.
    /// [`LifecycleError::Io`] when the kept copy exists and cannot be read.
    pub(super) fn create_metadata_blob(
        &self,
        asset: &AssetState,
        bound: Option<Hash32>,
    ) -> Result<Vec<u8>> {
        let Some(bound) = bound else {
            return Ok(asset.metadata_blob.clone());
        };
        if blob_ciphertext_hash(&asset.metadata_blob) == bound {
            return Ok(asset.metadata_blob.clone());
        }
        let path = self.create_metadata_blob_path_of(asset);
        let kept = match fs::read(&path) {
            Ok(kept) => Some(kept),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(LifecycleError::Io(format!(
                    "the create's metadata blob {}: {e}",
                    path.display()
                )));
            }
        };
        match kept {
            Some(kept) if blob_ciphertext_hash(&kept) == bound => {
                tracing::debug!(
                    asset_id = %asset.asset_id,
                    records = asset.chain.records().len(),
                    "upload bundle: an edit superseded the create's metadata blob; carrying the \
                     kept copy"
                );
                Ok(kept)
            }
            kept => {
                tracing::warn!(
                    asset_id = %asset.asset_id,
                    records = asset.chain.records().len(),
                    kept_copy = kept.is_some(),
                    "upload bundle: the metadata blob the create binds was superseded by an edit \
                     and no kept copy answers it"
                );
                Err(LifecycleError::CreateMetadataSuperseded(asset.asset_id))
            }
        }
    }

    /// Persist `original`'s source guard as `asset_id`'s, so it outlives the bundle, the
    /// session and the process until the bundle is custody-confirmed or abandoned.
    ///
    /// The tags `original` holds were recorded by a pass that matched the signed content
    /// address, so they are the signed ciphertext's own. A guard already recorded under the same
    /// nonce prefix holds the same tags, and only its identity is refreshed, when the file's
    /// attributes moved without its content. One under another prefix belonged to a create this
    /// asset has since re-sealed, and is replaced. One whose tags differ under the same prefix
    /// was not recorded from the signed ciphertext; it is replaced by the verified one, loudly.
    fn record_upload_guard(&self, asset_id: &Uuid, original: &OriginalCiphertext) -> Result<()> {
        let db = &self.library.db;
        let id = asset_id.to_string();
        let row = original.guard_row(asset_id, jiff::Timestamp::now().as_second());
        let prior = db
            .upload_guard(&id)
            .map_err(|e| LifecycleError::Db(format!("reading the upload guard of {id}: {e}")))?;
        match prior {
            None => tracing::debug!(
                %asset_id,
                chunk_tags = original.chunk_tags(),
                "upload guard recorded"
            ),
            Some(prior) if prior.nonce_prefix != row.nonce_prefix => tracing::info!(
                %asset_id,
                chunk_tags = original.chunk_tags(),
                "upload guard replaced: the create was re-sealed under another nonce prefix"
            ),
            Some(prior) if prior.chunk_tags != row.chunk_tags => tracing::warn!(
                %asset_id,
                recorded_tags = prior.chunk_tags.len() / TAG_LEN,
                chunk_tags = original.chunk_tags(),
                "upload guard replaced: its recorded tags are not the verified ciphertext's"
            ),
            Some(prior) => {
                let recorded = FileIdentity::of_row(&prior);
                match recorded.difference(&original.identity) {
                    None => {
                        tracing::trace!(%asset_id, "upload guard already recorded");
                        return Ok(());
                    }
                    Some(reason) => tracing::debug!(
                        %asset_id,
                        %reason,
                        "upload guard identity refreshed: the original's content re-verified \
                         unchanged"
                    ),
                }
            }
        }
        db.upsert_upload_guard(&row)
            .map_err(|e| LifecycleError::Db(format!("recording the upload guard of {id}: {e}")))
    }

    /// Forget `asset_id`'s upload source guard: every blob of its bundle is custody-confirmed,
    /// or the bundle is abandoned. Returns whether one was recorded. The next
    /// [`upload_bundle`](Self::upload_bundle) records it again.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::Db`] when the catalog refuses the delete.
    pub fn release_upload_guard(&self, asset_id: &Uuid) -> Result<bool> {
        let released = self
            .library
            .db
            .remove_upload_guard(&asset_id.to_string())
            .map_err(|e| {
                LifecycleError::Db(format!("releasing the upload guard of {asset_id}: {e}"))
            })?;
        tracing::debug!(%asset_id, released, "upload guard released");
        Ok(released)
    }

    /// The original's ciphertext as `core` pins it, checked against `core`'s content address.
    ///
    /// The re-derivation is deterministic, so a mismatch means the library's bytes and its own
    /// signed manifest disagree — never ship or export that. Hashed in one streaming pass, so
    /// this holds one STREAM chunk of the original (and its 16-byte tag per chunk), whatever its
    /// size; that pass records the source guards the returned original checks its reads
    /// against. Every record of a chain pins the same original, so any of them may be `core`.
    pub(super) fn verified_original(
        &self,
        asset: &AssetState,
        album: &AlbumKeys,
        core: &ManifestCore,
    ) -> Result<OriginalCiphertext> {
        let path = self.media_path(asset);
        let (original, rederived) = OriginalCiphertext::seal(
            path.clone(),
            self.original_file_key(album, core)?,
            core.nonce_prefix,
            core.plaintext_size,
        )
        .map_err(|e| LifecycleError::Io(format!("{}: {e}", path.display())))?;
        if rederived.ciphertext_hash != core.ciphertext_hash
            || rederived.plaintext_size != core.plaintext_size
        {
            tracing::error!(
                asset_id = %asset.asset_id,
                expected = %core.ciphertext_hash.to_hex(),
                actual = %rederived.ciphertext_hash.to_hex(),
                expected_plaintext_size = core.plaintext_size,
                actual_plaintext_size = rederived.plaintext_size,
                "upload bundle: re-derived ciphertext does not match the manifest"
            );
            return Err(LifecycleError::CiphertextMismatch(asset.asset_id));
        }
        Ok(original)
    }

    /// The asset's persisted derivative blobs, read back from
    /// `media/{YYYY}/{YYYY-MM}/derivatives/` and **encrypted** for the wire.
    ///
    /// The library holds derivatives as plaintext, exactly as it holds the original: the local
    /// gallery paints them, and the ciphertext is re-derived here from the nonce prefix the
    /// signed manifest recorded — the same round trip
    /// [`upload_bundle`](Self::upload_bundle) performs for the original. So what leaves this
    /// function in [`DerivativeBlob::bytes`] is ciphertext, and the field's `ciphertext_hash`
    /// name is now true of it. A thumbnail is a recognisable low-resolution copy of a private
    /// photo; the encryption doc admits no exception for it.
    ///
    /// **Still roles only.** An embedding-role manifest is skipped at `debug!` and named as out
    /// of scope: its `embedding/{model_id}` grammar is not in the still format set, and
    /// `capsule_core::ml` produces no derivative manifest for this reader to have an opinion about.
    ///
    /// Five reasons a manifest is skipped rather than shipped, and two of them are quiet:
    ///
    /// - the `original` sentinel, which references the original blob and has no bytes of its
    ///   own — an **expected** absence, logged at `debug!`, whose manifest still ships;
    /// - an embedding-role manifest, out of scope as above — also `debug!`;
    /// - a still-role `format` outside the closed set, which is the structural rejection the
    ///   tier table specifies;
    /// - an `amk_version` naming an epoch this album does not hold, which would otherwise panic
    ///   on the key lookup;
    /// - bytes missing on disk, or bytes whose re-derived ciphertext does not match the signed
    ///   content address.
    ///
    /// None of them fails the bundle: the original and its metadata are what a backup must not
    /// lose, and a stale thumbnail is regenerable.
    ///
    /// Beside the blobs it returns the signed manifests a receiver opens them with (#556): one
    /// for each blob it keeps and one for each `original` sentinel, canonically encoded. A
    /// manifest whose derivative is skipped is skipped with it.
    fn derivative_blobs(&self, asset: &AssetState, album: &AlbumKeys, epoch: u32) -> DerivativeSet {
        let dir = media_dir(&self.root, asset.capture_utc).join("derivatives");
        let stem = asset.asset_id.simple().to_string();
        let bundle_path = dir.join(format!("{stem}.derivatives.cbor"));
        let Ok(bytes) = fs::read(&bundle_path) else {
            return DerivativeSet::default();
        };
        let manifests: Vec<DerivativeManifest> = match cbor::from_slice(&bytes) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    path = %bundle_path.display(),
                    error = %e,
                    "upload bundle: undecodable derivative manifest bundle; skipping derivatives"
                );
                return DerivativeSet::default();
            }
        };

        let mut set = DerivativeSet {
            blobs: Vec::with_capacity(manifests.len()),
            manifests: Vec::with_capacity(manifests.len()),
        };
        for manifest in manifests {
            let role_name = derivative_role_name(manifest.core.role);

            // The closed-format check the tier table calls a structural rejection. It answers
            // `Ok(None)` for an embedding-role manifest, whose `embedding/{model_id}` grammar
            // this set deliberately does not model, so those pass through untouched.
            match verify_still_format(&manifest) {
                Ok(Some(DerivativeFormat::Original)) => {
                    // A reference to the original blob, not a missing file: the receiver
                    // resolves it against the original it already has. `debug!`, because an
                    // expected absence logged as a warning trains people to ignore warnings.
                    // The manifest still ships: it is how the receiver learns the tier *is*
                    // the original rather than missing.
                    tracing::debug!(
                        asset_id = %asset.asset_id,
                        role = role_name,
                        "upload bundle: `original` sentinel references the original blob; \
                         shipping its manifest and no derivative bytes"
                    );
                    if let Some(encoded) = encode_manifest(&manifest, asset, role_name) {
                        set.manifests.push(encoded);
                    }
                    continue;
                }
                Ok(None) => {
                    // An embedding-role manifest. Its `embedding/{model_id}` grammar is not in
                    // the still format set, and nothing produces one yet: `capsule_core::ml` writes no
                    // derivative manifest at all. Rather than invent a reader for an artefact
                    // with no writer, this reader says plainly that embeddings are out of its
                    // scope — at `debug!`, because encountering one is not a fault.
                    tracing::debug!(
                        asset_id = %asset.asset_id,
                        role = role_name,
                        "upload bundle: embedding-role derivatives are outside this reader's \
                         scope until `capsule_core::ml` produces manifests for them; skipping"
                    );
                    continue;
                }
                Ok(Some(_)) => {}
                Err(format) => {
                    tracing::warn!(
                        asset_id = %asset.asset_id,
                        role = role_name,
                        %format,
                        "upload bundle: still-role derivative names a format outside the closed \
                         set; skipping"
                    );
                    continue;
                }
            }

            let core = &manifest.core;
            let Some(plaintext) = read_derivative_bytes(&dir, &stem, role_name, &core.format)
            else {
                tracing::warn!(
                    asset_id = %asset.asset_id,
                    role = role_name,
                    "upload bundle: derivative manifest has no bytes on disk; skipping"
                );
                continue;
            };

            // Re-derive the ciphertext from the prefix the manifest signed. The prefix is
            // folded into the file-key salt, so it selects the key as well as the nonces —
            // there is exactly one ciphertext this manifest can be describing.
            //
            // The epoch is **checked**, not indexed. It comes off an unverified `.cbor` on
            // disk, and `file_key` reaches `album.amks[&epoch]`, which panics on a missing key
            // — inside a function whose whole contract is that it never fails the bundle. An
            // album recovered from a backup holds only the epochs it escrowed, so a manifest
            // naming one it does not hold is reachable without any tampering at all.
            let key_epoch = core.amk_version.map_or(epoch, |v| v.0);
            if !album.amks.contains_key(&key_epoch) {
                tracing::warn!(
                    asset_id = %asset.asset_id,
                    role = role_name,
                    epoch = key_epoch,
                    "upload bundle: derivative manifest names an AMK epoch this album does not \
                     hold; skipping"
                );
                continue;
            }
            let file_key = self.file_key(album, key_epoch, &asset.asset_id, &core.nonce_prefix);
            let (_, ciphertext) =
                encrypt_asset_vec_with_prefix(&file_key, core.nonce_prefix, &plaintext);

            let observed = hash::hash_bytes(&ciphertext);
            if observed != core.ciphertext_hash {
                tracing::warn!(
                    asset_id = %asset.asset_id,
                    role = role_name,
                    "upload bundle: derivative bytes do not match their signed manifest; skipping"
                );
                continue;
            }
            let Some(encoded) = encode_manifest(&manifest, asset, role_name) else {
                continue;
            };
            set.manifests.push(encoded);
            set.blobs.push(DerivativeBlob {
                role: core.role,
                format: core.format.clone(),
                amk_version: core.amk_version.map(|v| v.0),
                bytes: ciphertext,
                ciphertext_hash: observed,
            });
        }
        set
    }
}

/// What [`Workspace::derivative_blobs`] reads back: the derivative ciphertexts and the signed
/// manifests that open them, as the two [`UploadBundle`] fields they become.
#[derive(Debug, Default)]
struct DerivativeSet {
    blobs: Vec<DerivativeBlob>,
    manifests: Vec<Vec<u8>>,
}

/// The canonical CBOR of one signed derivative manifest — the `derivative_manifest` blob's
/// bytes — or `None`, logged, when it will not encode.
///
/// Canonical for the reason the provenance blob is: one encoding, so the bytes this device
/// ships and the bytes every receiver verifies are the same bytes, and a re-push of an
/// unchanged library names the same content address.
fn encode_manifest(
    manifest: &DerivativeManifest,
    asset: &AssetState,
    role_name: &str,
) -> Option<Vec<u8>> {
    match cbor::to_canonical_vec(manifest) {
        Ok(encoded) => Some(encoded),
        Err(e) => {
            tracing::warn!(
                asset_id = %asset.asset_id,
                role = role_name,
                error = %e,
                "upload bundle: a derivative manifest will not encode; skipping its tier"
            );
            None
        }
    }
}

/// The on-disk / wire name of a derivative role (mirrors `persist_derivatives`' file naming).
fn derivative_role_name(role: DerivativeRole) -> &'static str {
    match role {
        DerivativeRole::Thumbnail => "thumbnail",
        DerivativeRole::Preview => "preview",
        DerivativeRole::Embedding => "embedding",
    }
}

/// The persisted bytes of one derivative, addressed by **(role, format)** rather than by role
/// alone.
///
/// Role alone was ambiguous the moment a tier could carry more than one format: with a JXL and
/// an AVIF thumbnail side by side, a prefix match would take whichever sorted first and then
/// content-address it against the *other* manifest, so both would be skipped as mismatched.
/// `#437` lands exactly that pair, so this is a latent break rather than a hypothetical one.
///
/// Returns `None` for any `format` outside the closed set — including the embedding-role
/// grammar, which has no still extension. The caller has already skipped both cases, so reaching
/// this with one is not expected; it answers `None` rather than asserting, because a reader that
/// panics on a manifest it merely does not understand is worse than one that ships nothing. A stale file left by a
/// retired format is simply never read — nothing enumerates the directory any more, so an
/// orphan is inert rather than a candidate, and it is regenerable by design.
fn read_derivative_bytes(
    dir: &std::path::Path,
    stem: &str,
    role_name: &str,
    format: &str,
) -> Option<Vec<u8>> {
    let extension = DerivativeFormat::parse(format)?.extension()?;
    fs::read(dir.join(format!("{stem}.{role_name}.{extension}"))).ok()
}

#[cfg(test)]
mod tests;
