//! Applying a **remote** asset entry — the receiving half of the lifecycle (slice `S-P1`).
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
//!    signatures), and check the record's `prior_provenance_hash` against the manifest's;
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
//! [`record_hash()`](crate::crypto::provenance::ProvenanceRecord::record_hash) — the digest of
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
//! [`verify_asset`]: crate::crypto::verify_asset::verify_asset
//! [`VerifyOutcome`]: crate::crypto::verify_asset::VerifyOutcome

use uuid::Uuid;

use super::{Result, Workspace};
use crate::crypto::encryption::{blob_ciphertext_hash, blob_nonce, open_blob};
use crate::crypto::hash::Hash32;
use crate::crypto::keys::Amk;
use crate::crypto::provenance::{Action, AssetManifest, ProvenanceRecord, RecordDecodeError};
use crate::crypto::verify_asset::{
    BindingReject, PendingReason, RejectReason, VerifyOutcome, diagnose_misattributed_signer,
    verify_asset,
};
use crate::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};

/// One remote feed entry, exactly as a sync feed delivers it: opaque bytes plus the album the
/// entry claims and the head the receiving catalog already holds. Borrowed rather than owned so
/// a caller hands over buffers it already has without a copy.
#[derive(Debug, Clone, Copy)]
pub struct RemoteEntry<'a> {
    /// The album the feed says this entry belongs to. Cross-checked against the manifest by
    /// [`verify_asset`] itself ([`RejectReason::WrongAlbum`]), never trusted on its own.
    pub album_id: Uuid,
    /// The signed `AssetManifest` as **opaque canonical CBOR**, carried verbatim.
    pub manifest_cbor: &'a [u8],
    /// The sealed metadata blob the manifest commits to via `metadata_blob_hash`.
    pub metadata_blob: &'a [u8],
    /// The asset's original ciphertext — required for the chokepoint's content-integrity step.
    pub original_ciphertext: &'a [u8],
    /// The provenance head **the receiving catalog** holds for this asset, or `None` if it has
    /// never seen it. Passed straight through to [`verify_asset`]'s parameter of the same name,
    /// which is what decides replay ([`RejectReason::Replayed`]) and fork
    /// ([`RejectReason::ForgedChain`]).
    ///
    /// It is a caller parameter rather than something read off the workspace because the
    /// catalog that tracks synced assets is the client's, not this workspace's asset map — an
    /// iOS gallery, a CLI report, and a peering reconciler each hold their own head. Feed back
    /// [`RemoteAssetFacts::provenance_head`] after applying.
    pub local_chain_head: Option<Hash32>,
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
    /// [`verify_asset`] terminally rejected the manifest.
    Rejected(RejectReason),
    /// [`verify_asset`] rejected the manifest as [`RejectReason::BadDeviceSig`], and the
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
}

/// The verified facts one remote entry contributes to the receiving catalog.
///
/// Every field is downstream of an `Accept` from [`verify_asset`] plus — when the action carries
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
    /// Apply one remote feed entry: verify it through the [`verify_asset`] chokepoint, bind and
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
    #[tracing::instrument(skip_all, fields(album_id = %entry.album_id))]
    pub fn apply_remote_entry(&self, entry: RemoteEntry<'_>) -> Result<SyncApplyOutcome> {
        use QuarantineReason::{
            Binding, LegacyInheritedDevice, MalformedManifest, MalformedSidecar, Rejected,
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
        match verify_asset(
            &manifest,
            entry.original_ciphertext,
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
                    entry.original_ciphertext,
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
    #[tracing::instrument(skip_all, fields(album_id = %entry.album_id))]
    pub fn open_remote_original(&self, entry: RemoteEntry<'_>) -> Result<RemoteOriginal> {
        let facts = match self.apply_remote_entry(entry)? {
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
        let plaintext = match crate::crypto::encryption::decrypt_asset_vec(
            &file_key,
            &core.nonce_prefix,
            entry.original_ciphertext,
        ) {
            Ok(plaintext) => plaintext,
            Err(error) => {
                tracing::warn!(asset_id = %facts.asset_id, %error, "open-remote: the original failed authentication");
                return Ok(RemoteOriginal::Undecryptable { facts });
            }
        };
        if let Some(sidecar) = &facts.sidecar
            && crate::crypto::hash::hash_bytes(&plaintext) != sidecar.hash
        {
            tracing::warn!(
                asset_id = %facts.asset_id,
                "open-remote: the plaintext digest differs from the signed sidecar's"
            );
            return Ok(RemoteOriginal::DigestMismatch { facts });
        }
        tracing::debug!(
            asset_id = %facts.asset_id,
            bytes = plaintext.len(),
            "open-remote: original verified and decrypted"
        );
        Ok(RemoteOriginal::Opened { facts, plaintext })
    }
}

/// How [`Workspace::open_remote_original`] resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteOriginal {
    /// Verified end to end and decrypted; when the head carries a sidecar, `plaintext` hashes
    /// to its signed digest.
    Opened {
        /// The verified facts of the entry.
        facts: Box<RemoteAssetFacts>,
        /// The original's bytes.
        plaintext: Vec<u8>,
    },
    /// The entry did not apply (held pending or quarantined); nothing was decrypted.
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
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::cbor;
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
            ciphertext: bundle.ciphertext.clone(),
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
            ciphertext: bundle.ciphertext.clone(),
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
            SyncApplyOutcome::Quarantined(QuarantineReason::Binding(
                BindingReject::BlobHashMismatch
            ))
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
            crate::crypto::hash::hash_bytes(&wire.manifest_cbor),
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
        use crate::crypto::keys::{DeviceEntry, HybridSigningKey};

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
        core: crate::crypto::provenance::ManifestCore,
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
            crate::crypto::keys::HybridSigningKey::from_seed_bytes(&[66; 32], &[67; 32])
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
                max_known: crate::crypto::provenance::PROVENANCE_RECORD_VERSION,
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
            crate::crypto::hash::hash_bytes(&future_wire.manifest_cbor),
            "the head covers the preserved field"
        );
    }
}
