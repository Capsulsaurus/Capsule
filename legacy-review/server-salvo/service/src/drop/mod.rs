//! The web-upload drop store: upload links, the owner inbox, and atomic adoption
//! (slice `S-C5`).
//!
//! This is the DB layer the media drop routes (`capsule-api-media::drops`) orchestrate. It
//! owns the three server responsibilities the [Web Upload design doc] fixes:
//!
//! 1. **Link resolution + reservation** ([`Query::live_link_by_opaque`], [`Mutation::open_drop_reservation`]).
//!    A drop-session creation resolves a **live** upload link (invariant 26) and, in one
//!    transaction, enforces the cumulative per-link caps, debits the **provisioning owner's**
//!    quota (invariant 29 — `upload_user_id = owner_id`), reserves the drop's original blob in
//!    the quota ledger, and advances the link's cumulative-cap counters.
//! 2. **Inbox staging** ([`Mutation::stage_drop`]). A finalized drop blob becomes a
//!    `drop_inbox` row — never an album asset, never on any sync feed.
//! 3. **Atomic adoption** ([`Mutation::adopt`], invariant 32). The adopter's signed `create`
//!    manifest promotes the inbox blob to an `assets` row, mints the album's `sync_seq` +
//!    appends the provenance-bearing feed entry (S-C2's rule), releases the original
//!    reservation, charges the new metadata blob, and deletes the inbox row — **all in one
//!    transaction**, so a crash between any two steps leaves no half-adopted drop.
//!
//! [Web Upload design doc]: ../../../../capsule-docs/src/content/docs/design/web-upload.md

mod mutation;
mod query;

use jiff::Timestamp;
pub use mutation::{AdoptOutcome, DiscardedDrop, Mutation};
pub use query::{LiveLink, PendingDrop, Query};
use thiserror::Error;

/// A failure surfaced by the drop store. The media routes map each variant to its transport
/// status + stable `error.*` code (the `error.drop.*` namespace, plus the reused
/// `error.quota.*` / `error.upload.*` codes for the shared invariants).
#[derive(Debug, Error)]
pub enum DropError {
    /// The opaque id does not resolve to a live link (not found, revoked, or expired). The
    /// serve path renders this as an **indistinguishable `404`** — never `410` (invariant 26).
    #[error("upload link not found, revoked, or expired")]
    LinkNotFound,
    /// A cumulative per-link cap is already exhausted (invariant 26): the drop would push the
    /// link past its byte or file-count cap.
    #[error("per-link cap exhausted: {0}")]
    CapExceeded(&'static str),
    /// The provisioning owner's quota would be crossed by the declared size (invariant 29).
    #[error("owner quota exceeded")]
    QuotaExceeded,
    /// The provisioning owner's account is Grace-expired (read-only), so the adoption's
    /// metadata-growth write is refused.
    #[error("owner account is grace-expired (read-only)")]
    GraceLocked,
    /// The referenced drop blob is not a pending drop in the caller's own inbox (invariant 32).
    #[error("drop is not in the caller's inbox")]
    NotInInbox,
    /// A database failure.
    #[error(transparent)]
    Db(#[from] sea_orm::DbErr),
}

/// A request to register a new upload link (the server half of the Provision step). The
/// `opaque_id` is minted by the caller from `capsule_core::drop::generate_opaque_id` (a random
/// ≥128-bit token, never a structured id).
#[derive(Debug, Clone)]
pub struct NewLink {
    /// The provisioning user (whose quota drops through this link debit).
    pub owner_id: String,
    /// The random ≥128-bit opaque URL-path token (lowercase hex of 16 bytes).
    pub opaque_id: String,
    /// Optional destination-album hint (advisory).
    pub album_hint: Option<String>,
    /// Pinned wire protocol version (`YYYY-MM-DD`).
    pub protocol_version: String,
    /// Pinned crypto suite id.
    pub crypto_suite_id: u16,
    /// Cap: expiry instant (`None` = no expiry).
    pub expires_at: Option<Timestamp>,
    /// Cap: cumulative byte cap.
    pub max_total_bytes: Option<u64>,
    /// Cap: file-count cap.
    pub max_file_count: Option<u32>,
    /// Cap: per-file (ciphertext) size cap.
    pub max_file_size: Option<u64>,
    /// Cap: die after the first successful drop.
    pub single_use: bool,
    /// Optional Argon2id abuse-gate verifier (JSON of the S-A6 `PassphraseVerifier`).
    pub passphrase_verifier: Option<serde_json::Value>,
}

/// The inputs to [`Mutation::stage_drop`] — a finalized drop blob becoming an inbox row.
#[derive(Debug, Clone)]
pub struct StageInput {
    /// The inbox row id (UUIDv7).
    pub drop_id: String,
    /// The provisioning owner (inbox owner).
    pub owner_id: String,
    /// The link this drop arrived through.
    pub link_id: String,
    /// The content address of the staged drop blob.
    pub ciphertext_hash: String,
    /// Ciphertext size in bytes.
    pub size: u64,
    /// The guest-declared content type.
    pub content_type: String,
    /// Guest-supplied, unverified name.
    pub suggested_filename: Option<String>,
    /// The full `DropDescriptor` projection, carried opaquely.
    pub descriptor: serde_json::Value,
    /// Whether the link is single-use (revoked after this drop).
    pub single_use: bool,
}

/// The inputs to [`Mutation::adopt`] — the adopter's validated `create` manifest promoting an
/// inbox drop. The envelope battery (invariants 1–8, 16–18, 25) and album write-capability
/// have already run in the media route over the decoded manifest; this carries the extracted,
/// validated facts plus the opaque signed manifest for the feed.
#[derive(Debug, Clone)]
pub struct AdoptInput {
    /// The destination album (server id).
    pub album_id: String,
    /// The manifest's ciphertext hash — must reference the inbox blob (invariant 32).
    pub ciphertext_hash: String,
    /// The metadata blob's content address (== the manifest's `metadata_blob_hash`).
    pub metadata_hash: String,
    /// The encrypted metadata blob (inlined onto the feed entry; charged to the owner).
    pub metadata_blob: Vec<u8>,
    /// The signed `create` manifest as opaque canonical CBOR (the feed's provenance record).
    pub manifest_cbor: Vec<u8>,
    /// The album pin (`YYYY-MM-DD`).
    pub protocol_version: String,
}
