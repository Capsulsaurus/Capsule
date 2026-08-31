//! Custody receipts (`S-C15`) — the server's signed admission of what it accepted.
//!
//! # What a receipt is for
//!
//! The manifest envelope proves what a client *claimed and signed*. A receipt proves what the
//! server *accepted*, over a ciphertext hash the server recomputed itself. Before dropping the
//! only local copy of a photo a client requires a verified receipt for the write, so a server
//! that quietly withholds receipts never becomes the sole holder of an only copy — and one that
//! later loses the bytes has already signed a statement it cannot take back.
//!
//! The type is [`capsule_core::crypto::receipts::CustodyReceipt`], shared with the client rather
//! than mirrored (`S-C46`). A signed structure defined at both ends is one added field away from
//! a signature that stops verifying.
//!
//! # The chain is the log, and it is minted where it is written
//!
//! Every receipt carries a `receipt_seq` strictly monotonic per server and a
//! `prior_receipt_hash` over its predecessor. Both are **inside the signed core**, so the
//! signature cannot be computed until the position is allocated — which is why
//! [`ReceiptLog::issue`] takes a [`ReceiptSigner`] and does the whole thing in one operation
//! rather than handing a caller a sequence number to sign against. A caller that read the head,
//! signed, and then appended would let two concurrent finalizations sign the same position, and
//! the chain would fork with both halves validly signed. That is the `S-C37` lesson in a place
//! where getting it wrong produces *forged-looking* evidence rather than a missing entry.
//!
//! The signer is passed in rather than held by the store: a log is a table, and key material has
//! no business in one.
//!
//! # Atomicity, honestly
//!
//! The contract asks that the receipt and the asset's `uploaded` flip commit **together**.
//! Across two in-memory ports they cannot: they are two writes, and all that is available is an
//! order. The order chosen guarantees the direction that matters — **no receipt without
//! custody**. Custody is recorded first, so a crash between them leaves a finalized blob with no
//! receipt (visible as `error.upload.receipt_not_available`, and reissuable), never a signed
//! statement that the server holds bytes it does not.
//!
//! The other direction is the unrecoverable one: a receipt is evidence a client may keep
//! forever, and one attesting to a write that never landed cannot be withdrawn from whoever
//! already has it. Real atomicity arrives with the Postgres adapter, where both are rows in one
//! transaction; until then this is an ordering with a stated failure mode rather than a
//! guarantee dressed up as one.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::keys::hybrid_sig::{HybridSignature, HybridSigningKey};
use capsule_core::crypto::receipts::{CustodyReceipt, CustodyReceiptCore};

use crate::store::{AssetId, StoreFuture, UploadId};

/// The schema version every receipt this server issues carries.
pub const RECEIPT_VERSION: &str = "custody-receipt/v1";

/// Whatever can sign a receipt core under the server's attestation key.
///
/// A trait so the key can be a process-local `HybridSigningKey` today and an HSM handle later
/// without the log or the finalization path noticing. It carries the two identifying fields the
/// receipt commits to, because a signature and the identity it is attributed to have to come
/// from the same place — a signer that let a caller supply its own `server_key_id` would let a
/// receipt name a key that did not sign it.
pub trait ReceiptSigner: std::fmt::Debug + Send + Sync {
    /// This server's canonical origin. Binds a receipt to one server, which is what makes a
    /// cross-server replay refusable.
    fn server_id(&self) -> &str;

    /// The attestation key's fingerprint. Survives rotation: a pre-rotation receipt still
    /// verifies because it names the key that signed it.
    fn key_id(&self) -> Hash32;

    /// Sign the canonical core bytes.
    fn sign(&self, bytes: &[u8]) -> HybridSignature;
}

/// A process-local attestation key.
///
/// The **attestation** key, deliberately distinct from the operational one that signs access
/// tokens: they have different lifetimes, different blast radii, and a receipt that verified
/// under the token key would let anything holding that key manufacture custody evidence.
pub struct LocalAttestationKey {
    server_id: String,
    signing: HybridSigningKey,
    key_id: Hash32,
}

/// Hand-written so a key never reaches a log line.
///
/// `HybridSigningKey` implements no `Debug` of its own, which is the right default and the
/// reason this is spelled out rather than derived: the identity is printable, the secret is not.
impl std::fmt::Debug for LocalAttestationKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalAttestationKey")
            .field("server_id", &self.server_id)
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

impl LocalAttestationKey {
    /// Build from a signing key and this server's origin.
    ///
    /// The fingerprint is derived from the verifying key rather than supplied, so it cannot
    /// disagree with the key that actually signs.
    pub fn new(server_id: impl Into<String>, signing: HybridSigningKey) -> Self {
        let key_id = hash_bytes(
            &capsule_core::cbor::to_canonical_vec(&signing.verifying_key())
                .expect("a verifying key serializes"),
        );
        Self {
            server_id: server_id.into(),
            signing,
            key_id,
        }
    }

    /// The public half, for publication in the key history clients pin against.
    pub fn verifying_key(&self) -> capsule_core::crypto::keys::HybridVerifyingKey {
        self.signing.verifying_key()
    }
}

impl ReceiptSigner for LocalAttestationKey {
    fn server_id(&self) -> &str {
        &self.server_id
    }

    fn key_id(&self) -> Hash32 {
        self.key_id
    }

    fn sign(&self, bytes: &[u8]) -> HybridSignature {
        self.signing.sign(bytes)
    }
}

/// Everything a receipt needs that is not the chain position or the signature.
///
/// The facts the *server* established, not the ones the client declared: `ciphertext_hash` and
/// `size` are what finalization recomputed and stored, which is the whole evidentiary value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptDraft {
    /// The primitive bundle the write used.
    pub crypto_suite_id: u16,
    /// The album's pinned protocol date.
    pub protocol_version: String,
    /// The session that produced custody.
    pub upload_id: UploadId,
    /// The asset the blob belongs to.
    pub asset_id: AssetId,
    /// The blob's role, as the wire spells it.
    pub blob_role: String,
    /// The content address the server recomputed over the bytes it stored.
    pub ciphertext_hash: Hash32,
    /// Those bytes' length.
    pub size: u64,
    /// The manifest envelope's hash, when the write carries one.
    pub envelope_hash: Option<Hash32>,
    /// The account the storage is attributed to.
    pub uploaded_by_user: String,
    /// The device that uploaded, when the manifest named one.
    pub uploaded_by_device: Option<String>,
    /// The server's own clock at the commit.
    pub received_at: String,
}

/// The append-only receipt log.
///
/// Append-only is a property of the port, not a convention: there is no operation that replaces
/// or removes a receipt, so invariant 33's "any overwrite or delete attempt is rejected at the
/// structural layer" is satisfied by the absence of a method rather than by a check.
pub trait ReceiptLog: std::fmt::Debug + Send + Sync {
    /// Allocate the next chain position, sign, and append — one operation.
    ///
    /// See the module docs for why this is not three.
    fn issue<'a>(
        &'a self,
        draft: ReceiptDraft,
        signer: &'a dyn ReceiptSigner,
    ) -> StoreFuture<'a, CustodyReceipt>;

    /// The receipt issued for `upload`, if one was.
    fn for_upload<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, Option<CustodyReceipt>>;

    /// Every receipt issued for `asset`, in chain order.
    fn for_asset<'a>(&'a self, asset: &'a AssetId) -> StoreFuture<'a, Vec<CustodyReceipt>>;
}

/// A deterministic in-memory log.
#[derive(Debug, Default)]
pub struct InMemoryReceipts {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// Every receipt in issue order. The chain, materialised.
    chain: Vec<CustodyReceipt>,
    /// Which position each upload's receipt sits at, so a lookup is not a scan.
    by_upload: BTreeMap<UploadId, usize>,
}

impl InMemoryReceipts {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Take the lock, recovering from a poisoned mutex.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl ReceiptLog for InMemoryReceipts {
    fn issue<'a>(
        &'a self,
        draft: ReceiptDraft,
        signer: &'a dyn ReceiptSigner,
    ) -> StoreFuture<'a, CustodyReceipt> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);

            // A retried finalization must not mint a second receipt for one custody event: the
            // chain would then carry two signed statements about the same bytes, which is
            // indistinguishable from the server double-counting.
            if let Some(index) = inner.by_upload.get(&draft.upload_id) {
                return Ok(inner.chain[*index].clone());
            }

            // Position and predecessor, allocated under the lock the append happens under.
            let receipt_seq = inner.chain.len() as u64 + 1;
            let prior_receipt_hash = inner
                .chain
                .last()
                .map(|prior| hash_bytes(&prior.to_canonical_cbor()));

            let core = CustodyReceiptCore {
                version: RECEIPT_VERSION.to_owned(),
                crypto_suite_id: draft.crypto_suite_id,
                protocol_version: draft.protocol_version,
                server_id: signer.server_id().to_owned(),
                server_key_id: signer.key_id(),
                receipt_seq,
                prior_receipt_hash,
                upload_id: draft.upload_id.as_str().to_owned(),
                asset_id: draft.asset_id.as_str().to_owned(),
                blob_role: draft.blob_role,
                ciphertext_hash: draft.ciphertext_hash,
                size: draft.size,
                envelope_hash: draft.envelope_hash,
                uploaded_by_user: draft.uploaded_by_user,
                uploaded_by_device: draft.uploaded_by_device,
                received_at: draft.received_at,
            };
            let receipt = CustodyReceipt {
                server_sig: signer.sign(&core.signing_bytes()),
                core,
            };

            tracing::info!(
                upload_id = %draft.upload_id,
                asset = %draft.asset_id,
                receipt_seq,
                "issued a custody receipt"
            );
            let position = inner.chain.len();
            inner.by_upload.insert(draft.upload_id, position);
            inner.chain.push(receipt.clone());
            Ok(receipt)
        })
    }

    fn for_upload<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, Option<CustodyReceipt>> {
        Box::pin(async move {
            let inner = lock(&self.inner);
            Ok(inner
                .by_upload
                .get(upload)
                .map(|index| inner.chain[*index].clone()))
        })
    }

    fn for_asset<'a>(&'a self, asset: &'a AssetId) -> StoreFuture<'a, Vec<CustodyReceipt>> {
        Box::pin(async move {
            Ok(lock(&self.inner)
                .chain
                .iter()
                .filter(|receipt| receipt.core.asset_id == asset.as_str())
                .cloned()
                .collect())
        })
    }
}

/// The attestation module's collaborators.
#[derive(Debug, Clone)]
pub struct AttestationContext {
    receipts: Arc<dyn ReceiptLog>,
    signer: Arc<dyn ReceiptSigner>,
}

impl AttestationContext {
    /// Assembles the module from its log and its key.
    pub fn new(receipts: Arc<dyn ReceiptLog>, signer: Arc<dyn ReceiptSigner>) -> Self {
        Self { receipts, signer }
    }

    /// The append-only log.
    pub fn receipts(&self) -> &dyn ReceiptLog {
        self.receipts.as_ref()
    }

    /// The attestation key.
    pub fn signer(&self) -> &dyn ReceiptSigner {
        self.signer.as_ref()
    }
}

#[cfg(test)]
mod tests;
