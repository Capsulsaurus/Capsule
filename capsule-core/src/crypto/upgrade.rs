//! The album-upgrade ceremony's **wire vocabulary** — the parts both ends need and neither end
//! needs MLS for (slices `S-X3`, `S-C24`).
//!
//! SSoT: [Versioning — Album Upgrade Ceremony](https://docs/design/versioning/#album-upgrade-ceremony).
//!
//! # Why this is not in `crypto::authority`
//!
//! It was, and it was unreachable. `openmls_authority` is behind the `mls` feature, and a
//! **key-free server** does not enable it — it holds no group state and cannot read an MLS
//! message. But the server owns four of the ceremony's steps: the deadline is evaluated on its
//! trusted clock, the `409` on a stale upload session is its refusal, the drain count is its
//! answer, and the lineage travels in a manifest it stores. All four need these three types, and
//! none of them needs a group.
//!
//! So the types that are *pure data plus a signature check* live here, ungated, and
//! `openmls_authority` re-exports them. It is the same move `S-C46` made for the custody receipt,
//! for the same reason: a structure defined at both ends is one added field away from a signature
//! that stops verifying, so there is one definition and both ends read it.
//!
//! What stays behind the feature is everything that touches a group: the `AlbumTombstone` commit,
//! its receive-side verification, the quiescence state an MLS client persists, and the
//! `frozen_state_hash` pre-image, which is computed over the MLS-derived member list.
//!
//! # What the server can decide from here, and what it cannot
//!
//! It can verify [`SignedUpgradeIntent`] against the account's published
//! [`DeviceDirectory`] — the same trust anchor `S-C42`
//! established — so a quiescence it records is one an admin device really asked for. It can
//! evaluate [`UpgradeIntent::is_expired`] against its own clock, which is the whole point of the
//! deadline being a *duration*: a skewed member clock can neither extend nor shorten the window.
//!
//! It cannot verify the `frozen_state_hash`, and must never try. That hash is each member's
//! independent statement about its own view of the album, and a server that adjudicated it would
//! be the single point the ceremony's *"hostile member sabotage"* defence exists to avoid.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto::hash::Hash32;
use crate::crypto::keys::{DeviceDirectory, HybridSignature};

/// What went wrong reading or verifying an upgrade intent.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UpgradeError {
    /// The intent could not be canonically encoded for signing.
    #[error("the upgrade intent could not be encoded: {0}")]
    Encode(String),
    /// The signature did not verify, or the proposer is not a device of the named account.
    #[error("the upgrade intent's proposer signature did not verify: {0}")]
    Proposer(&'static str),
}

/// The default upgrade deadline (7 days), as a [`jiff::SignedDuration`] the caller can pass to
/// `OpenMlsAuthority::propose_upgrade`.
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
    ///
    /// # Errors
    ///
    /// Returns [`UpgradeError::Encode`] if the intent cannot be canonically encoded.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, UpgradeError> {
        crate::cbor::to_canonical_vec(self).map_err(|e| UpgradeError::Encode(e.to_string()))
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
/// the group's application-message channel (self-describing as the `MlsAppPayload::Upgrade` variant).
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
    /// # Errors
    ///
    /// Returns [`UpgradeError::Proposer`] when the directory names a different account, does not
    /// hold the proposing device, or the signature does not verify under its DSK.
    pub fn verify(&self, directory: &DeviceDirectory) -> Result<(), UpgradeError> {
        if directory.core.user_id != self.intent.proposer_user {
            return Err(UpgradeError::Proposer(
                "the intent's proposer_user is not this directory's account",
            ));
        }
        let entry =
            directory
                .device(&self.intent.proposer_device)
                .ok_or(UpgradeError::Proposer(
                    "the proposing device is not in the directory",
                ))?;
        if !entry
            .dsk_public
            .verify(&self.intent.signing_bytes()?, &self.proposer_sig)
        {
            return Err(UpgradeError::Proposer(
                "the proposer's DSK signature does not verify",
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
