//! The **tombstone-plus-fork album upgrade ceremony** (slice S-X3).
//!
//! SSoT: [Versioning — Album Upgrade Ceremony](https://docs/design/versioning/#album-upgrade-ceremony).
//!
//! A version-pinned album's `protocol_version` is immutable for its lifetime, so adopting a new
//! protocol feature — or migrating to a new `crypto_suite_id` (e.g. a future move off the `0x004D`
//! X-Wing suite) — requires **freezing** the old album and **forking** a fresh one at the target
//! version. The ceremony is atomic at the user level (the single `AlbumTombstone` commit is the
//! cutover) and resumable (`intent_id`-keyed). This module owns the MLS-layer half: the signed
//! `UpgradeIntent`, quiescence, the `frozen_state_hash`, the `AlbumTombstone` commit + its
//! receive-side verification, and the fork's `upgraded_from` continuity pointer.
//!
//! **Suite-parametric.** The intent and lineage carry `from_suite_id` / `to_suite_id` as `u16`
//! wire codepoints, so the ceremony is the general migration vehicle even though only `0x0001`
//! (whose MLS suite is `0x004D`) exists in this build. Reads of the old album are never gated —
//! only *writes* refuse after a tombstone (`verify_asset` stays byte-for-byte untouched).
//!
//! **Server-side out of scope.** The server-clock deadline evaluation, the `409 Conflict` on stale
//! upload sessions, and the drain of in-flight sessions are server concerns (S-X3 makes **no
//! server changes**). [`UpgradeIntent::is_expired`] is provided as the pure clock predicate the
//! server would evaluate against its trusted clock; here it is exercised in isolation.

use jiff::Timestamp;
use openmls::group::CommitMessageBundle;
use openmls::prelude::{LeafNodeParameters, MlsMessageIn, MlsMessageOut, ProcessedMessageContent};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::identity::{LeafBinding, LeafBindingCore};
use super::messages::MlsAppPayload;
use super::{
    CommitAad, OpenMlsAuthority, OpenMlsAuthorityError, Result, WriteTierIngest, describe_content,
    parse_commit_aad, protocol_message,
};
use crate::crypto::hash::{self, Hash32};
use crate::crypto::keys::{DeviceDirectory, HybridSignature, HybridSigningKey};

/// The default upgrade deadline (7 days), as a [`jiff::SignedDuration`] the caller can pass to
/// [`OpenMlsAuthority::propose_upgrade`].
pub const DEFAULT_UPGRADE_DEADLINE: jiff::SignedDuration = jiff::SignedDuration::from_hours(24 * 7);

/// The signed-over content of an upgrade proposal (versioning.md step 1). Every field is covered by
/// the proposer's DSK hybrid signature in the enclosing [`SignedUpgradeIntent`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpgradeIntent {
    /// UUIDv7 idempotency key for the whole ceremony — a duplicate/contradictory proposal under a
    /// *different* id is rejected while one is in flight; a *replayed* one is a no-op.
    pub intent_id: Uuid,
    /// The album's current (immutable) `protocol_version`.
    pub from_protocol_version: String,
    /// The target `protocol_version` the fork is pinned to.
    pub to_protocol_version: String,
    /// The album's current `crypto_suite_id` wire codepoint.
    pub from_suite_id: u16,
    /// The target `crypto_suite_id` wire codepoint (may equal `from_suite_id` for a protocol-only
    /// upgrade; differs for a suite migration).
    pub to_suite_id: u16,
    /// The account the proposing admin device belongs to.
    pub proposer_user: Uuid,
    /// The proposing admin device (its DSK signs this intent; verified against the device directory).
    pub proposer_device: Uuid,
    /// The deadline **duration** in whole seconds (default [`DEFAULT_UPGRADE_DEADLINE`]). The
    /// effective expiry is `received_at + deadline` on the **server's** trusted clock — see
    /// [`is_expired`](Self::is_expired); a member clock can neither extend nor shorten it.
    pub deadline_secs: u64,
}

impl UpgradeIntent {
    /// The canonical-CBOR signing bytes the proposer's DSK covers.
    pub(super) fn signing_bytes(&self) -> Result<Vec<u8>> {
        crate::cbor::to_canonical_vec(self)
            .map_err(|e| OpenMlsAuthorityError::Upgrade(format!("intent encode: {e}")))
    }

    /// Whether this intent has expired: `now >= received_at + deadline` on the caller's (server's)
    /// trusted clock. Overflow is treated as expired (fail-closed). This is the **only** clock
    /// evaluation; it is a server concern here and is exercised in isolation.
    pub fn is_expired(&self, received_at: Timestamp, now: Timestamp) -> bool {
        let secs = i64::try_from(self.deadline_secs).unwrap_or(i64::MAX);
        match received_at.checked_add(jiff::SignedDuration::from_secs(secs)) {
            Ok(expiry) => now >= expiry,
            Err(_) => true,
        }
    }
}

/// An [`UpgradeIntent`] plus the proposing admin device's DSK **hybrid** signature over it. Rides
/// the group's application-message channel (self-describing as [`MlsAppPayload::Upgrade`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedUpgradeIntent {
    /// The proposed upgrade.
    pub intent: UpgradeIntent,
    /// The proposer DSK's hybrid signature over [`UpgradeIntent::signing_bytes`].
    pub proposer_sig: HybridSignature,
}

impl SignedUpgradeIntent {
    /// Verify the proposer's DSK hybrid signature (Ed25519 **and** ML-DSA) against the device
    /// directory — the same trust resolution `verify_leaf_binding` uses. A stale, forged, or
    /// wrong-device signature is rejected before any quiescence state is entered.
    pub fn verify(&self, directory: &DeviceDirectory) -> Result<()> {
        if directory.core.user_id != self.intent.proposer_user {
            return Err(OpenMlsAuthorityError::Upgrade(
                "intent proposer_user does not match the device directory".into(),
            ));
        }
        let entry = directory
            .device(&self.intent.proposer_device)
            .ok_or_else(|| {
                OpenMlsAuthorityError::Upgrade(
                    "proposer device is not in the device directory".into(),
                )
            })?;
        if !entry
            .dsk_public
            .verify(&self.intent.signing_bytes()?, &self.proposer_sig)
        {
            return Err(OpenMlsAuthorityError::Upgrade(
                "proposer DSK hybrid signature over the upgrade intent did not verify".into(),
            ));
        }
        Ok(())
    }
}

/// The `upgraded_from` continuity pointer the fork's manifests carry — the normative link between
/// the old album and its fork (never the MLS group name, which is an internal detail).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpgradeLineage {
    /// The album this fork was upgraded from.
    pub old_album_id: Uuid,
    /// The ceremony's idempotency key.
    pub intent_id: Uuid,
    /// The frozen old-album state hash the tombstone committed to.
    pub frozen_state_hash: Hash32,
    /// The old album's `crypto_suite_id`.
    pub from_suite_id: u16,
    /// The fork's `crypto_suite_id`.
    pub to_suite_id: u16,
}

/// The caller-supplied facts about an album's full state that the `frozen_state_hash` is computed
/// over, **alongside** the MLS-derived sorted member list. Owned by the application/library layer
/// (this authority has no view of manifests or the provenance log), so the caller assembles it from
/// its own accepted state; a member whose view diverges yields a different hash and aborts the
/// tombstone (each member independently).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumStateSummary {
    /// Every accepted manifest's content hash (order-independent — hashed sorted).
    pub manifest_hashes: Vec<Hash32>,
    /// The head of the album's provenance log, if any.
    pub provenance_head: Option<Hash32>,
}

/// The freeze marker an `AlbumTombstone` commit carries in its authenticated data (inside
/// [`CommitAad::Tombstone`]). Every receiving member recomputes `frozen_state_hash` over its own
/// state and compares — mismatch aborts the upgrade.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct TombstoneMark {
    /// The ceremony's idempotency key.
    pub intent_id: Uuid,
    /// The proposer's `frozen_state_hash` over the album's full state.
    pub frozen_state_hash: Hash32,
}

/// The upgrade-quiescence state an album enters on issuing/receiving an [`UpgradeIntent`]. Persisted
/// (so a crash mid-ceremony resumes) and consulted to reject a second, conflicting intent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Quiescence {
    /// The in-flight ceremony's idempotency key.
    pub intent_id: Uuid,
    /// The `protocol_version` the fork will be pinned to (drives queued-write re-encoding).
    pub to_protocol_version: String,
    /// The fork's `crypto_suite_id`.
    pub to_suite_id: u16,
    /// The proposing admin device.
    pub proposer_device: Uuid,
}

/// The artifacts a [`propose_upgrade`](OpenMlsAuthority::propose_upgrade) call produces.
pub struct UpgradeProposal {
    /// The signed intent (kept by the proposer; also carried in `message`).
    pub signed_intent: SignedUpgradeIntent,
    /// The MLS application message broadcasting the intent to the other members.
    pub message: MlsMessageOut,
}

/// The artifacts a [`commit_tombstone`](OpenMlsAuthority::commit_tombstone) call produces.
pub struct TombstoneOutcome {
    /// The ceremony's idempotency key.
    pub intent_id: Uuid,
    /// The `frozen_state_hash` the tombstone committed to.
    pub frozen_state_hash: Hash32,
    /// The `AlbumTombstone` commit, for delivery to the other members (they verify + freeze).
    pub commit: MlsMessageOut,
}

/// The canonical-CBOR pre-image of an album's `frozen_state_hash`: the album id, the sorted member
/// list, and the caller-supplied [`AlbumStateSummary`] (with its manifest hashes sorted).
#[derive(Serialize)]
struct FrozenState {
    album_id: Uuid,
    members: Vec<LeafBindingCore>,
    manifest_hashes: Vec<Hash32>,
    provenance_head: Option<Hash32>,
}

impl OpenMlsAuthority {
    // ── Step 1: propose (admin) / receive (member) ───────────────────────────

    /// **Step 1 (admin).** Issue a hybrid-signed [`UpgradeIntent`] and enter upgrade quiescence.
    /// The `intent_id` is minted here (UUIDv7). Returns the signed intent + the MLS application
    /// message to broadcast to the other members. Refused if the album is already tombstoned or
    /// already quiescing under another intent.
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id, %to_suite_id))]
    pub fn propose_upgrade(
        &mut self,
        from_protocol_version: impl Into<String>,
        from_suite_id: u16,
        to_protocol_version: impl Into<String>,
        to_suite_id: u16,
        deadline: jiff::SignedDuration,
    ) -> Result<UpgradeProposal> {
        self.ensure_not_tombstoned()?;
        if let Some(q) = &self.quiescence {
            return Err(OpenMlsAuthorityError::Upgrade(format!(
                "album is already quiescing under upgrade intent {}",
                q.intent_id
            )));
        }
        let deadline_secs = u64::try_from(deadline.as_secs().max(0)).unwrap_or(0);
        let intent = UpgradeIntent {
            intent_id: Uuid::now_v7(),
            from_protocol_version: from_protocol_version.into(),
            to_protocol_version: to_protocol_version.into(),
            from_suite_id,
            to_suite_id,
            proposer_user: self.identity.user_id,
            proposer_device: self.identity.device_id,
            deadline_secs,
        };
        let proposer_sig = self.identity.dsk.sign(&intent.signing_bytes()?);
        let signed = SignedUpgradeIntent {
            intent: intent.clone(),
            proposer_sig,
        };
        let message = self.create_app_message(&MlsAppPayload::Upgrade(signed.clone()))?;
        self.enter_quiescence(&intent);
        tracing::info!(album_id = %self.album_id, intent_id = %intent.intent_id, "album upgrade proposed; entering quiescence");
        Ok(UpgradeProposal {
            signed_intent: signed,
            message,
        })
    }

    /// **Step 1/2 (member).** Process a broadcast [`UpgradeIntent`]: verify the proposer's DSK
    /// hybrid signature against `directory`, then enter quiescence. Rejects a *different* intent
    /// while one is already in flight (only one upgrade per album); a re-receipt of the same intent
    /// is idempotent. Returns the verified signed intent.
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id))]
    pub fn receive_upgrade_intent(
        &mut self,
        message: MlsMessageIn,
        directory: &DeviceDirectory,
    ) -> Result<SignedUpgradeIntent> {
        let protocol = protocol_message(message)?;
        let processed = self
            .group
            .process_message(&self.identity.provider, protocol)
            .map_err(|e| OpenMlsAuthorityError::ProcessMessage(format!("{e:?}")))?;
        let signed = match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app) => {
                match MlsAppPayload::from_bytes(&app.into_bytes())? {
                    MlsAppPayload::Upgrade(signed) => signed,
                    _ => {
                        return Err(OpenMlsAuthorityError::UnexpectedMessage(
                            "expected an upgrade intent application message".into(),
                        ));
                    }
                }
            }
            other => {
                return Err(OpenMlsAuthorityError::UnexpectedMessage(format!(
                    "expected an application message, got {}",
                    describe_content(&other)
                )));
            }
        };
        signed.verify(directory)?;
        if let Some(q) = &self.quiescence {
            if q.intent_id != signed.intent.intent_id {
                return Err(OpenMlsAuthorityError::Upgrade(format!(
                    "album is already quiescing under a different upgrade intent {}",
                    q.intent_id
                )));
            }
            return Ok(signed); // idempotent re-receipt
        }
        self.enter_quiescence(&signed.intent);
        tracing::info!(album_id = %self.album_id, intent_id = %signed.intent.intent_id, "upgrade intent accepted; entering quiescence");
        Ok(signed)
    }

    // ── Step 2/3: quiescence write queue ─────────────────────────────────────

    /// Queue a write locally during upgrade quiescence (`pending_until_upgrade`). The caller owns
    /// the opaque encoding; the writes are handed back by [`take_pending_writes`](Self::take_pending_writes)
    /// after cutover for re-encoding against the fork's `to_version`. Refused if not quiescing.
    pub fn queue_pending_write(&mut self, encoded_write: Vec<u8>) -> Result<()> {
        if self.quiescence.is_none() {
            return Err(OpenMlsAuthorityError::Upgrade(
                "album is not in upgrade quiescence; write directly".into(),
            ));
        }
        self.pending_writes.push(encoded_write);
        Ok(())
    }

    /// Drain the locally-queued `pending_until_upgrade` writes (for replay into the fork). Draining
    /// is the caller's cue to re-encode each against the fork's `to_version` and replay it.
    pub fn take_pending_writes(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.pending_writes)
    }

    // ── Step 4: tombstone (freeze) ───────────────────────────────────────────

    /// The album's `frozen_state_hash` over its full state: the album id, the sorted member list
    /// (from live MLS group state), and the caller's [`AlbumStateSummary`]. Suite-fixed SHA-256.
    pub fn frozen_state_hash(&self, summary: &AlbumStateSummary) -> Result<Hash32> {
        let members = self.sorted_member_bindings()?;
        let mut manifest_hashes = summary.manifest_hashes.clone();
        manifest_hashes.sort();
        let frozen = FrozenState {
            album_id: self.album_id,
            members,
            manifest_hashes,
            provenance_head: summary.provenance_head,
        };
        let bytes = crate::cbor::to_canonical_vec(&frozen)
            .map_err(|e| OpenMlsAuthorityError::Upgrade(format!("frozen-state encode: {e}")))?;
        Ok(hash::hash_bytes(&bytes))
    }

    /// **Step 4 (admin).** Freeze the album with an `AlbumTombstone` commit: compute the
    /// `frozen_state_hash` over `summary`, attach it (with the terminal epoch's write-tier
    /// attestation) to the commit's authenticated data, advance the group one terminal epoch, and
    /// mark the album tombstoned. Requires an in-flight quiescence ([`propose_upgrade`](Self::propose_upgrade)).
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id))]
    pub fn commit_tombstone(&mut self, summary: &AlbumStateSummary) -> Result<TombstoneOutcome> {
        self.ensure_not_tombstoned()?;
        let quiescence = self.quiescence.clone().ok_or_else(|| {
            OpenMlsAuthorityError::Upgrade(
                "no upgrade in quiescence; call propose_upgrade first".into(),
            )
        })?;
        let frozen_state_hash = self.frozen_state_hash(summary)?;
        let minted = HybridSigningKey::generate();
        let write_tier = self.next_write_tier_attestation(&minted)?;
        let mark = TombstoneMark {
            intent_id: quiescence.intent_id,
            frozen_state_hash,
        };
        self.set_commit_aad(&CommitAad::Tombstone {
            write_tier,
            mark: mark.clone(),
        })?;
        let commit = self
            .group
            .self_update(
                &self.identity.provider,
                &self.identity.mls_signer,
                LeafNodeParameters::default(),
            )
            .map(CommitMessageBundle::into_commit)
            .map_err(|e| OpenMlsAuthorityError::Upgrade(format!("tombstone commit: {e:?}")))?;
        self.group
            .merge_pending_commit(&self.identity.provider)
            .map_err(|e| OpenMlsAuthorityError::Upgrade(format!("tombstone merge: {e:?}")))?;
        self.ingest_current_epoch(WriteTierIngest::Minted(minted), true)?;
        self.tombstoned = Some(quiescence.intent_id);
        self.completed_intents.insert(quiescence.intent_id);
        tracing::info!(album_id = %self.album_id, intent_id = %quiescence.intent_id, %frozen_state_hash, "album tombstoned (frozen for upgrade)");
        Ok(TombstoneOutcome {
            intent_id: quiescence.intent_id,
            frozen_state_hash,
            commit,
        })
    }

    /// **Step 4 (member).** Process a received `AlbumTombstone` commit: recompute `frozen_state_hash`
    /// over this member's own `summary` and compare it to the committed mark. On mismatch the
    /// upgrade **aborts cleanly** — the staged commit is dropped, the album stays at its prior epoch
    /// and normal operation ([`FrozenStateMismatch`](OpenMlsAuthorityError::FrozenStateMismatch)).
    /// On match, the freeze is adopted (album tombstoned). Idempotent for a re-delivered tombstone
    /// of an already-frozen album.
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id))]
    pub fn process_tombstone(
        &mut self,
        message: MlsMessageIn,
        summary: &AlbumStateSummary,
    ) -> Result<()> {
        let protocol = protocol_message(message)?;
        let processed = self
            .group
            .process_message(&self.identity.provider, protocol)
            .map_err(|e| OpenMlsAuthorityError::ProcessMessage(format!("{e:?}")))?;
        let aad = processed.aad().to_vec();
        match processed.into_content() {
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                let (write_tier, mark) = match parse_commit_aad(&aad)? {
                    CommitAad::Tombstone { write_tier, mark } => (write_tier, mark),
                    CommitAad::WriteTier(_) => {
                        return Err(OpenMlsAuthorityError::Upgrade(
                            "expected an AlbumTombstone commit, got an ordinary commit".into(),
                        ));
                    }
                };
                // Verify BEFORE merging: a self-update tombstone leaves the member list unchanged,
                // so the local hash is stable across the merge. On mismatch we drop the staged
                // commit (never merge) — the album stays at its prior epoch.
                let local_hash = self.frozen_state_hash(summary)?;
                if local_hash != mark.frozen_state_hash {
                    tracing::warn!(album_id = %self.album_id, intent_id = %mark.intent_id, "tombstone frozen-state hash mismatch; aborting upgrade");
                    return Err(OpenMlsAuthorityError::FrozenStateMismatch);
                }
                self.group
                    .merge_staged_commit(&self.identity.provider, *staged)
                    .map_err(|e| {
                        OpenMlsAuthorityError::Upgrade(format!("tombstone merge: {e:?}"))
                    })?;
                let version = self.ingest_current_epoch(
                    WriteTierIngest::Attested(write_tier.write_tier_pub),
                    false,
                )?;
                if write_tier.amk_version != version.0 {
                    return Err(OpenMlsAuthorityError::Upgrade(format!(
                        "tombstone attests epoch {}, but advances the group to {}",
                        write_tier.amk_version, version.0
                    )));
                }
                self.tombstoned = Some(mark.intent_id);
                self.completed_intents.insert(mark.intent_id);
                tracing::info!(album_id = %self.album_id, intent_id = %mark.intent_id, "tombstone accepted; album frozen for upgrade");
                Ok(())
            }
            other => Err(OpenMlsAuthorityError::UnexpectedMessage(format!(
                "expected a commit, got {}",
                describe_content(&other)
            ))),
        }
    }

    // ── Step 5: fork ─────────────────────────────────────────────────────────

    /// **Step 5.** Found the fork: a fresh MLS album group at `new_album_id` and the target
    /// version, minting `AMK_v1` + a fresh write-tier key (via [`create_album`](Self::create_album)),
    /// and recording the `upgraded_from` continuity pointer. Members are then migrated with the
    /// standard [`add_member`](Self::add_member) / [`join_via_welcome`](Self::join_via_welcome) flow.
    /// Assets are **not** re-encrypted — the fork references the existing ciphertext by content hash.
    #[tracing::instrument(skip_all, fields(%new_album_id, old_album_id = %lineage.old_album_id, to_suite = lineage.to_suite_id))]
    pub fn fork_upgrade(
        admin_identity: super::MlsDeviceIdentity,
        new_album_id: Uuid,
        lineage: UpgradeLineage,
        history_policy: super::HistoryPolicy,
    ) -> Result<OpenMlsAuthority> {
        let mut forked =
            OpenMlsAuthority::create_album(admin_identity, new_album_id, history_policy)?;
        tracing::info!(%new_album_id, intent_id = %lineage.intent_id, "album forked for upgrade");
        forked.upgraded_from = Some(lineage);
        Ok(forked)
    }

    // ── Accessors ────────────────────────────────────────────────────────────

    /// The upgrade `intent_id` this album was tombstoned under, or `None` if it is live.
    pub fn is_tombstoned(&self) -> Option<Uuid> {
        self.tombstoned
    }

    /// The `upgraded_from` continuity pointer, if this album is a fork produced by an upgrade.
    pub fn upgraded_from(&self) -> Option<&UpgradeLineage> {
        self.upgraded_from.as_ref()
    }

    /// The `intent_id` of an in-flight upgrade quiescence, or `None`.
    pub fn quiescing_intent(&self) -> Option<Uuid> {
        self.quiescence.as_ref().map(|q| q.intent_id)
    }

    /// Whether a ceremony `intent_id` has already completed on this authority (idempotency ledger).
    pub fn has_completed_intent(&self, intent_id: Uuid) -> bool {
        self.completed_intents.contains(&intent_id)
    }

    // ── Internals ────────────────────────────────────────────────────────────

    /// The live group's members as sorted [`LeafBindingCore`]s (by `user_id` then `device_id`) —
    /// the deterministic member list the `frozen_state_hash` folds in.
    fn sorted_member_bindings(&self) -> Result<Vec<LeafBindingCore>> {
        let mut members: Vec<LeafBindingCore> = self
            .group
            .members()
            .map(|m| {
                LeafBinding::from_credential_bytes(m.credential.serialized_content())
                    .map(|b| b.core)
            })
            .collect::<Result<_>>()?;
        members.sort_by_key(|m| (m.user_id, m.device_id));
        Ok(members)
    }

    fn enter_quiescence(&mut self, intent: &UpgradeIntent) {
        self.quiescence = Some(Quiescence {
            intent_id: intent.intent_id,
            to_protocol_version: intent.to_protocol_version.clone(),
            to_suite_id: intent.to_suite_id,
            proposer_device: intent.proposer_device,
        });
    }
}
