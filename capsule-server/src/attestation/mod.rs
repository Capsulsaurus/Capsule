//! Custody receipts — the server's signed admission of what it accepted — and the
//! signed storage attestation ([`sign_storage_attestation`]) the same key puts on a durability
//! verdict when a client asks for one.
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
//! than mirrored. A signed structure defined at both ends is one added field away from
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
//! # Issued with the index rows, in one transaction
//!
//! The contract (threat-model invariant 33, and Transfer — Upload § Finalization) is that the
//! receipt and the index rows recording the blob commit **together**: there is never an indexed
//! blob without a receipt, and never a receipt without custody. Both directions matter. A
//! receipt is evidence a client may keep forever, so one attesting to a write that never landed
//! cannot be withdrawn from whoever already has it; and an indexed blob with no receipt is a
//! write the client is never told it may release its local copy for.
//!
//! So finalization does not call [`ReceiptLog::issue`] after the index. It hands the draft and
//! the signer to the index itself — [`crate::index::AssetIndex::record_blob_with_receipt`] and
//! [`crate::index::AssetIndex::apply_op_with_receipt`] — and each index adapter appends the
//! receipt inside the critical section that writes its rows: the Postgres index in the same
//! transaction, through `postgres::issue_in`, over the same `custody_receipts` table this log
//! reads; the in-memory index under its own lock, through `InMemoryReceipts::append`, into the
//! log it shares with the [`AttestationContext`].
//!
//! [`ReceiptLog::issue`] remains for the one finalization that writes no index row: a member of
//! a `replace` bundle other than its manifest, whose bytes the manifest applies later. With
//! nothing else to commit, the receipt is the whole durable write and is atomic on its own.
//!
//! # Adapters
//!
//! [`InMemoryReceipts`] is the deterministic double, [`postgres::PostgresReceipts`] the durable
//! log, and [`conformance`] the suite both pass. Both build the receipt through one shared
//! function, so the signed shape cannot differ between them.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::keys::{HybridSignature, HybridSigningKey};
use capsule_core::crypto::receipts::{CustodyReceipt, CustodyReceiptCore};
use capsule_core::crypto::storage_attestation::{
    AttestationNonce, AttestedAsset, STORAGE_ATTESTATION_VERSION, StorageAttestation,
    StorageAttestationCore,
};
use jiff::Timestamp;

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

    /// The public half.
    ///
    /// On the *signer* rather than beside it, because a key that cannot be published cannot
    /// produce verifiable receipts — and a receipt nobody can verify is the server claiming
    /// accountability it does not have. Requiring it here makes publishing the active key a
    /// consequence of being able to sign at all.
    fn verifying_key(&self) -> capsule_core::crypto::keys::HybridVerifyingKey;

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

/// One key in the published, append-only history.
///
/// Append-only is what makes a receipt verifiable for the life of the asset it covers: a
/// receipt names the key that signed it, so a key retired years ago must still be resolvable
/// or every receipt under it becomes unverifiable at once — which is indistinguishable, from
/// the outside, from the server having forged them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedKey {
    /// The fingerprint a receipt's `server_key_id` selects on.
    pub key_id: Hash32,
    /// The public half.
    pub public: capsule_core::crypto::keys::HybridVerifyingKey,
    /// When it began signing.
    pub active_from: Timestamp,
    /// When it stopped, or `None` while it is the active key.
    pub active_to: Option<Timestamp>,
}

impl ReceiptSigner for LocalAttestationKey {
    fn server_id(&self) -> &str {
        &self.server_id
    }

    fn key_id(&self) -> Hash32 {
        self.key_id
    }

    fn verifying_key(&self) -> capsule_core::crypto::keys::HybridVerifyingKey {
        self.signing.verifying_key()
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

    /// Allocate, sign and append in one step, synchronously.
    ///
    /// The body of [`ReceiptLog::issue`], exposed so the in-memory asset index can append a
    /// receipt while it holds its own lock — which is what makes the index rows and the receipt
    /// one critical section in the double, as one transaction makes them one in Postgres. It
    /// cannot fail, so there is no state in which the rows landed and the receipt did not.
    ///
    /// Idempotent per upload: a retried finalization gets the receipt it already has.
    pub(crate) fn append(&self, draft: ReceiptDraft, signer: &dyn ReceiptSigner) -> CustodyReceipt {
        let mut inner = lock(&self.inner);

        // A retried finalization must not mint a second receipt for one custody event: the
        // chain would then carry two signed statements about the same bytes, which is
        // indistinguishable from the server double-counting.
        if let Some(index) = inner.by_upload.get(&draft.upload_id) {
            return inner.chain[*index].clone();
        }

        // Position and predecessor, allocated under the lock the append happens under.
        let receipt_seq = inner.chain.len() as u64 + 1;
        let prior_receipt_hash = inner.chain.last().map(chain_hash);

        let upload_id = draft.upload_id.clone();
        let receipt = sign_receipt(draft, signer, receipt_seq, prior_receipt_hash);

        tracing::info!(
            upload_id = %upload_id,
            asset = %receipt.core.asset_id,
            receipt_seq,
            "issued a custody receipt"
        );
        let position = inner.chain.len();
        inner.by_upload.insert(upload_id, position);
        inner.chain.push(receipt.clone());
        receipt
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
        Box::pin(async move { Ok(self.append(draft, signer)) })
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

/// Sign `draft` at chain position `receipt_seq`, after the receipt `prior_receipt_hash` names.
///
/// The one place a receipt core is assembled, shared by every [`ReceiptLog`] adapter. The
/// position and the predecessor are the adapter's to allocate under its own lock; everything
/// else about the receipt is decided here, so two adapters cannot sign two different shapes of
/// the same custody event.
pub(crate) fn sign_receipt(
    draft: ReceiptDraft,
    signer: &dyn ReceiptSigner,
    receipt_seq: u64,
    prior_receipt_hash: Option<Hash32>,
) -> CustodyReceipt {
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
        unknown: Default::default(),
    };
    CustodyReceipt {
        server_sig: signer.sign(&core.signing_bytes()),
        core,
    }
}

/// Sign a durability verdict as a [`StorageAttestation`] for `account`, echoing `nonce`.
///
/// Under the same attestation key as a receipt, and for the same reason: the attestation is
/// evidence a client keeps, and only the key whose history the server publishes append-only
/// makes it verifiable for as long as the client keeps it. The two signed shapes cannot be
/// confused — the core's `version` and field set differ, so neither decodes as the other.
///
/// Not logged in the chain: a verdict is a point-in-time read, not a custody event, and a log
/// entry per audit would make the receipt log's length a function of how often clients ask.
pub fn sign_storage_attestation(
    signer: &dyn ReceiptSigner,
    account: &str,
    nonce: AttestationNonce,
    checked_at: String,
    verdicts: Vec<AttestedAsset>,
) -> StorageAttestation {
    let core = StorageAttestationCore {
        version: STORAGE_ATTESTATION_VERSION.to_owned(),
        server_id: signer.server_id().to_owned(),
        server_key_id: signer.key_id(),
        account: account.to_owned(),
        nonce,
        checked_at,
        verdicts,
    };
    tracing::debug!(
        %account,
        assets = core.verdicts.len(),
        "signed a storage attestation"
    );
    StorageAttestation {
        server_sig: signer.sign(&core.signing_bytes()),
        core,
    }
}

/// The hash a receipt's successor names as its `prior_receipt_hash`.
pub(crate) fn chain_hash(receipt: &CustodyReceipt) -> Hash32 {
    hash_bytes(&receipt.to_canonical_cbor())
}

/// The attestation module's collaborators.
///
/// The [owner ledger](crate::ledger) rides here because the attestation key signs its entries
/// exactly as it signs receipts, and every writer — finalization, a drop adoption, a lifecycle
/// op, a directory publish — already holds this context.
#[derive(Debug, Clone)]
pub struct AttestationContext {
    receipts: Arc<dyn ReceiptLog>,
    signer: Arc<dyn ReceiptSigner>,
    history: Vec<PublishedKey>,
    ledger: Arc<dyn crate::ledger::OwnerLedger>,
}

impl AttestationContext {
    /// Assembles the module from its log and its active key.
    ///
    /// The active key's published entry is **derived from the signer**, never supplied
    /// alongside it. A server that issued receipts naming a key it did not publish would emit
    /// evidence nobody can check, and that failure is silent on the issuing side — so it is
    /// made unrepresentable rather than validated.
    pub fn new(
        receipts: Arc<dyn ReceiptLog>,
        signer: Arc<dyn ReceiptSigner>,
        active_from: Timestamp,
    ) -> Self {
        let history = vec![PublishedKey {
            key_id: signer.key_id(),
            public: signer.verifying_key(),
            active_from,
            active_to: None,
        }];
        Self {
            receipts,
            signer,
            history,
            // The deterministic double until a deployment names its blob root's ledger: a
            // context nobody gave a ledger still records, so no test path skips the append.
            ledger: Arc::new(crate::ledger::InMemoryLedger::new()),
        }
    }

    /// The same, appending to `ledger` — the blob root's, in a deployment.
    #[must_use]
    pub fn with_ledger(mut self, ledger: Arc<dyn crate::ledger::OwnerLedger>) -> Self {
        self.ledger = ledger;
        self
    }

    /// The owner ledger every index and directory write appends to.
    pub fn ledger(&self) -> &dyn crate::ledger::OwnerLedger {
        self.ledger.as_ref()
    }

    /// Append `event` to `owner`'s ledger, signed by this context's attestation key.
    ///
    /// The one call every producer makes, so no producer can sign an entry under a key other
    /// than the one its receipts name. Durable before it returns, as
    /// [`OwnerLedger::append`](crate::ledger::OwnerLedger::append) is.
    pub async fn append_to_ledger(
        &self,
        owner: &crate::store::OwnerId,
        event: crate::ledger::LedgerEvent,
        recorded_at: Timestamp,
    ) -> Result<crate::ledger::LedgerEntry, crate::ledger::LedgerError> {
        let kind = event.kind();
        let entry = self
            .ledger
            .append(owner, event, recorded_at, self.signer.as_ref())
            .await?;
        tracing::debug!(%owner, kind, ledger_seq = entry.core.seq, "appended an owner-ledger entry");
        Ok(entry)
    }

    /// The same, with keys this server has retired.
    ///
    /// Prepended, so the published history reads oldest-first and the active key is last —
    /// which is the order a reader walking for a `server_key_id` wants and the order a rotation
    /// appends in.
    #[must_use]
    pub fn with_retired(mut self, retired: Vec<PublishedKey>) -> Self {
        let mut history = retired;
        history.extend(self.history);
        self.history = history;
        self
    }

    /// The published, append-only key history — oldest first, active last.
    pub fn history(&self) -> &[PublishedKey] {
        &self.history
    }

    /// The published key `key_id` names, if this server has ever signed with it.
    ///
    /// What a `server_key_id` resolves through, and what makes rotation continuous.
    pub fn resolve(&self, key_id: &Hash32) -> Option<&PublishedKey> {
        self.history.iter().find(|key| &key.key_id == key_id)
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

pub mod conformance;
pub mod postgres;

pub use self::postgres::PostgresReceipts;

#[cfg(test)]
mod tests;
