//! The owner ledger: an append-only, hash-chained, attestation-key-signed log per owner.
//!
//! # What it is for
//!
//! [Server Filesystem § The Owner Ledger] defines it. Every signed manifest is a blob, and a blob
//! names everything an index row needs except **who owns it and in what order the server accepted
//! it**. The ledger carries those facts under `{blob_root}/ledger/`, beside the blobs, so the
//! PostgreSQL asset index can be rebuilt from the blob root rather than only restored from a
//! backup. Rebuild itself, and the scrub checks over the chain, are the ledger's consumers; this
//! module is the write side and the chain's verification.
//!
//! # Two entries per finalization, and why
//!
//! The design orders the write: an entry is appended and fsynced **after** the blobs it names
//! are committed and **before** the index row commits, so a crash between the two leaves an
//! entry without a row (replayable) and never a row without an entry. But the `sync_seq` and the
//! custody receipt are both minted *inside* the index transaction (see [`crate::attestation`]),
//! so no entry written before that transaction can hold them. Finalization therefore appends:
//!
//! 1. [`LedgerEvent::Custody`], before the index transaction: the content hash, the uploader,
//!    `received_at` and every fact the receipt will sign. Durable before the index is touched;
//!    a failure here fails the finalization, so no row exists without one.
//! 2. [`LedgerEvent::Indexed`], after the index transaction: the `sync_seq` it minted and the
//!    receipt it issued. A crash before this entry leaves a custody entry whose row did commit,
//!    which rebuild handles exactly like a lost tail: it replays the custody entry, and cannot
//!    prove the sequence number, so it bumps the owner's sync epoch.
//!
//! A write the index **refuses** after the custody entry is durable — a role already holding
//! another blob, a replace whose bundle is incomplete or whose chain is stale, an asset that is
//! gone — gets a third entry instead of the second: [`LedgerEvent::Refused`], naming the upload.
//! Without it a refusal would look exactly like the crash case, and a rebuild would replay a
//! write the server refused. So a custody entry is resolved by an `indexed` or a `refused`
//! entry for its upload, and only an **unresolved** one is replayed. The `refused` append is
//! best-effort, as the `indexed` one is; if it is lost, the replay re-runs the same checks the
//! index ran and refuses the write again, so the entry is the record rather than the safeguard.
//! A write the index could not *answer* (an outage, where whether it committed is unknown) gets
//! no entry: it is the crash case, and is replayed as one.
//!
//! A drop adoption is the other path by which a blob becomes indexed, and it appends the same
//! three kinds in the same order, under the drop id as its `upload_id`.
//!
//! # The other producers
//!
//! Every write that changes what a rebuild must reproduce appends before it commits, and a
//! failed append refuses the write:
//!
//! - a **lifecycle op** (`POST /v1/albums/{album_id}/ops`) appends [`LedgerEvent::Lifecycle`]
//!   with no `sync_seq` before the index call, and a second `Lifecycle` entry with the sequence
//!   number once the index has applied (or recognised a replay of) it — best-effort, as the
//!   `indexed` entry is. The manifest hash is the index's replay key, so replaying both entries
//!   applies the op once;
//! - a **device-directory publish** appends [`LedgerEvent::Directory`] before the directory
//!   store commits, under the publishing account.
//!
//! An op or a publish the index or the store then refuses leaves its entry unresolved: whatever
//! replays the ledger re-runs the checks that refused it. There is no entry kind for those
//! refusals because the entries they would resolve carry no session to name.
//!
//! # What makes tampering detectable
//!
//! Each entry's signed core carries its owner, its position (`seq`, from 1, contiguous) and the
//! hash of its predecessor's signed bytes, and is signed by the attestation key. So
//! [`verify_chain`] finds an edited entry (its signature fails, or its successor's link does),
//! a removed or reordered one (a sequence gap or a broken link), one moved between owners (the
//! owner field), a dropped *head* (the first entry must open the chain) and a torn tail (a
//! frame that does not decode). The one thing a chain cannot see on its own is a tail cut
//! exactly at an entry boundary: what is left is a valid shorter chain. That is detected
//! against an **anchor** — a head recorded outside the ledger — which is why [`verify_chain`]
//! and [`verify_links`] take one.
//!
//! # Where the anchor lives
//!
//! In the asset index, one `(seq, hash)` per owner ([`crate::index::AssetIndex::ledger_head`]).
//! [`AnchoredLedger`] wraps the ledger every producer appends to and records each entry's head
//! there once the append is durable, raising it and never lowering it. So the anchor is never
//! ahead of what the ledger durably held, and a ledger that later ends below it lost a tail
//! ([`ChainFault::BehindAnchor`]). Rebuild and the scrub read it per owner and pass it to the
//! verifier. Recording is best-effort, as the `indexed` entry is: a lost record leaves the
//! anchor behind the ledger, which weakens the check and never fakes a fault.
//!
//! The first append per owner in a process checks the ledger against its anchor and refuses the
//! write while the ledger is behind it or disagrees with it, so an adapter that reloads a cut
//! ledger after a restart cannot bury the loss under new entries ([`AnchoredLedger`]).
//!
//! The anchor survives exactly when the index does. A rebuild into an empty schema has none and
//! checks the chain alone; one alongside a restored backup checks against the head as it stood
//! at the backup. The one write that lowers it is a rebuild under `--apply` that has reported a
//! cut and moved the owner to a new sync epoch: it re-anchors at the head it verified, so the
//! loss is reported once and appends after it are not judged against entries that are gone.
//!
//! # The byte format
//!
//! A ledger is a sequence of frames, each a 4-byte big-endian length followed by that many bytes
//! of the entry's canonical CBOR. The filesystem adapter splits the sequence into segments
//! (`ledger/{owner_id}/{first_seq}.log`); the in-memory double keeps one buffer. Both share the
//! framing and the decoder below, so the conformance suite's tampering cases mean the same thing
//! on each.
//!
//! # Never rewritten, with one exception
//!
//! There is no operation that replaces or removes an entry. The only bytes an adapter ever cuts
//! are a **torn tail**: whatever follows the last whole entry when no complete entry starts
//! anywhere after it — a short frame, or the zeroes or stale blocks a crash can leave where an
//! append's frame was being written. Such bytes were never acknowledged — an append is durable
//! before it returns, and only one append per owner is in flight — so they were never an entry,
//! and cutting them is what lets the next append land rather than wedging the owner. Damage
//! followed by a complete entry is a hole in an acknowledged chain: that ledger refuses further
//! appends ([`LedgerError::Damaged`]) and is left for an operator.
//!
//! An **intact frame this server cannot read** is never a torn tail, wherever it sits. A frame
//! is intact when its length prefix is plausible and its body is one canonical CBOR item shaped
//! like an entry (a map holding a `core` map and a `server_sig`): an entry written by a newer
//! server, with a kind or a field this one does not know, looks exactly like that, and zeroes,
//! a short frame or stale blocks practically never do. Such a frame was acknowledged by the
//! server that wrote it, so cutting it would lose an entry; the read reports
//! [`ChainFault::Unrecognised`] and the ledger refuses appends, as the
//! [versioning invariants] require of a component that meets a record it does not understand.
//!
//! [versioning invariants]: https://github.com/Capsulsaurus/Capsule/blob/master/capsule-docs/src/content/docs/design/versioning.md#system-invariants
//!
//! [Server Filesystem § The Owner Ledger]: https://github.com/Capsulsaurus/Capsule/blob/master/capsule-docs/src/content/docs/design/filesystem/server.md#the-owner-ledger

pub mod anchored;
pub mod conformance;
pub mod fs;
pub mod manifest;
pub mod memory;
pub mod rebuild;
pub mod replay;

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::keys::{HybridSignature, HybridVerifyingKey};
use capsule_core::crypto::receipts::CustodyReceipt;
use jiff::Timestamp;
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

pub use self::anchored::AnchoredLedger;
pub use self::fs::FilesystemLedger;
pub use self::memory::InMemoryLedger;
use crate::attestation::ReceiptSigner;
use crate::store::OwnerId;

/// The schema version every entry this server writes carries.
pub const LEDGER_VERSION: &str = "owner-ledger/v1";

/// The largest frame a ledger may hold.
///
/// An entry is a few kilobytes: the hybrid signature dominates, and an indexed entry carries a
/// receipt with another. A length prefix beyond this is damage, not an entry, and refusing it
/// keeps a corrupted prefix from asking the decoder for gigabytes.
pub const MAX_FRAME_BYTES: u32 = 1024 * 1024;

/// How long an owner identifier may be before no adapter will name a ledger with it.
pub const MAX_OWNER_ID_LEN: usize = 64;

/// The future every ledger operation returns. Boxed so the port stays dyn-compatible.
pub type LedgerFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, LedgerError>> + Send + 'a>>;

/// What can go wrong reaching a ledger.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LedgerError {
    /// The backend could not carry out `operation`.
    #[error("the owner ledger could not {operation}: {detail}")]
    Backend {
        /// What was being attempted, for the log line.
        operation: &'static str,
        /// The backend's own description of the failure.
        detail: String,
    },

    /// The backend has no room left for `operation`.
    #[error("the owner ledger has no space left to {operation}")]
    StorageFull {
        /// What was being attempted, for the log line.
        operation: &'static str,
    },

    /// An owner identifier that cannot name a ledger.
    #[error("`{owner}` is not an identifier the owner ledger can name a log with")]
    MalformedOwner {
        /// The identifier that was refused.
        owner: OwnerId,
    },

    /// The owner's ledger is damaged somewhere other than a torn tail, so it takes no appends.
    ///
    /// Extending a chain past a break would bury the break under entries that verify, so the
    /// write is refused and the damage is left for an operator and the scrub to report.
    #[error("the ledger of {owner} is damaged and takes no appends: {fault}")]
    Damaged {
        /// The owner whose ledger is damaged.
        owner: OwnerId,
        /// What is wrong with it.
        fault: ChainFault,
    },
}

/// One fact the ledger records.
///
/// Internally tagged so a reader can tell the kinds apart without the version, and every field is
/// a value the server established rather than one a client declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum LedgerEvent {
    /// The server took custody of a blob, before the index recorded it.
    Custody {
        /// The session that produced custody.
        upload_id: String,
        /// The asset the blob belongs to.
        asset_id: String,
        /// The blob's role, as the wire spells it.
        blob_role: String,
        /// The content address the server recomputed over the bytes it committed.
        content_hash: Hash32,
        /// Those bytes' length.
        size: u64,
        /// The SHA-256 of the signed manifest, when this blob is one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        manifest_sha256: Option<Hash32>,
        /// The account the storage is attributed to.
        upload_user_id: String,
        /// The device that uploaded, when the manifest named one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uploader_device: Option<String>,
        /// The server's own clock at the commit, as the receipt will carry it.
        received_at: String,
    },

    /// The index recorded a custody event: the sequence number it minted and the receipt it
    /// issued, in the same transaction.
    Indexed {
        /// The session the custody entry named.
        upload_id: String,
        /// The asset the row belongs to.
        asset_id: String,
        /// The asset's `sync_seq` after the write, when the write minted or found one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sync_seq: Option<u64>,
        /// The custody receipt issued with the write, when one was. Boxed: it is most of the
        /// entry's size, and the other kinds should not carry its footprint.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt: Option<Box<CustodyReceipt>>,
    },

    /// The index refused a write whose custody entry is already durable.
    ///
    /// Resolves that custody entry, so a rebuild does not replay a write the server refused.
    Refused {
        /// The session the custody entry named.
        upload_id: String,
        /// The asset the write was for.
        asset_id: String,
        /// Why, in English, for an operator.
        reason: String,
    },

    /// A device-directory version the server accepted.
    ///
    /// Recorded so a rebuild verifies each manifest against the directory as it stood when the
    /// manifest was accepted.
    Directory {
        /// The account whose directory it is.
        user_id: String,
        /// The version the server accepted.
        directory_version: u64,
        /// The hash of the signed directory document.
        document_hash: Hash32,
    },

    /// A lifecycle action applied to an asset.
    Lifecycle {
        /// The asset the action applies to.
        asset_id: String,
        /// The action, as the wire spells it.
        action: String,
        /// The manifest that authorised it, when one did.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        manifest_hash: Option<Hash32>,
        /// The asset's `sync_seq` after the action.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sync_seq: Option<u64>,
    },
}

impl LedgerEvent {
    /// The kind, as it is tagged on the wire, for log lines.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Custody { .. } => "custody",
            Self::Indexed { .. } => "indexed",
            Self::Refused { .. } => "refused",
            Self::Directory { .. } => "directory",
            Self::Lifecycle { .. } => "lifecycle",
        }
    }
}

/// The signed part of an entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntryCore {
    /// [`LEDGER_VERSION`].
    pub version: String,
    /// The server that wrote it.
    pub server_id: String,
    /// The attestation key that signed it.
    pub server_key_id: Hash32,
    /// The owner whose ledger it belongs to. Signed, so an entry moved between owners is caught.
    pub owner_id: String,
    /// Its position in the owner's ledger, from 1.
    pub seq: u64,
    /// The hash of the predecessor's canonical bytes; `None` only at `seq` 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_entry_hash: Option<Hash32>,
    /// The server's clock when the entry was written.
    pub recorded_at: String,
    /// What happened.
    pub event: LedgerEvent,
}

impl LedgerEntryCore {
    /// The canonical bytes the signature is over.
    pub fn signing_bytes(&self) -> Vec<u8> {
        capsule_core::cbor::to_canonical_vec(self).expect("a ledger entry core serializes")
    }
}

/// One entry: its core and the attestation key's signature over it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// The signed facts.
    pub core: LedgerEntryCore,
    /// The attestation key's signature over [`LedgerEntryCore::signing_bytes`].
    pub server_sig: HybridSignature,
}

impl LedgerEntry {
    /// The entry's canonical bytes, which is what a frame carries.
    pub fn to_canonical_cbor(&self) -> Vec<u8> {
        capsule_core::cbor::to_canonical_vec(self).expect("a ledger entry serializes")
    }

    /// The hash the successor names as its `prior_entry_hash`.
    pub fn chain_hash(&self) -> Hash32 {
        hash_bytes(&self.to_canonical_cbor())
    }

    /// Whether `key` signed this entry.
    pub fn verify_under(&self, key: &HybridVerifyingKey) -> bool {
        key.verify(&self.core.signing_bytes(), &self.server_sig)
    }
}

/// The last entry of a chain: its position and its hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerHead {
    /// The entry's `seq`.
    pub seq: u64,
    /// Its [`LedgerEntry::chain_hash`].
    pub hash: Hash32,
}

/// What reading an owner's ledger produced, before any signature is checked.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LedgerScan {
    /// Every entry that decoded, in byte order, up to the first damage.
    pub entries: Vec<LedgerEntry>,
    /// The structural damage that stopped the read, if any.
    pub damage: Option<ChainFault>,
}

/// Why a chain stops verifying, or stopped decoding.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChainFault {
    /// The ledger ends inside a frame: a write was interrupted, or the tail was cut.
    #[error("the ledger ends inside a frame after {entries} whole entries")]
    TornTail {
        /// How many whole frames precede the tear.
        entries: usize,
    },
    /// A whole frame that is not an entry.
    #[error("the frame after {entries} whole entries does not decode as an entry")]
    Undecodable {
        /// How many whole entries precede it.
        entries: usize,
    },
    /// A whole frame shaped like an entry that this server cannot decode: most likely one a newer
    /// server wrote, with a kind or a field this one does not know.
    ///
    /// Never a torn tail, even at the end of the ledger: the server that wrote it acknowledged
    /// it, so it is refused rather than cut.
    #[error(
        "the frame after {entries} whole entries is an entry this server cannot read, perhaps one a newer server wrote"
    )]
    Unrecognised {
        /// How many whole entries precede it.
        entries: usize,
    },
    /// An entry at the wrong position: one was removed, reordered or duplicated.
    #[error("expected entry {expected}, found entry {found}")]
    SequenceGap {
        /// The position the chain required.
        expected: u64,
        /// The position the entry claims.
        found: u64,
    },
    /// An entry whose `prior_entry_hash` is not its predecessor's hash.
    #[error("entry {seq} does not link to its predecessor")]
    BrokenLink {
        /// The entry that does not link.
        seq: u64,
    },
    /// An entry that names another owner.
    #[error("entry {seq} belongs to another owner's ledger")]
    WrongOwner {
        /// The misplaced entry.
        seq: u64,
    },
    /// An entry signed by a key this server never published.
    #[error("entry {seq} names a key this server never published")]
    UnknownKey {
        /// The entry.
        seq: u64,
    },
    /// An entry whose signature does not verify: it was edited.
    #[error("entry {seq} does not verify under the key it names")]
    BadSignature {
        /// The entry.
        seq: u64,
    },
    /// The chain is shorter than a head known from elsewhere: its tail was cut.
    #[error("the chain ends at entry {found} but entry {anchored} is known to exist")]
    BehindAnchor {
        /// The last entry the chain holds.
        found: u64,
        /// The entry the anchor names.
        anchored: u64,
    },
    /// The chain reaches the anchor's position with a different entry there: it was rewritten.
    #[error("entry {seq} is not the entry the anchor names")]
    AnchorMismatch {
        /// The position the anchor names.
        seq: u64,
    },
}

/// What [`verify_chain`] established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainReport {
    /// How many entries, from the first, verify. Replay stops after them.
    pub verified: usize,
    /// The last of those entries, or `None` when none verify.
    pub head: Option<LedgerHead>,
    /// Why verification stopped short of the end, or `None` when the whole ledger verifies.
    pub fault: Option<ChainFault>,
}

/// An owner ledger: append, read back, list.
///
/// Append-only is the port's shape: no method replaces or removes an entry. See the module docs
/// for the one kind of bytes an adapter may cut.
pub trait OwnerLedger: fmt::Debug + Send + Sync {
    /// Allocate the next position in `owner`'s ledger, sign `event` there, and append it durably.
    ///
    /// One operation, for the reason [`crate::attestation::ReceiptLog::issue`] is one: the
    /// position and the link are inside the signed core, so two concurrent appends that each
    /// read the head and then wrote would fork the chain with both halves validly signed.
    /// Durable before it returns, so a caller may commit its index row next.
    fn append<'a>(
        &'a self,
        owner: &'a OwnerId,
        event: LedgerEvent,
        recorded_at: Timestamp,
        signer: &'a dyn ReceiptSigner,
    ) -> LedgerFuture<'a, LedgerEntry>;

    /// Every entry of `owner`'s ledger that decodes, and the damage that stopped the read.
    ///
    /// An owner with no ledger scans to nothing. Signatures are not checked here; that is
    /// [`verify_chain`]'s, which needs the published key history.
    fn scan<'a>(&'a self, owner: &'a OwnerId) -> LedgerFuture<'a, LedgerScan>;

    /// Every owner that has a ledger, in ascending order.
    fn owners(&self) -> LedgerFuture<'_, Vec<OwnerId>>;
}

/// Check that `owner` can name a ledger on any adapter.
///
/// The rule [`crate::blob::check_upload_id`] applies to a staged file, for the same reason: an
/// identifier that is a path segment on one backend must mean the same thing on every one, and
/// a traversal sequence must mean nothing anywhere.
///
/// # Errors
///
/// [`LedgerError::MalformedOwner`] unless `owner` is 1..=[`MAX_OWNER_ID_LEN`] characters of
/// ASCII alphanumerics, `-` or `_`.
pub fn check_owner_id(owner: &OwnerId) -> Result<(), LedgerError> {
    let text = owner.as_str();
    let acceptable = !text.is_empty()
        && text.len() <= MAX_OWNER_ID_LEN
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if acceptable {
        Ok(())
    } else {
        Err(LedgerError::MalformedOwner {
            owner: owner.clone(),
        })
    }
}

/// Sign `event` at the position after `head` in `owner`'s ledger.
///
/// The one place an entry is assembled, shared by every adapter, so two adapters cannot sign two
/// shapes of the same fact. The position is the adapter's to allocate under its own lock.
pub(crate) fn sign_entry(
    owner: &OwnerId,
    head: Option<LedgerHead>,
    event: LedgerEvent,
    recorded_at: Timestamp,
    signer: &dyn ReceiptSigner,
) -> LedgerEntry {
    let core = LedgerEntryCore {
        version: LEDGER_VERSION.to_owned(),
        server_id: signer.server_id().to_owned(),
        server_key_id: signer.key_id(),
        owner_id: owner.as_str().to_owned(),
        seq: head.map_or(1, |head| head.seq + 1),
        prior_entry_hash: head.map(|head| head.hash),
        recorded_at: recorded_at.to_string(),
        event,
    };
    LedgerEntry {
        server_sig: signer.sign(&core.signing_bytes()),
        core,
    }
}

/// Encode `entry` as one frame: a 4-byte big-endian length and the canonical bytes.
pub(crate) fn encode_frame(entry: &LedgerEntry) -> Vec<u8> {
    let body = entry.to_canonical_cbor();
    let len = u32::try_from(body.len()).expect("an entry is far smaller than 4 GiB");
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&body);
    frame
}

/// What [`decode_frames`] found: the entries, the byte length they span, and any damage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Decoded {
    /// Every entry before the first damage.
    pub(crate) entries: Vec<LedgerEntry>,
    /// The byte length of those entries' frames: where a torn tail begins.
    pub(crate) whole_len: usize,
    /// The damage, if any.
    pub(crate) damage: Option<ChainFault>,
}

/// Decode a ledger's frames, stopping at the first one that is incomplete or not an entry.
pub(crate) fn decode_frames(bytes: &[u8]) -> Decoded {
    let mut entries = Vec::new();
    let mut at = 0_usize;
    while at < bytes.len() {
        let Some(prefix) = bytes.get(at..at + 4) else {
            return torn(entries, at);
        };
        let len = u32::from_be_bytes([prefix[0], prefix[1], prefix[2], prefix[3]]);
        if len > MAX_FRAME_BYTES {
            return damaged(entries, bytes, at);
        }
        let start = at + 4;
        let Some(body) = bytes.get(start..start + len as usize) else {
            return torn(entries, at);
        };
        let Some(entry) = entry_in(body) else {
            if intact_frame(body) {
                return unrecognised(entries, at);
            }
            return damaged(entries, bytes, at);
        };
        entries.push(entry);
        at = start + len as usize;
    }
    Decoded {
        entries,
        whole_len: at,
        damage: None,
    }
}

/// The entry `body` holds, if it is the canonical encoding of one.
///
/// Canonical bytes only: an entry that decodes but re-encodes differently was not written by
/// this server, and its hash would not be the one its successor names.
fn entry_in(body: &[u8]) -> Option<LedgerEntry> {
    capsule_core::cbor::from_slice::<LedgerEntry>(body)
        .ok()
        .filter(|entry| entry.to_canonical_cbor() == body)
}

/// The shape every entry has: a `core` map and a signature over it.
///
/// Only the shape, so a frame a newer server wrote — an event kind or a field this server does
/// not know, inside `core` or beside it — still matches; what the fields hold is not read.
#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "core")]
    #[expect(
        clippy::zero_sized_map_values,
        reason = "only that `core` is a map with text keys is checked; its values are never read"
    )]
    _core: BTreeMap<String, IgnoredAny>,
    #[serde(rename = "server_sig")]
    _server_sig: IgnoredAny,
}

/// Whether `body` is an intact frame: one canonical CBOR item shaped like an entry.
///
/// Every entry this server decodes is one, and so is an entry a newer server wrote. Zeroes are
/// not CBOR at all, and stale bytes or a short frame are vanishingly unlikely to be a single
/// canonical map holding both envelope fields with nothing after it.
fn intact_frame(body: &[u8]) -> bool {
    capsule_core::cbor::canonicalize(body).is_ok_and(|canonical| canonical == body)
        && capsule_core::cbor::from_slice::<Envelope>(body).is_ok()
}

/// A read that met, at `at`, an intact frame this server cannot decode.
fn unrecognised(entries: Vec<LedgerEntry>, at: usize) -> Decoded {
    let count = entries.len();
    tracing::warn!(
        entries = count,
        "an owner ledger holds an intact entry this server cannot read; refusing rather than cutting it"
    );
    Decoded {
        entries,
        whole_len: at,
        damage: Some(ChainFault::Unrecognised { entries: count }),
    }
}

/// A read that met bytes at `at` that are not an intact frame.
///
/// A **torn tail** when nothing from `at` to the end holds an intact frame: a crash mid-append
/// can leave a frame's worth of zeroes or stale blocks rather than a short file, and those bytes
/// were never acknowledged any more than a short frame was. Anything else is a hole in the
/// middle of the chain — an acknowledged entry follows the damage, so cutting it would lose one.
fn damaged(entries: Vec<LedgerEntry>, bytes: &[u8], at: usize) -> Decoded {
    if holds_a_frame(&bytes[at..]) {
        let count = entries.len();
        return Decoded {
            entries,
            whole_len: at,
            damage: Some(ChainFault::Undecodable { entries: count }),
        };
    }
    torn(entries, at)
}

/// Whether any offset of `region` begins an intact frame: an entry, or one a newer server wrote.
///
/// Every offset, not frame boundaries: the damage may have destroyed the length prefix the
/// boundaries were derived from. Run only on a damaged ledger, and a length prefix that does
/// not fit the region is rejected before anything is decoded, so zero-filled bytes cost a pass.
fn holds_a_frame(region: &[u8]) -> bool {
    (0..region.len()).any(|offset| {
        let Some(prefix) = region.get(offset..offset + 4) else {
            return false;
        };
        let len = u32::from_be_bytes([prefix[0], prefix[1], prefix[2], prefix[3]]);
        if len == 0 || len > MAX_FRAME_BYTES {
            return false;
        }
        region
            .get(offset + 4..offset + 4 + len as usize)
            .is_some_and(intact_frame)
    })
}

/// A read that ended inside a frame.
fn torn(entries: Vec<LedgerEntry>, at: usize) -> Decoded {
    let count = entries.len();
    Decoded {
        entries,
        whole_len: at,
        damage: Some(ChainFault::TornTail { entries: count }),
    }
}

/// Check the structure an appender relies on: positions from 1, contiguous, linked, one owner.
///
/// Returns the head, or the first fault. No signature is checked: the appender trusts what it
/// wrote, and this is what stops it extending a chain that was edited behind it.
pub(crate) fn link_check(
    owner: &OwnerId,
    entries: &[LedgerEntry],
) -> Result<Option<LedgerHead>, ChainFault> {
    let mut head: Option<LedgerHead> = None;
    for entry in entries {
        link_one(owner, head, entry)?;
        head = Some(LedgerHead {
            seq: entry.core.seq,
            hash: entry.chain_hash(),
        });
    }
    Ok(head)
}

/// Whether `entry` may follow `head` in `owner`'s ledger.
fn link_one(
    owner: &OwnerId,
    head: Option<LedgerHead>,
    entry: &LedgerEntry,
) -> Result<(), ChainFault> {
    let expected = head.map_or(1, |head| head.seq + 1);
    if entry.core.seq != expected {
        return Err(ChainFault::SequenceGap {
            expected,
            found: entry.core.seq,
        });
    }
    if entry.core.prior_entry_hash != head.map(|head| head.hash) {
        return Err(ChainFault::BrokenLink {
            seq: entry.core.seq,
        });
    }
    if entry.core.owner_id != owner.as_str() {
        return Err(ChainFault::WrongOwner {
            seq: entry.core.seq,
        });
    }
    Ok(())
}

/// Verify `owner`'s scanned ledger as far as it can be without the attestation key: positions,
/// links, owner, the decode damage the scan met, and the anchor.
///
/// What a maintenance host holding no key material can establish. It finds a removed,
/// reordered, moved or torn entry, a dropped head, and — given an anchor — a tail cut at an
/// entry boundary; it cannot find an entry edited by someone who also re-linked every
/// successor, which only the signatures catch. The report says nothing about which of the two
/// ran: a caller that has the published keys uses [`verify_chain`].
pub fn verify_links(owner: &OwnerId, scan: &LedgerScan, anchor: Option<LedgerHead>) -> ChainReport {
    walk(owner, scan, anchor, |_| None)
}

/// Verify `owner`'s scanned ledger: structure, links, signatures, and the anchor.
///
/// `resolve` maps a `server_key_id` to the published key it names
/// ([`crate::attestation::AttestationContext::resolve`]). `anchor` is the head recorded outside
/// the ledger ([`crate::index::AssetIndex::ledger_head`]); without one, a tail cut at an entry
/// boundary is indistinguishable from a shorter ledger (see the module docs).
///
/// The report's `verified` prefix is what a replay may trust; everything after the first fault
/// is not, however well it decodes.
pub fn verify_chain(
    owner: &OwnerId,
    scan: &LedgerScan,
    resolve: &dyn Fn(&Hash32) -> Option<HybridVerifyingKey>,
    anchor: Option<LedgerHead>,
) -> ChainReport {
    walk(owner, scan, anchor, |entry| {
        let Some(key) = resolve(&entry.core.server_key_id) else {
            return Some(ChainFault::UnknownKey {
                seq: entry.core.seq,
            });
        };
        (!entry.verify_under(&key)).then_some(ChainFault::BadSignature {
            seq: entry.core.seq,
        })
    })
}

/// The walk both verifiers share: links, then `check` on each linked entry, then the anchor.
///
/// One walk, so a key-less host and a keyed one judge an anchor the same way: an entry at the
/// anchor's position that is not the anchored one is [`ChainFault::AnchorMismatch`], and a chain
/// that ends below it is [`ChainFault::BehindAnchor`].
fn walk(
    owner: &OwnerId,
    scan: &LedgerScan,
    anchor: Option<LedgerHead>,
    check: impl Fn(&LedgerEntry) -> Option<ChainFault>,
) -> ChainReport {
    let mut head: Option<LedgerHead> = None;
    let mut verified = 0;
    let mut fault = None;

    for entry in &scan.entries {
        if let Err(found) = link_one(owner, head, entry) {
            fault = Some(found);
            break;
        }
        if let Some(found) = check(entry) {
            fault = Some(found);
            break;
        }
        let here = LedgerHead {
            seq: entry.core.seq,
            hash: entry.chain_hash(),
        };
        if let Some(anchor) = anchor
            && anchor.seq == here.seq
            && anchor.hash != here.hash
        {
            fault = Some(ChainFault::AnchorMismatch { seq: here.seq });
            break;
        }
        head = Some(here);
        verified += 1;
    }

    let fault = fault.or_else(|| scan.damage.clone()).or_else(|| {
        let found = head.map_or(0, |head| head.seq);
        anchor
            .filter(|anchor| anchor.seq > found)
            .map(|anchor| ChainFault::BehindAnchor {
                found,
                anchored: anchor.seq,
            })
    });

    if let Some(fault) = &fault {
        tracing::warn!(%owner, verified, anchored = ?anchor.map(|anchor| anchor.seq), %fault, "an owner ledger does not verify to its end");
    } else {
        tracing::debug!(%owner, verified, anchored = ?anchor.map(|anchor| anchor.seq), "an owner ledger verified");
    }

    ChainReport {
        verified,
        head,
        fault,
    }
}

#[cfg(test)]
mod tests;
