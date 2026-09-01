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
//! 1. decode the manifest from the **opaque canonical CBOR** the feed carries verbatim (never
//!    re-encoded — re-encoding would detach it from its signatures);
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
//! [download & sync]: https://docs/design/import/download-sync/
//! [`verify_asset`]: crate::crypto::verify_asset::verify_asset
//! [`VerifyOutcome`]: crate::crypto::verify_asset::VerifyOutcome

use uuid::Uuid;

use super::{Result, Workspace};
use crate::cbor;
use crate::crypto::encryption::{blob_ciphertext_hash, blob_nonce, open_blob};
use crate::crypto::hash::Hash32;
use crate::crypto::keys::Amk;
use crate::crypto::provenance::action::Action;
use crate::crypto::provenance::{AssetManifest, ProvenanceRecord};
use crate::crypto::verify_asset::{
    BindingReject, PendingReason, RejectReason, VerifyOutcome, verify_asset,
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
    /// This workspace holds no key material or authority for the album the entry names, so
    /// there is nothing to verify it against. Not a rejection of the entry — a statement that
    /// this device cannot adjudicate it.
    UnknownAlbum(Uuid),
    /// [`verify_asset`] terminally rejected the manifest.
    Rejected(RejectReason),
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
            Binding, MalformedManifest, MalformedSidecar, Rejected, SidecarSignature, UnknownAlbum,
        };

        let manifest: AssetManifest = match cbor::from_slice(entry.manifest_cbor) {
            Ok(manifest) => manifest,
            Err(e) => {
                tracing::warn!(error = %e, "sync-apply: manifest CBOR did not decode");
                return Ok(SyncApplyOutcome::Quarantined(MalformedManifest(
                    e.to_string(),
                )));
            }
        };
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
        let provenance_head = ProvenanceRecord {
            asset_id: core.file_id,
            manifest,
            prior_provenance_hash: core.prior_provenance_hash,
        }
        .record_hash();

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
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::lifecycle::fast_workspace;

    /// The three byte strings a feed entry carries for one asset, exactly as `upload_bundle`
    /// puts them on the wire.
    struct Wire {
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
        let head = &ws
            .asset(&asset)
            .unwrap()
            .chain
            .records()
            .last()
            .unwrap()
            .manifest;
        let wire = Wire {
            manifest_cbor: cbor::to_canonical_vec(head).unwrap(),
            metadata_blob: bundle.metadata_blob.clone(),
            ciphertext: bundle.ciphertext.clone(),
        };
        (ws, album, asset, wire)
    }

    fn entry<'a>(album: Uuid, wire: &'a Wire, head: Option<Hash32>) -> RemoteEntry<'a> {
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
            manifest_cbor: cbor::to_canonical_vec(&records[1].manifest).unwrap(),
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
}
