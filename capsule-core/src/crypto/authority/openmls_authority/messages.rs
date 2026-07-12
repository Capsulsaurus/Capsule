//! The in-band key-delivery messages an album group exchanges over MLS application messages,
//! and the per-album [`HistoryPolicy`] that governs what a joiner is entitled to.
//!
//! SSoT: [Cryptography — MLS § History Delivery](https://docs/design/cryptography/mls/#history-delivery-for-new-joiners)
//! and [Keys — AMKs](https://docs/design/cryptography/keys/#album-master-keys-amks) (the
//! read/write capability split).
//!
//! Three payloads ride the group's encrypted application-message channel:
//!
//! - [`AlbumKeyDistribution`] `{ amk_version, amk_bytes }` — the doc-named steady-state broadcast:
//!   when a commit advances the epoch, the committer broadcasts the fresh epoch's AMK content key
//!   to **all members** (read capability), flipping their *pending* epoch (ceiling attested, key
//!   not yet local) to *present*.
//! - [`WriteTierDistribution`] `{ amk_version, write_tier_seed }` — the **write capability**: the
//!   private half of the per-epoch write-tier signing keypair the committer *minted* for the fresh
//!   epoch. Per the keys doc it is "distributed via MLS to **writers only**"; the public half rides
//!   the commit itself (authenticated AAD), so readers can verify manifests without ever holding
//!   the private half. See [`OpenMlsAuthority::writers`](super::OpenMlsAuthority::writers) for the
//!   recipient-set seam (today: all members).
//! - [`AlbumHistoryBundle`] — the join-time batch a committer sends a freshly-added member, one
//!   [`AmkHistoryEntry`] per prior epoch the album's [`HistoryPolicy`] entitles them to. Each entry
//!   carries that epoch's AMK **and** its write-tier public key, because a joiner cannot re-derive
//!   either for an epoch it never held (it joined after those epochs closed) — so without the
//!   bundle it could neither decrypt nor authorization-check a pre-join asset. Prior epochs are
//!   delivered **read-only** (no private write-tier halves — nobody signs new writes under an old
//!   epoch).
//!
//! All are wrapped in [`MlsAppPayload`] so one deprotected application message self-describes which
//! kind it is. The AMK content key itself is owned by
//! [Keys — AMKs](https://docs/design/cryptography/keys/#album-master-keys-amks); this module owns
//! only the *delivery envelope*.

use serde::{Deserialize, Serialize};

use super::{AMK_LEN, WRITE_TIER_SEED_LEN};
use crate::crypto::keys::HybridVerifyingKey;

/// How much history a new joiner may decrypt, fixed **per album** at creation and read from the
/// album's MLS metadata on every add — never chosen per-add, so a member's history visibility
/// never depends on which device added them or in what order (SSoT: MLS § History Delivery).
/// Changing it is an album upgrade ceremony (S-X3), never an ad-hoc per-add decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryPolicy {
    /// Full history (the default for shared albums): the joiner receives every prior AMK
    /// (`AMK_v1..AMK_current`) and can read every prior asset.
    Full,
    /// Capped history: the joiner receives only the most-recent `n` epochs' AMKs. Older assets
    /// remain visible but not decryptable (placeholders). `n = 0` collapses to "current epoch
    /// only"; a value ≥ the current epoch count is equivalent to [`Full`](Self::Full).
    Capped(u32),
}

impl HistoryPolicy {
    /// The inclusive `amk_version` range a joiner is entitled to when the album's current epoch
    /// (ceiling) is `ceiling`. Always includes the current epoch; the lower bound is `1` for
    /// [`Full`](Self::Full) and `ceiling - n + 1` (floored at `1`) for [`Capped(n)`](Self::Capped).
    ///
    /// This is a **pure function of `(policy, ceiling)`** — the load-bearing property that makes a
    /// joiner's history independent of which admin device performs the add.
    pub fn entitled_range(self, ceiling: u32) -> std::ops::RangeInclusive<u32> {
        debug_assert!(
            ceiling >= 1,
            "an album always has at least the genesis epoch"
        );
        let lo = match self {
            HistoryPolicy::Full => 1,
            HistoryPolicy::Capped(n) => ceiling.saturating_sub(n.saturating_sub(1)).max(1),
        };
        lo..=ceiling
    }
}

/// The doc-named steady-state AMK broadcast for **one** epoch: read capability delivered to all
/// members over an MLS application message. Its shape is fixed by MLS § History Delivery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumKeyDistribution {
    /// The epoch this key belongs to.
    pub amk_version: u32,
    /// The 32-byte AMK content key for that epoch.
    pub amk_bytes: [u8; AMK_LEN],
}

/// The per-epoch **write capability** delivery: the private half of the write-tier signing
/// keypair the committer minted for `amk_version`, sent over an MLS application message to the
/// epoch's writers (SSoT: [Keys — AMKs], "distributed via MLS to writers only"). The public half
/// is attested by the epoch's commit (authenticated AAD), so this message is the *only* way a
/// member obtains signing capability — it is never derivable from group secrets.
///
/// [Keys — AMKs]: https://docs/design/cryptography/keys/#album-master-keys-amks
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteTierDistribution {
    /// The epoch this write-tier key signs for.
    pub amk_version: u32,
    /// The 64-byte hybrid seed (Ed25519 secret ‖ ML-DSA ξ) of the epoch's write-tier signing key.
    #[serde(with = "serde_bytes")]
    pub write_tier_seed: Vec<u8>,
}

impl WriteTierDistribution {
    /// The seed as the fixed 64-byte array [`HybridSigningKey::from_seed64`] consumes, or a
    /// [`Message`](super::OpenMlsAuthorityError::Message) error on a malformed length.
    ///
    /// [`HybridSigningKey::from_seed64`]: crate::crypto::keys::HybridSigningKey::from_seed64
    pub(crate) fn seed64(&self) -> Result<[u8; WRITE_TIER_SEED_LEN], super::OpenMlsAuthorityError> {
        self.write_tier_seed.as_slice().try_into().map_err(|_| {
            super::OpenMlsAuthorityError::Message(format!(
                "write-tier seed for epoch {} is {} bytes, expected {WRITE_TIER_SEED_LEN}",
                self.amk_version,
                self.write_tier_seed.len()
            ))
        })
    }
}

/// One prior-epoch record in a join-time [`AlbumHistoryBundle`]. Carries both halves a joiner
/// needs but cannot re-derive for a pre-join epoch: the AMK (to decrypt) and the write-tier public
/// key (to authorization-check the epoch's manifests through `verify_asset`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AmkHistoryEntry {
    /// The prior epoch.
    pub amk_version: u32,
    /// The 32-byte AMK content key for that epoch.
    pub amk_bytes: [u8; AMK_LEN],
    /// The write-tier public key attested for that epoch (what a manifest's `write_sig` is
    /// verified against).
    pub write_tier_pub: HybridVerifyingKey,
}

/// The join-time history batch a committer sends a newly-added member: the prior epochs the
/// album's [`HistoryPolicy`] entitles them to, current epoch excluded (the joiner derives that
/// from the group state delivered in its Welcome).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumHistoryBundle {
    /// One entry per entitled prior epoch, ascending by `amk_version`.
    pub entries: Vec<AmkHistoryEntry>,
}

/// The self-describing wrapper for every Capsule application message on the group channel, so a
/// deprotected message names which key-delivery kind it carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum MlsAppPayload {
    /// A single-epoch steady-state AMK broadcast (read capability, all members).
    KeyDistribution(AlbumKeyDistribution),
    /// A single-epoch write-tier private-key delivery (write capability, writers).
    WriteTier(WriteTierDistribution),
    /// A join-time history batch (read-only prior epochs).
    History(AlbumHistoryBundle),
}

impl MlsAppPayload {
    /// Encode to canonical CBOR — the plaintext of an MLS application message.
    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>, super::OpenMlsAuthorityError> {
        crate::cbor::to_canonical_vec(self)
            .map_err(|e| super::OpenMlsAuthorityError::Message(format!("encode: {e}")))
    }

    /// Decode from an MLS application message plaintext.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, super::OpenMlsAuthorityError> {
        crate::cbor::from_slice(bytes)
            .map_err(|e| super::OpenMlsAuthorityError::Message(format!("decode: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_history_entitles_the_whole_range() {
        assert_eq!(HistoryPolicy::Full.entitled_range(1), 1..=1);
        assert_eq!(HistoryPolicy::Full.entitled_range(5), 1..=5);
    }

    #[test]
    fn capped_history_entitles_only_the_last_n_epochs() {
        // Last 2 epochs at ceiling 5 → {4, 5}.
        assert_eq!(HistoryPolicy::Capped(2).entitled_range(5), 4..=5);
        // Cap wider than history → whole range (equivalent to Full).
        assert_eq!(HistoryPolicy::Capped(10).entitled_range(3), 1..=3);
        // Cap of 1 → current epoch only.
        assert_eq!(HistoryPolicy::Capped(1).entitled_range(7), 7..=7);
        // Cap of 0 floors to the current epoch (never an empty range).
        assert_eq!(HistoryPolicy::Capped(0).entitled_range(7), 7..=7);
    }

    #[test]
    fn entitled_range_is_a_pure_function_of_policy_and_ceiling() {
        // The consistency invariant: identical (policy, ceiling) ⇒ identical range, regardless of
        // any per-add context — there is no per-add input to this function at all.
        for ceiling in 1..=8u32 {
            for policy in [HistoryPolicy::Full, HistoryPolicy::Capped(3)] {
                assert_eq!(
                    policy.entitled_range(ceiling),
                    policy.entitled_range(ceiling)
                );
            }
        }
    }

    #[test]
    fn app_payload_round_trips_through_canonical_cbor() {
        let kd = MlsAppPayload::KeyDistribution(AlbumKeyDistribution {
            amk_version: 3,
            amk_bytes: [7u8; AMK_LEN],
        });
        let bytes = kd.to_bytes().unwrap();
        assert_eq!(MlsAppPayload::from_bytes(&bytes).unwrap(), kd);
    }
}
