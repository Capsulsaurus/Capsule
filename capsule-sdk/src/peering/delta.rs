//! Determining the delta: what each side is missing.
//!
//! Before building an artifact the two devices must agree on what to move. Peering **reuses the
//! sync-cursor model** rather than inventing a diff: each side offers its set of held
//! [ciphertext content addresses](capsule_core::crypto::hash::Hash32) and its cursor, and the
//! delta is the *complement*. "What changed" is already defined by the sync feed — peering
//! borrows that definition wholesale. Because every blob is content-addressed, dedup is free:
//! the complement already excludes anything the receiver holds.
//!
//! The addresses a sync offer carries are the **head record hashes** of the assets a device
//! holds ([`record_hash`] of each chain's last record): an asset whose head the
//! receiver already holds is what the complement leaves out, and a metadata edit that keeps the
//! ciphertext still moves, because it changes the head.
//!
//! On the wire an offer is split into pages of at most [`OFFER_PAGE_CAP`] addresses, and each
//! page is **padded to a size bucket** (a power of two of address slots, at least
//! [`MIN_OFFER_BUCKET`]), so the encrypted channel's record sizes reveal only a coarse bucket of
//! how much a device holds, and no single request can carry an unbounded offer. A whole offer is
//! at most [`MAX_OFFER_PAGES`] pages.
//!
//! [`record_hash`]: capsule_core::crypto::provenance::ProvenanceRecord::record_hash

use std::collections::BTreeSet;

use capsule_core::backup::BackupAsset;
use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::provenance::ProvenanceRecord;

use super::PeeringError;

/// The most addresses one offer page carries — the per-request cap.
pub const OFFER_PAGE_CAP: usize = 4096;

/// The smallest size bucket, in address slots, a page is padded to.
pub const MIN_OFFER_BUCKET: usize = 64;

/// The most pages one offer is carried in, so a whole offer (at most
/// `MAX_OFFER_PAGES * OFFER_PAGE_CAP` addresses) is bounded too, not only each page.
pub const MAX_OFFER_PAGES: usize = 256;

/// Bytes of a page header: the cursor (`u64`) then the real address count (`u32`), big-endian.
const PAGE_HEADER: usize = 12;

/// Bytes of one address slot.
const SLOT: usize = 32;

/// The largest encoded page: a full page padded to the cap's bucket.
pub(crate) const MAX_OFFER_PAGE_BYTES: usize = PAGE_HEADER + OFFER_PAGE_CAP * SLOT;

/// The bucket, in address slots, a page holding `count` addresses is padded to.
#[must_use]
pub fn offer_bucket(count: usize) -> usize {
    count.max(MIN_OFFER_BUCKET).next_power_of_two()
}

/// One decoded offer page: the sender's cursor and the real addresses it carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferPage {
    /// The sender's sync-cursor high-water mark.
    pub cursor: u64,
    /// The real (unpadded) addresses on this page.
    pub addresses: Vec<Hash32>,
}

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

    /// Encode the offer as wire pages: at most [`OFFER_PAGE_CAP`] addresses each, every page
    /// zero-padded to its [`offer_bucket`]. An empty offer is one empty, padded page.
    #[must_use]
    pub fn encode_pages(&self) -> Vec<Vec<u8>> {
        let addresses: Vec<&Hash32> = self.addresses.iter().collect();
        let mut chunks: Vec<&[&Hash32]> = addresses.chunks(OFFER_PAGE_CAP).collect();
        if chunks.is_empty() {
            chunks.push(&[]);
        }
        chunks
            .into_iter()
            .map(|chunk| {
                let slots = offer_bucket(chunk.len());
                let mut page = Vec::with_capacity(PAGE_HEADER + slots * SLOT);
                page.extend_from_slice(&self.cursor.to_be_bytes());
                // `chunk.len() <= OFFER_PAGE_CAP`, which fits a u32.
                page.extend_from_slice(&(chunk.len() as u32).to_be_bytes());
                for address in chunk {
                    page.extend_from_slice(&address.0);
                }
                page.resize(PAGE_HEADER + slots * SLOT, 0);
                page
            })
            .collect()
    }

    /// Reassemble an offer from its decoded pages. Every page must carry the same cursor.
    pub fn from_pages(pages: &[OfferPage]) -> Result<Self, PeeringError> {
        let cursor = pages.first().map_or(0, |page| page.cursor);
        if pages.iter().any(|page| page.cursor != cursor) {
            return Err(PeeringError::Codec(
                "offer pages disagree on the cursor".into(),
            ));
        }
        let addresses = pages
            .iter()
            .flat_map(|page| page.addresses.iter().copied())
            .collect();
        Ok(Self { addresses, cursor })
    }
}

/// Decode one wire offer page, refusing anything [`Offer::encode_pages`] would not produce: a
/// size that is not a bucket, a bucket above the cap, a count above the bucket, or non-zero
/// padding.
pub fn decode_offer_page(bytes: &[u8]) -> Result<OfferPage, PeeringError> {
    let codec = |what: &str| PeeringError::Codec(format!("offer page: {what}"));
    let body = bytes
        .get(PAGE_HEADER..)
        .ok_or_else(|| codec("shorter than its header"))?;
    if body.len() % SLOT != 0 {
        return Err(codec("not a whole number of slots"));
    }
    let slots = body.len() / SLOT;
    if slots > offer_bucket(OFFER_PAGE_CAP) || slots != offer_bucket(slots) {
        return Err(codec("size is not an admitted bucket"));
    }
    let mut cursor = [0u8; 8];
    cursor.copy_from_slice(&bytes[..8]);
    let mut count = [0u8; 4];
    count.copy_from_slice(&bytes[8..PAGE_HEADER]);
    let count = u32::from_be_bytes(count) as usize;
    if count > slots.min(OFFER_PAGE_CAP) {
        return Err(codec("count exceeds the page"));
    }
    let (real, padding) = body.split_at(count * SLOT);
    if padding.iter().any(|byte| *byte != 0) {
        return Err(codec("non-zero padding"));
    }
    let addresses = real
        .chunks_exact(SLOT)
        .map(|slot| {
            let mut address = [0u8; SLOT];
            address.copy_from_slice(slot);
            Hash32(address)
        })
        .collect();
    Ok(OfferPage {
        cursor: u64::from_be_bytes(cursor),
        addresses,
    })
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

/// The address an asset is offered under: its chain's head record hash. `None` for an asset with
/// no provenance, which no offer can name.
#[must_use]
pub fn head_address(asset: &BackupAsset) -> Option<Hash32> {
    asset.provenance.last().map(ProvenanceRecord::record_hash)
}

/// Cut `assets` to the ones `receiver` is missing: [`missing_from`]`(receiver, ours)` over their
/// [`head_address`]es. Returns what remains and how many the receiver's offer excluded. An asset
/// with no head address is kept, since no offer can show the receiver holds it.
#[must_use]
pub fn cut_to_missing(assets: Vec<BackupAsset>, receiver: &Offer) -> (Vec<BackupAsset>, usize) {
    let ours = Offer::new(assets.iter().filter_map(head_address).collect(), 0);
    let missing = missing_from(receiver, &ours);
    let before = assets.len();
    let kept: Vec<BackupAsset> = assets
        .into_iter()
        .filter(|asset| head_address(asset).is_none_or(|head| missing.contains(&head)))
        .collect();
    let excluded = before - kept.len();
    tracing::debug!(
        offered = receiver.addresses.len(),
        kept = kept.len(),
        excluded,
        "peering: delta cut from the receiver's offer"
    );
    (kept, excluded)
}
