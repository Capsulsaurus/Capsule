//! Determining the delta: what each side is missing.
//!
//! Before building an artifact the two devices must agree on what to move. Peering **reuses the
//! sync-cursor model** rather than inventing a diff: each side offers its set of held
//! [ciphertext content addresses](capsule_core::crypto::hash::Hash32) and its cursor, and the
//! delta is the *complement*. "What changed" is already defined by the sync feed — peering
//! borrows that definition wholesale. Because every blob is content-addressed, dedup is free:
//! the complement already excludes anything the receiver holds.

use std::collections::BTreeSet;

use capsule_core::crypto::hash::Hash32;

/// One side's offer in the delta negotiation: the content addresses it currently holds and its
/// sync cursor high-water mark. The cursor is carried for parity with server sync (a peer only
/// offers state it has itself reconciled up to); the set is what the complement is computed over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// The ciphertext content addresses this device holds.
    pub addresses: BTreeSet<Hash32>,
    /// This device's sync-cursor high-water mark.
    pub cursor: u64,
}

impl Offer {
    /// An offer over an explicit address set and cursor.
    #[must_use]
    pub fn new(addresses: BTreeSet<Hash32>, cursor: u64) -> Self {
        Self { addresses, cursor }
    }
}

/// The addresses `local` is **missing** relative to `remote` — the pull set the behind-device
/// requests. This is `remote \ local`: everything the remote holds that we do not, which is
/// exactly the set to transfer (content-addressing makes anything we already hold a no-op, so
/// the complement doubles as dedup).
#[must_use]
pub fn missing_from(local: &Offer, remote: &Offer) -> BTreeSet<Hash32> {
    remote
        .addresses
        .difference(&local.addresses)
        .copied()
        .collect()
}

/// The **symmetric difference** of two offers — every address exactly one side holds. Over two
/// devices with overlapping-but-distinct content, this is the union of the two one-way pulls
/// (`(a \ b) ∪ (b \ a)`): the total set of assets that must move for the pair to converge.
#[must_use]
pub fn symmetric_difference(a: &Offer, b: &Offer) -> BTreeSet<Hash32> {
    a.addresses
        .symmetric_difference(&b.addresses)
        .copied()
        .collect()
}
