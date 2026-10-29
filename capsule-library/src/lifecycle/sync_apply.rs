//! Applying a **remote** asset entry — the receiving half of the lifecycle.
//!
//! Everything else in [`lifecycle`](super) authors state this device owns. This module is the
//! other direction: a signed manifest + sealed metadata blob that arrived from a server feed
//! (the [download & sync] `Sync` entry), turned into **verified facts** a client may upsert
//! into its own catalog — or into a quarantine verdict it must surface.
//!
//! # The chokepoint is not re-implemented here
//!
//! [`verify_asset`] is *the* single path by which an asset enters a client's trusted set, and
//! this module calls it — it does not re-derive, re-check, or re-order any of its twelve
//! steps. What this module adds is the orchestration a feed entry needs and a local import
//! does not:
//!
//! 1. decode the **provenance record** from the opaque canonical CBOR the feed carries
//!    verbatim (never re-encoded — re-encoding would detach the manifest inside it from its
//!    signatures), check the record's `prior_provenance_hash` against the manifest's, and
//!    refuse an asset this workspace purged (its tombstone, [`Workspace::tombstones`]);
//! 2. run [`verify_asset`] against this workspace's device directory, the album's attested
//!    authority, and the caller's local provenance head;
//! 3. bind the sealed metadata blob to the manifest (content address, then AEAD open under the
//!    nonce-folded blob key) and decode the signed [`SidecarV1`] inside it;
//! 4. check that sidecar's own user-IK signature;
//! 5. project the result into [`RemoteAssetFacts`].
//!
//! Every failure is a **verdict**, never a silent drop and never a silent accept: the three
//! [`SyncApplyOutcome`] variants are exactly `Applied` / `Pending` (hold and retry as MLS
//! state catches up) / `Quarantined`, mirroring [`VerifyOutcome`]'s own trichotomy.
//!
//! # Why the original ciphertext is required
//!
//! [`verify_asset`] step 5 is content integrity: the ciphertext must hash to the manifest's
//! declared `ciphertext_hash`. There is no variant of the chokepoint that skips it, and this
//! module does not invent one — so a caller applies an entry once it holds the original blob
//! (the T2 rung of the tier ladder). A client that has pulled only the T0 index rung *holds*
//! the entry; it does not yet *apply* it.
//!
//! An original too large to hold is applied as a [`RemoteHead`], which names the ciphertext by
//! its SHA-256 instead of carrying it, through [`verify_asset_hashed`] — the same twelve steps
//! over a digest. The digest is a claim until the bytes are read:
//! [`Workspace::open_remote_original_from`] hashes the ciphertext it streams and opens nothing
//! unless that hash is the one the chokepoint accepted.
//!
//! # Transport independence
//!
//! Nothing below names a transport. The gRPC sync feed, a REST re-fronting of the same feed, a
//! federation pull, and a LAN peering delta all deliver the identical three byte strings, so
//! these signatures are unaffected by which one a client speaks.
//!
//! # What the feed's `manifest_cbor` actually carries
//!
//! The **provenance blob's** bytes, unchanged — and the provenance blob is the canonical CBOR
//! of a [`ProvenanceRecord`], not of a bare [`AssetManifest`]. That is forced, not chosen:
//! [provenance.md § Physical Storage] makes the server-side chain an append-only sequence of
//! envelope objects "served back unchanged", and the server's chain head is the SHA-256 of
//! those bytes while a client's next `prior_provenance_hash` is
//! [`record_hash()`](capsule_core::crypto::provenance::ProvenanceRecord::record_hash) — the digest of
//! the canonical *record*. Any other encoding makes the two disagree, and no lifecycle op could
//! ever chain onto a synced asset.
//!
//! The manifest is not re-encoded by the wrapper: canonical CBOR is deterministic, so the
//! manifest sub-map inside the record is byte-identical to the manifest's own signed bytes, and
//! [provenance.md § Asset Manifest]'s "the signed bytes are the served bytes" holds through it.
//! [`verify_asset`] still verifies over the manifest's recomputed signing bytes, exactly as
//! before.
//!
//! The record's `prior_provenance_hash` is checked against the manifest's own before either is
//! used — [provenance.md § Chained, Append-Only Structure] calls the pair "a checked invariant,
//! not trusted redundancy", and this is the wire path where nothing else would check it.
//!
//! # The payload leads with its version
//!
//! `manifest_cbor` is decoded through [`ProvenanceRecord::decode`], which reads the record's
//! leading `version` before anything else (ADR-0009, superseding ADR-0008). A record from a
//! newer writer is quarantined as `UnsupportedRecordVersion` — kept for the caller to store
//! verbatim and retry after an update, never applied and never dropped. A byte string that is
//! not a record is quarantined as `MalformedManifest`, never guessed at. Within a version the
//! manifest keeps keys this build does not know under its signatures, so a newer writer that
//! only adds a field still verifies here. The sealed sidecar inside carries its own version
//! (`sidecar_schema`, refused forward below).
//!
//! [download & sync]: https://docs/design/sync/
//! [provenance.md § Asset Manifest]: https://docs/design/cryptography/provenance/#asset-manifest
//! [provenance.md § Chained, Append-Only Structure]: https://docs/design/cryptography/provenance/#chained-append-only-structure
//! [provenance.md § Physical Storage]: https://docs/design/cryptography/provenance/#physical-storage
//! [`verify_asset`]: capsule_core::crypto::verify_asset::verify_asset
//! [`verify_asset_hashed`]: capsule_core::crypto::verify_asset::verify_asset_hashed
//! [`VerifyOutcome`]: capsule_core::crypto::verify_asset::VerifyOutcome

use std::io::{Read, Write};

use capsule_core::crypto::encryption::{
    CIPHERTEXT_CHUNK, StreamDecryptor, StreamError, blob_ciphertext_hash, blob_nonce, open_blob,
};
use capsule_core::crypto::hash::{Hash32, Sha256Hasher, hash_bytes};
use capsule_core::crypto::keys::Amk;
use capsule_core::crypto::provenance::{
    Action, AssetManifest, ProvenanceRecord, RecordDecodeError,
};
use capsule_core::crypto::verify_asset::{
    BindingReject, PendingReason, RejectReason, VerifyOutcome, diagnose_misattributed_signer,
    verify_asset_hashed,
};
use capsule_core::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};
use uuid::Uuid;

use super::{LifecycleError, Result, Workspace};

/// One remote feed entry, exactly as a sync feed delivers it: opaque bytes plus the album the
/// entry claims and the head the receiving catalog already holds. Borrowed rather than owned so
/// a caller hands over buffers it already has without a copy.
#[derive(Debug, Clone, Copy)]
pub struct RemoteEntry<'a> {
    /// The album the feed says this entry belongs to. Cross-checked against the manifest by
    /// [`verify_asset`](capsule_core::crypto::verify_asset::verify_asset) itself
    /// ([`RejectReason::WrongAlbum`]), never trusted on its own.
    pub album_id: Uuid,
    /// The signed `AssetManifest` as **opaque canonical CBOR**, carried verbatim.
    pub manifest_cbor: &'a [u8],
    /// The sealed metadata blob the manifest commits to via `metadata_blob_hash`.
    pub metadata_blob: &'a [u8],
    /// The asset's original ciphertext — required for the chokepoint's content-integrity step.
    pub original_ciphertext: &'a [u8],
    /// The provenance head **the receiving catalog** holds for this asset, or `None` if it has
    /// never seen it. Passed straight through to
    /// [`verify_asset`](capsule_core::crypto::verify_asset::verify_asset)'s parameter of the same name,
    /// which is what decides replay ([`RejectReason::Replayed`]) and fork
    /// ([`RejectReason::ForgedChain`]).
    ///
    /// It is a caller parameter rather than something read off the workspace because the
    /// catalog that tracks synced assets is the client's, not this workspace's asset map — an
    /// iOS gallery, a CLI report, and a peering reconciler each hold their own head. Feed back
    /// [`RemoteAssetFacts::provenance_head`] after applying.
    pub local_chain_head: Option<Hash32>,
}

impl<'a> RemoteEntry<'a> {
    /// This entry with its original named by the SHA-256 of
    /// [`original_ciphertext`](Self::original_ciphertext) — a digest these bytes back, so the
    /// head is no weaker.
    #[must_use]
    pub fn head(&self) -> RemoteHead<'a> {
        RemoteHead {
            album_id: self.album_id,
            manifest_cbor: self.manifest_cbor,
            metadata_blob: self.metadata_blob,
            original_ciphertext_hash: hash_bytes(self.original_ciphertext),
            local_chain_head: self.local_chain_head,
        }
    }
}

/// A [`RemoteEntry`] whose original ciphertext is named by its SHA-256 rather than carried: what
/// a caller holds when the original is too large to keep in memory — a restore that streams it
/// to disk, say.
///
/// [`original_ciphertext_hash`](Self::original_ciphertext_hash) stands in for the bytes at the
/// chokepoint's content-integrity step, so on its own it is a claim. Applying a head therefore
/// vouches for the ciphertext only once something has hashed the bytes and found this digest:
/// [`Workspace::open_remote_original_from`] does, over exactly the bytes it decrypts.
#[derive(Debug, Clone, Copy)]
pub struct RemoteHead<'a> {
    /// As [`RemoteEntry::album_id`].
    pub album_id: Uuid,
    /// As [`RemoteEntry::manifest_cbor`].
    pub manifest_cbor: &'a [u8],
    /// As [`RemoteEntry::metadata_blob`].
    pub metadata_blob: &'a [u8],
    /// The SHA-256 of the asset's original ciphertext: the content address the chokepoint checks
    /// against the manifest's `ciphertext_hash`.
    pub original_ciphertext_hash: Hash32,
    /// As [`RemoteEntry::local_chain_head`].
    pub local_chain_head: Option<Hash32>,
}

/// The SHA-256 and length of the original ciphertext `ciphertext` yields, read one STREAM chunk
/// at a time: the [`RemoteHead::original_ciphertext_hash`] for an original that is on disk
/// rather than in memory, computed in memory bounded by one chunk whatever the original's size.
///
/// The digest is of exactly the bytes read, so a head built from it vouches for those bytes:
/// [`Workspace::apply_remote_head`] then checks it against the manifest's content address as
/// [`Workspace::apply_remote_entry`] checks the bytes themselves.
///
/// # Errors
///
/// [`LifecycleError::Io`] when `ciphertext` cannot be read.
pub fn hash_ciphertext_from<R: Read>(mut ciphertext: R) -> Result<(Hash32, u64)> {
    let mut hasher = Sha256Hasher::new();
    let mut window = vec![0u8; CIPHERTEXT_CHUNK];
    let mut read: u64 = 0;
    loop {
        let n = match ciphertext.read(&mut window) {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(LifecycleError::Io(format!(
                    "reading the original's ciphertext: {error}"
                )));
            }
        };
        hasher.update(&window[..n]);
        read += n as u64;
    }
    tracing::debug!(
        bytes = read,
        "sync-apply: original ciphertext hashed from a stream"
    );
    Ok((hasher.finalize(), read))
}

/// Why an entry was quarantined: surfaced to the user, never silently dropped.
///
/// Each variant names the check that refused it, so a client's quarantine surface can say what
/// happened rather than "sync failed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineReason {
    /// The manifest bytes are not a decodable `AssetManifest`.
    MalformedManifest(String),
    /// The provenance record declares a version newer than this build reads. Nothing about it
    /// was checked or applied; the caller keeps the entry verbatim and retries it once the user
    /// updates (versioning invariants 3 and 5).
    UnsupportedRecordVersion {
        /// The version the record declares.
        found: u64,
        /// The newest version this build reads.
        max_known: u16,
    },
    /// This workspace holds no key material or authority for the album the entry names, so
    /// there is nothing to verify it against. Not a rejection of the entry — a statement that
    /// this device cannot adjudicate it.
    UnknownAlbum(Uuid),
    /// [`verify_asset`](capsule_core::crypto::verify_asset::verify_asset) terminally rejected the
    /// manifest.
    Rejected(RejectReason),
    /// [`verify_asset`](capsule_core::crypto::verify_asset::verify_asset) rejected the manifest as
    /// [`RejectReason::BadDeviceSig`], and the
    /// rejection is the **legacy inherited-device defect** (issue #475), not a forgery: an older
    /// client's continuation names `named_device` (inherited from the chain head) but was signed
    /// by `signing_device`, another device of the same account, and every other step of the
    /// chokepoint holds (see [`diagnose_misattributed_signer`]).
    ///
    /// Still a quarantine: the record is never accepted, the caller's chain head does not
    /// advance, and every later record of this asset's chain is refused as
    /// [`RejectReason::ForgedChain`] behind it. `record_hash` is this record's hash — the value
    /// the *next* record's `prior_provenance_hash` carries — so a client can attribute that tail
    /// to this stall rather than to an attack. SSoT: [provenance.md § Legacy Inherited-Device
    /// Records].
    ///
    /// [provenance.md § Legacy Inherited-Device Records]: https://docs/design/cryptography/provenance/#legacy-inherited-device-records
    LegacyInheritedDevice {
        /// The device the record claims as its author (the chain head's, inherited).
        named_device: Uuid,
        /// The device of the same account whose DSK actually signed the record.
        signing_device: Uuid,
        /// The refused record's hash, where the asset's chain stalls.
        record_hash: Hash32,
    },
    /// The manifest verified, but its metadata blob does not bind to it.
    Binding(BindingReject),
    /// The metadata blob opened, but its plaintext is not a decodable `SidecarV1`.
    MalformedSidecar(String),
    /// The sidecar decoded, but its hybrid signature does not verify under the account's user
    /// identity key — the metadata was authored by something that does not hold this account's
    /// IK.
    SidecarSignature,
    /// This workspace purged the asset: its tombstone ([`Workspace::tombstones`]) refuses every
    /// later entry for it, whatever the entry says, so a feed cannot revive a purged asset. Not a
    /// verdict on the entry's authenticity — nothing past the record's decode was checked. The
    /// purge's own `delete` ([`Workspace::purge_record`]) is unaffected and can still be sent.
    Purged(Uuid),
}

/// The verified facts one remote entry contributes to the receiving catalog.
///
/// Every field is downstream of an `Accept` from
/// [`verify_asset`](capsule_core::crypto::verify_asset::verify_asset) plus — when the action carries
/// metadata — a bound, correctly signed sidecar, so a client may persist all of it without
/// re-checking anything.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteAssetFacts {
    /// The asset id (the manifest's `file_id`).
    pub asset_id: Uuid,
    /// The album the asset belongs to.
    pub album_id: Uuid,
    /// The AMK epoch the asset is sealed under.
    pub amk_version: u32,
    /// The original's ciphertext content address.
    pub ciphertext_hash: Hash32,
    /// The original's plaintext length in bytes.
    pub plaintext_size: u64,
    /// The head manifest's lifecycle action (`create`, `metadata-update`, `delete`, …).
    pub action: Action,
    /// RFC3339 authoring timestamp of the head manifest.
    pub timestamp: String,
    /// The device that authored it, resolved in the signed device directory.
    pub created_by_device: Uuid,
    /// The provenance-record hash this entry establishes — the value a client persists as the
    /// asset's new head and passes back as [`RemoteEntry::local_chain_head`] next time.
    pub provenance_head: Hash32,
    /// The decrypted, signature-checked sidecar: capture time, content type, dimensions, LQIP,
    /// and the CRDT registers. Carried whole rather than pre-projected, so a client upserts
    /// exactly the facts it indexes without this layer guessing at its schema.
    ///
    /// `None` exactly when the head action mints no metadata blob — `delete`,
    /// `trash-restore`, and the derivative actions, per
    /// [`Action::binds_metadata_blob`]. A tombstone is a legitimate, fully-verified entry that
    /// simply carries no metadata; treating its absent blob as a binding failure would
    /// quarantine every deletion the feed delivers.
    pub sidecar: Option<SidecarV1>,
}

/// How applying one remote entry resolved. These three are the verdicts the
/// [client validation duties] permit — there is no fourth, "ignored" case.
///
/// [client validation duties]: https://docs/design/clients/#client-validation-duties
#[derive(Debug, Clone, PartialEq)]
pub enum SyncApplyOutcome {
    /// Verified end to end; the facts may be upserted.
    Applied(Box<RemoteAssetFacts>),
    /// Authorization is intact but the epoch's AMK has not arrived locally yet. Hold the entry
    /// and retry — never quarantine (SSoT: [`PendingReason`]).
    Pending(PendingReason),
    /// Refused, with the reason to surface.
    Quarantined(QuarantineReason),
}

impl SyncApplyOutcome {
    /// The facts, if the entry applied.
    pub fn facts(&self) -> Option<&RemoteAssetFacts> {
        match self {
            Self::Applied(facts) => Some(facts),
            _ => None,
        }
    }
}

impl Workspace {
    /// Apply one remote feed entry: verify it through the
    /// [`verify_asset`](capsule_core::crypto::verify_asset::verify_asset) chokepoint, bind and
    /// open its metadata blob, and project the result into [`RemoteAssetFacts`].
    ///
    /// Read-only with respect to this workspace — it decides *what is true* and leaves *what to
    /// persist* to the caller's catalog. That split is deliberate: the same verdict feeds an
    /// iOS gallery upsert, a CLI `sync` report, and a quarantine surface, none of which want
    /// this layer writing to their index.
    ///
    /// `Err` is reserved for a failure of *this workspace* (an unreadable album keystore, say).
    /// A refused **entry** is never an `Err` — it is a [`SyncApplyOutcome::Quarantined`],
    /// because a hostile feed must not be able to abort a whole sync by handing over one bad
    /// row.
    pub fn apply_remote_entry(&self, entry: RemoteEntry<'_>) -> Result<SyncApplyOutcome> {
        self.apply_remote_head(entry.head())
    }

    /// [`apply_remote_entry`](Self::apply_remote_entry) for an entry whose original is named by
    /// its SHA-256 ([`RemoteHead`]): the same chokepoint, metadata binding and verdicts, with
    /// the content-integrity step run against the digest.
    ///
    /// An `Applied` verdict here attests the manifest and its metadata, and that the manifest
    /// commits to `original_ciphertext_hash`; it says nothing about any bytes until the caller
    /// has hashed them to that digest. [`open_remote_original_from`](Self::open_remote_original_from)
    /// is the caller that does.
    ///
    /// # Errors
    ///
    /// As [`apply_remote_entry`](Self::apply_remote_entry).
    #[tracing::instrument(skip_all, fields(album_id = %entry.album_id))]
    pub fn apply_remote_head(&self, entry: RemoteHead<'_>) -> Result<SyncApplyOutcome> {
        use QuarantineReason::{
            Binding, LegacyInheritedDevice, MalformedManifest, MalformedSidecar, Purged, Rejected,
            SidecarSignature, UnknownAlbum, UnsupportedRecordVersion,
        };

        // The feed serves the provenance blob's bytes, which are a *record* — see the module
        // docs. Decoding them as a bare manifest is why a correctly pushed asset used to be
        // quarantined by every receiving device.
        let record = match ProvenanceRecord::decode(entry.manifest_cbor) {
            Ok(record) => record,
            Err(RecordDecodeError::UnsupportedVersion { found, max_known }) => {
                tracing::warn!(
                    found,
                    max_known,
                    "sync-apply: provenance record from a newer writer; quarantined until update"
                );
                return Ok(SyncApplyOutcome::Quarantined(UnsupportedRecordVersion {
                    found,
                    max_known,
                }));
            }
            Err(RecordDecodeError::Malformed(e)) => {
                tracing::warn!(error = %e, "sync-apply: provenance record CBOR did not decode");
                return Ok(SyncApplyOutcome::Quarantined(MalformedManifest(e)));
            }
        };
        // The two copies of the chain link, checked before either is used. A record that
        // disagrees with the manifest it carries is malformed whichever copy is right, and the
        // manifest's is the signed one — so preferring either would be trusting a value no
        // signature covers.
        if !record.mirrors_manifest() {
            tracing::warn!(
                asset_id = %record.asset_id,
                "sync-apply: the record's prior_provenance_hash does not mirror the manifest's"
            );
            return Ok(SyncApplyOutcome::Quarantined(MalformedManifest(
                "the record's prior_provenance_hash does not mirror the manifest's".to_owned(),
            )));
        }
        // The record names its asset too; a record whose subject is not the manifest's `file_id`
        // is a splice of two assets' history and never applies.
        if record.asset_id != record.manifest.core.file_id {
            tracing::warn!(
                record_asset = %record.asset_id,
                manifest_asset = %record.manifest.core.file_id,
                "sync-apply: the record names a different asset than its manifest"
            );
            return Ok(SyncApplyOutcome::Quarantined(MalformedManifest(format!(
                "record names asset {} but its manifest names {}",
                record.asset_id, record.manifest.core.file_id
            ))));
        }
        // A purged asset's tombstone outranks anything a feed says about it: checked before the
        // album or the chokepoint, so no later entry, however well signed, revives it.
        if self.purged.contains_key(&record.asset_id) {
            tracing::warn!(
                asset_id = %record.asset_id,
                action = ?record.manifest.core.action,
                "sync-apply: the asset was purged here; its tombstone refuses the entry"
            );
            return Ok(SyncApplyOutcome::Quarantined(Purged(record.asset_id)));
        }
        let manifest: AssetManifest = record.manifest;
        let core = manifest.core.clone();
        tracing::debug!(
            asset_id = %core.file_id,
            action = ?core.action,
            amk_version = core.amk_version.0,
            "sync-apply: manifest decoded"
        );

        // No keys, no authority, no verdict. Say so, rather than rejecting an entry this device
        // simply cannot adjudicate.
        if !self.has_album(&entry.album_id) {
            tracing::info!("sync-apply: no local key material for the album");
            return Ok(SyncApplyOutcome::Quarantined(UnknownAlbum(entry.album_id)));
        }
        let album = self.album(&entry.album_id)?;
        let authority = self.authority(&entry.album_id)?;

        // ── The chokepoint. Not re-implemented, not partially applied. ─────────────────
        match verify_asset_hashed(
            &manifest,
            &entry.original_ciphertext_hash,
            &self.directory,
            authority,
            entry.local_chain_head,
        ) {
            VerifyOutcome::Accept => {}
            VerifyOutcome::Pending(reason) => {
                tracing::info!(?reason, "sync-apply: held pending — AMK not yet local");
                return Ok(SyncApplyOutcome::Pending(reason));
            }
            VerifyOutcome::TerminalReject(RejectReason::BadDeviceSig) => {
                // Tell the pre-#475 continuation apart from a forgery. Either way it is refused.
                if let Some(signing_device) = diagnose_misattributed_signer(
                    &manifest,
                    &entry.original_ciphertext_hash,
                    &self.directory,
                    authority,
                    entry.local_chain_head,
                ) {
                    let record_hash =
                        ProvenanceRecord::new(core.file_id, manifest, core.prior_provenance_hash)
                            .record_hash();
                    tracing::warn!(
                        asset_id = %core.file_id,
                        named_device = %core.created_by_device,
                        %signing_device,
                        %record_hash,
                        "sync-apply: quarantined a legacy inherited-device record; the chain stalls here"
                    );
                    return Ok(SyncApplyOutcome::Quarantined(LegacyInheritedDevice {
                        named_device: core.created_by_device,
                        signing_device,
                        record_hash,
                    }));
                }
                tracing::warn!(
                    reason = ?RejectReason::BadDeviceSig,
                    "sync-apply: quarantined by verify_asset"
                );
                return Ok(SyncApplyOutcome::Quarantined(Rejected(
                    RejectReason::BadDeviceSig,
                )));
            }
            VerifyOutcome::TerminalReject(reason) => {
                tracing::warn!(?reason, "sync-apply: quarantined by verify_asset");
                return Ok(SyncApplyOutcome::Quarantined(Rejected(reason)));
            }
        }

        // ── Metadata: bind the blob to the manifest, then open it. ─────────────────────
        //
        // Only for an action that mints one. `delete`, `trash-restore`, and the derivative
        // actions commit to no `metadata_blob_hash` by the presence-by-action rule — and
        // `verify_asset`'s structural check already refused any manifest that disagrees with
        // that rule, so this branch is a statement about the action, not a second guess at it.
        // A tombstone is a fully-verified entry with no metadata; quarantining it for the
        // absence would drop every deletion the feed delivers.
        let sidecar = if core.action.binds_metadata_blob() {
            let Some(committed) = core.metadata_blob_hash else {
                return Ok(SyncApplyOutcome::Quarantined(Binding(
                    BindingReject::NoManifestCommitment,
                )));
            };
            if blob_ciphertext_hash(entry.metadata_blob) != committed {
                tracing::warn!("sync-apply: metadata blob does not hash to the committed address");
                return Ok(SyncApplyOutcome::Quarantined(Binding(
                    BindingReject::BlobHashMismatch,
                )));
            }
            // `verify_asset` accepted, so the authority attests this epoch's AMK is held; a
            // missing entry here would be a keystore inconsistency — reported as pending,
            // never panicked.
            let Some(amk_bytes) = album.amks.get(&core.amk_version.0).copied() else {
                tracing::warn!(
                    epoch = core.amk_version.0,
                    "sync-apply: authority attests the AMK but the keystore holds no bytes for it"
                );
                return Ok(SyncApplyOutcome::Pending(PendingReason::AmkNotYetLocal));
            };
            let Some(nonce) = blob_nonce(entry.metadata_blob) else {
                return Ok(SyncApplyOutcome::Quarantined(Binding(
                    BindingReject::Undecryptable,
                )));
            };
            let blob_key = Amk::from_bytes(amk_bytes).derive_blob_key(&core.file_id, &nonce);
            let Ok(plaintext) = open_blob(&blob_key, entry.metadata_blob) else {
                tracing::warn!(
                    "sync-apply: metadata blob failed to open under the derived blob key"
                );
                return Ok(SyncApplyOutcome::Quarantined(Binding(
                    BindingReject::Undecryptable,
                )));
            };

            // `from_canonical_slice` is also where the **forward-version refusal** lives: a
            // sidecar whose `sidecar_schema` exceeds this build's max known is refused rather
            // than strip-and-re-signed, which is a client validation duty in its own right.
            let sidecar = match SidecarV1::from_canonical_slice(&plaintext, SIDECAR_SCHEMA_V1) {
                Ok(sidecar) => sidecar,
                Err(e) => {
                    tracing::warn!(error = %e, "sync-apply: sidecar did not decode");
                    return Ok(SyncApplyOutcome::Quarantined(MalformedSidecar(e)));
                }
            };
            // The sidecar carries its own hybrid signature under the account's user IK (the key
            // that also signs the device directory). Checking it here is what makes the
            // decrypted metadata *facts* rather than merely plaintext.
            if !sidecar.verify(&self.user_ik_public()) {
                tracing::warn!("sync-apply: sidecar signature does not verify under the user IK");
                return Ok(SyncApplyOutcome::Quarantined(SidecarSignature));
            }
            Some(sidecar)
        } else {
            tracing::debug!(
                action = ?core.action,
                "sync-apply: action mints no metadata blob; applying without a sidecar"
            );
            None
        };

        // The record hash this entry establishes — the caller's next `local_chain_head`.
        let provenance_head =
            ProvenanceRecord::new(core.file_id, manifest, core.prior_provenance_hash).record_hash();

        tracing::info!(
            asset_id = %core.file_id,
            action = ?core.action,
            "sync-apply: entry verified and applied"
        );
        Ok(SyncApplyOutcome::Applied(Box::new(RemoteAssetFacts {
            asset_id: core.file_id,
            album_id: core.album_id,
            amk_version: core.amk_version.0,
            ciphertext_hash: core.ciphertext_hash,
            plaintext_size: core.plaintext_size,
            action: core.action,
            timestamp: core.timestamp,
            created_by_device: core.created_by_device,
            provenance_head,
            sidecar,
        })))
    }

    /// Verify a remote entry and **open its original**: run it through
    /// [`apply_remote_entry`](Self::apply_remote_entry), recover the original's file key from
    /// this workspace's album keys (honouring the manifest's `key_mode`), decrypt the STREAM
    /// ciphertext, and check the plaintext digest against the signed sidecar's `hash`.
    ///
    /// The read half of a restore: what comes back as [`RemoteOriginal::Opened`] is byte for
    /// byte the original the account's device signed, because every link from the bytes to a
    /// signature has been checked — the ciphertext against the manifest's `ciphertext_hash`
    /// (inside the chokepoint), every chunk against its AEAD tag, and the plaintext against the
    /// sidecar's digest, which the account's identity key signs. A head that binds no metadata
    /// blob (a `trash-restore`, say) has no sidecar, so its plaintext stands on the first two.
    ///
    /// An entry that does not apply is handed back unchanged as [`RemoteOriginal::Refused`] —
    /// nothing is decrypted. Read-only with respect to this workspace, like the apply it wraps.
    ///
    /// # Errors
    ///
    /// As [`apply_remote_entry`](Self::apply_remote_entry): only a failure of this workspace
    /// itself. A file key that cannot be recovered or bytes that do not open are verdicts
    /// ([`RemoteOriginal::Undecryptable`], [`RemoteOriginal::DigestMismatch`]), never errors.
    pub fn open_remote_original(&self, entry: RemoteEntry<'_>) -> Result<RemoteOriginal> {
        self.open_remote_original_from(entry.head(), entry.original_ciphertext, Vec::new())
    }

    /// [`open_remote_original`](Self::open_remote_original) in bounded memory: the original's
    /// ciphertext is read from `ciphertext` and its plaintext written to `plaintext` one STREAM
    /// chunk at a time, so opening an original of any size holds a few chunks of it.
    ///
    /// The checks are [`open_remote_original`](Self::open_remote_original)'s, in an order a
    /// stream allows. The head goes through the chokepoint first
    /// ([`apply_remote_head`](Self::apply_remote_head)), against its claimed
    /// `original_ciphertext_hash`; then every ciphertext byte read is hashed while it is
    /// decrypted, and the original opens only if that hash is the manifest's `ciphertext_hash`,
    /// every chunk authenticated, and the plaintext hashes to the signed sidecar's digest.
    /// Bytes that are not the committed ciphertext are refused as the chokepoint refuses them
    /// ([`RejectReason::CiphertextHashMismatch`]), whichever check they fail first.
    ///
    /// Plaintext reaches `plaintext` before those checks finish, so the writer holds
    /// **unverified** bytes until this returns [`RemoteOriginal::Opened`], which hands the writer
    /// back. On any other outcome the caller must discard what it wrote. An entry that does not
    /// apply is refused before anything is read or written.
    ///
    /// # Errors
    ///
    /// As [`open_remote_original`](Self::open_remote_original), plus [`LifecycleError::Io`] when
    /// `ciphertext` cannot be read or `plaintext` cannot be written.
    #[tracing::instrument(skip_all, fields(album_id = %entry.album_id))]
    pub fn open_remote_original_from<R: Read, W: Write>(
        &self,
        entry: RemoteHead<'_>,
        mut ciphertext: R,
        plaintext: W,
    ) -> Result<RemoteOriginal<W>> {
        let facts = match self.apply_remote_head(entry)? {
            SyncApplyOutcome::Applied(facts) => facts,
            refused => return Ok(RemoteOriginal::Refused(refused)),
        };
        // The apply decoded these exact bytes a moment ago, so this cannot fail; answered as a
        // verdict rather than unwrapped all the same.
        let Ok(record) = ProvenanceRecord::decode(entry.manifest_cbor) else {
            return Ok(RemoteOriginal::Undecryptable { facts });
        };
        let core = &record.manifest.core;
        let album = self.album(&facts.album_id)?;
        let file_key = match self.original_file_key(album, core) {
            Ok(key) => key,
            Err(error) => {
                tracing::warn!(asset_id = %facts.asset_id, %error, "open-remote: the file key could not be recovered");
                return Ok(RemoteOriginal::Undecryptable { facts });
            }
        };
        // One pass over the ciphertext: every byte read is hashed, and fed to the decryptor
        // until a chunk fails authentication. The rest is still read and hashed, so bytes that
        // are not the committed ciphertext earn the chokepoint's verdict, not a decrypt failure.
        let mut ciphertext_hash = Sha256Hasher::new();
        let mut decryptor = Some(StreamDecryptor::new(
            &file_key,
            &core.nonce_prefix,
            DigestWriter::new(plaintext),
        ));
        let mut window = vec![0u8; CIPHERTEXT_CHUNK];
        let mut read: u64 = 0;
        loop {
            let n = match ciphertext.read(&mut window) {
                Ok(0) => break,
                Ok(n) => n,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    return Err(LifecycleError::Io(format!(
                        "reading the original's ciphertext: {error}"
                    )));
                }
            };
            ciphertext_hash.update(&window[..n]);
            read += n as u64;
            if let Some(open) = decryptor.as_mut() {
                match open.update(&window[..n]) {
                    Ok(()) => {}
                    Err(StreamError::Auth(chunk)) => {
                        tracing::warn!(asset_id = %facts.asset_id, chunk, "open-remote: a chunk of the original failed authentication");
                        decryptor = None;
                    }
                    Err(StreamError::Io(error)) => {
                        return Err(LifecycleError::Io(format!(
                            "writing the original's plaintext: {error}"
                        )));
                    }
                }
            }
        }
        if ciphertext_hash.finalize() != core.ciphertext_hash {
            tracing::warn!(
                asset_id = %facts.asset_id,
                bytes = read,
                "open-remote: the streamed ciphertext does not hash to the manifest's content address"
            );
            return Ok(RemoteOriginal::Refused(SyncApplyOutcome::Quarantined(
                QuarantineReason::Rejected(RejectReason::CiphertextHashMismatch),
            )));
        }
        let Some(decryptor) = decryptor else {
            return Ok(RemoteOriginal::Undecryptable { facts });
        };
        let opened = match decryptor.finish() {
            Ok(opened) => opened,
            Err(StreamError::Auth(chunk)) => {
                tracing::warn!(asset_id = %facts.asset_id, chunk, "open-remote: the original's last chunk failed authentication");
                return Ok(RemoteOriginal::Undecryptable { facts });
            }
            Err(StreamError::Io(error)) => {
                return Err(LifecycleError::Io(format!(
                    "writing the original's plaintext: {error}"
                )));
            }
        };
        let (plaintext, digest, written) = opened.into_parts();
        if let Some(sidecar) = &facts.sidecar
            && digest != sidecar.hash
        {
            tracing::warn!(
                asset_id = %facts.asset_id,
                "open-remote: the plaintext digest differs from the signed sidecar's"
            );
            return Ok(RemoteOriginal::DigestMismatch { facts });
        }
        tracing::debug!(
            asset_id = %facts.asset_id,
            ciphertext_bytes = read,
            plaintext_bytes = written,
            "open-remote: original verified and decrypted"
        );
        Ok(RemoteOriginal::Opened { facts, plaintext })
    }
}

/// A writer that hashes and counts what passes through it to `inner`.
struct DigestWriter<W> {
    inner: W,
    hasher: Sha256Hasher,
    written: u64,
}

impl<W> DigestWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256Hasher::new(),
            written: 0,
        }
    }

    /// The inner writer, the SHA-256 of everything written, and its length.
    fn into_parts(self) -> (W, Hash32, u64) {
        (self.inner, self.hasher.finalize(), self.written)
    }
}

impl<W: Write> Write for DigestWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// How [`Workspace::open_remote_original`] (`P` = `Vec<u8>`) or
/// [`Workspace::open_remote_original_from`] (`P` = the caller's writer) resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteOriginal<P = Vec<u8>> {
    /// Verified end to end and decrypted; when the head carries a sidecar, the plaintext hashes
    /// to its signed digest.
    Opened {
        /// The verified facts of the entry.
        facts: Box<RemoteAssetFacts>,
        /// The original's bytes, or the writer they were streamed into.
        plaintext: P,
    },
    /// The entry did not apply (held pending or quarantined), or the ciphertext streamed to
    /// [`Workspace::open_remote_original_from`] is not the one its head named
    /// ([`RejectReason::CiphertextHashMismatch`]). Nothing decrypted is to be kept.
    Refused(SyncApplyOutcome),
    /// The entry applied, but its file key could not be recovered or its ciphertext failed
    /// authentication under it.
    Undecryptable {
        /// The verified facts of the entry.
        facts: Box<RemoteAssetFacts>,
    },
    /// The ciphertext opened, but the plaintext does not hash to the signed sidecar's digest.
    DigestMismatch {
        /// The verified facts of the entry.
        facts: Box<RemoteAssetFacts>,
    },
}

#[cfg(test)]
mod tests;
