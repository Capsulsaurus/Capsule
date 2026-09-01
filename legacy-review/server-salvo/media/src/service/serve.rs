//! The key-free media serving engine (slice `S-C10`).
//!
//! Resolves a bare ciphertext **content address** to a serve decision, with no decryption key
//! and no plaintext-era assumptions: the server holds opaque ciphertext blobs and a key-free
//! index, so a serve is `index reference ∧ liveness ∧ bytes-on-disk` composed from the same
//! three facts [storage verification](super::verify) already tracks — never a filesystem
//! oracle over arbitrary hashes.
//!
//! The [`ServeResolution`] the route renders is the load-bearing status taxonomy:
//!
//! - **[`NotFound`](ServeResolution::NotFound)** (`404`) — no committed feed row names the
//!   hash (an unknown / never-indexed content address). This is also the answer for a
//!   malformed address, so the endpoint is not a blob-existence oracle.
//! - **[`Gone`](ServeResolution::Gone)** (`410`) — the blob is referenced but *not
//!   retrievable per policy*: its asset was **taken down** (moderation, `served = false`),
//!   quarantined (integrity fault) or mid-GC (`collectable_since` set), or its bytes are a
//!   dangling reference. Permanent → the client degrades gracefully (download-sync doc).
//!   A takedown answers `410` on *this* path too (slice `S-C17`) — this is the real
//!   client/federation fetch path, and [Moderation — Takedown] is explicit that federated
//!   peers fetching a taken-down asset receive `410`, deliberately distinct from the
//!   capability-URL surfaces' indistinguishable `404`.
//! - **[`PendingUpload`](ServeResolution::PendingUpload)** (`error.blob.pending_upload`) — the
//!   asset's **original** is legitimately not yet uploaded (`original_held = false`, the
//!   staged-upload `awaiting-original` state). Explicitly **transient, never `410`**: the
//!   client shows the badge and re-fetches when the feed flips `original_held`.
//! - **[`Serve`](ServeResolution::Serve)** — present ∧ indexed ∧ retrievable; the bytes are
//!   served with HTTP `Range` at the ciphertext stride (the route's concern).
//!
//! Byte serving itself is delegated to salvo's ranged file writer, so resumable partial reads
//! at the **65,536-byte ciphertext stride** (`capsule_core::crypto::encryption::stream::`
//! `CIPHERTEXT_CHUNK` — a 65,520-byte plaintext chunk plus its 16-byte GCM tag) are honored
//! without this module ever reading a whole blob into memory. The server serves opaque byte
//! ranges; the *stride alignment* is the client's concern — it requests `Range`s at
//! `chunk_index × 65,536` so each fetched chunk decrypts in isolation under core's
//! `decrypt_chunk`.
//!
//! SSoT: [Filesystem — Server], [Encryption — ranged reads], [Download & Sync], [Moderation —
//! Takedown].
//!
//! [Filesystem — Server]: ../../../../../capsule-docs/src/content/docs/design/filesystem/server.md
//! [Moderation — Takedown]: ../../../../../capsule-docs/src/content/docs/design/moderation.md
//! [Encryption — ranged reads]: ../../../../../capsule-docs/src/content/docs/design/cryptography/encryption.md
//! [Download & Sync]: ../../../../../capsule-docs/src/content/docs/design/import/download-sync.md

use std::path::PathBuf;

use entity::asset;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait, QuerySelect};
use service::blob_store;
use service::sync::Query as SyncQuery;
use tracing::{debug, info, instrument, trace};

/// The serve decision for one content address — the route maps each arm to its HTTP status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ServeResolution {
    /// Present ∧ indexed ∧ retrievable — serve the ciphertext at this on-disk path (the route
    /// applies HTTP `Range`).
    Serve {
        /// The content-addressed blob path (`blobs/{hash}.bin`).
        path: PathBuf,
    },
    /// No committed reference names the hash (or it is malformed) — unknown content address.
    /// `404`, no blob-existence oracle.
    NotFound,
    /// Referenced but gone per policy — quarantined, mid-GC, or a dangling reference. `410`.
    Gone,
    /// The asset's original is not yet uploaded (`awaiting-original`) — transient
    /// `error.blob.pending_upload`, **never** `410`.
    PendingUpload,
}

/// The key-free blob-serving engine. Cheap to clone/construct — it holds only the blob-store
/// root; all liveness facts are read per request from Postgres + a single `stat`.
#[derive(Clone)]
pub(crate) struct BlobServeService {
    /// The blob store root (`{upload_dir}/blobs/{hash}.bin`).
    upload_dir: PathBuf,
}

impl BlobServeService {
    /// The production service over `upload_dir`'s content-addressed blob tree.
    #[must_use]
    pub(crate) fn new(upload_dir: PathBuf) -> Self {
        Self { upload_dir }
    }

    /// Resolve a bare content address to a serve decision.
    ///
    /// The resolution order is the invariant: an unknown/malformed address is `NotFound`
    /// before any liveness read; a **taken-down** asset (`served = false`) is `Gone` before the
    /// GC read and before the `stat`, so a moderated blob's bytes are never touched on disk; a
    /// referenced-but-not-retrievable blob (quarantined or mid-GC) is `Gone` before the `stat`,
    /// so a blob mid-collection is never served even while its bytes survive the GC grace
    /// window; and a missing original is `PendingUpload` **only** when the asset is genuinely
    /// `awaiting-original`, never masking a real dangling reference.
    #[instrument(skip(self, conn), fields(hash = %hash))]
    pub(crate) async fn resolve<C: ConnectionTrait>(
        &self,
        conn: &C,
        hash: &str,
    ) -> Result<ServeResolution, DbErr> {
        // A malformed address can address no committed blob — answered as unknown, never an
        // oracle and never interpolated into a query.
        if !blob_store::is_content_hash(hash) {
            trace!("malformed content address → not found");
            return Ok(ServeResolution::NotFound);
        }

        // The `indexed` fact + the awaiting-original discriminator: the newest committed feed
        // reference that names the hash.
        let Some(reference) = SyncQuery::blob_serve_reference(conn, hash).await? else {
            debug!("no committed reference for content address → 404");
            return Ok(ServeResolution::NotFound);
        };

        // Moderation (slice `S-C17`): a taken-down asset is refused with `410` before any GC
        // read and before the on-disk stat — the takedown is a *serving* constraint, so the
        // bytes stay untouched (and undeleted) on disk. Federated peers see this `410` too;
        // that is the moderation doc's per-surface rule for content whose existence the peer
        // already knows.
        if !Self::asset_served(conn, &reference.asset_id).await? {
            info!(
                asset = %reference.asset_id,
                "blob fetch refused: asset taken down (served = false) → 410 gone"
            );
            return Ok(ServeResolution::Gone);
        }

        // Liveness: a quarantined or mid-GC blob is not retrievable per policy, decided before
        // the on-disk stat so its (still-present, grace-window) bytes are never served.
        let hashes = [hash.to_string()];
        let gc_states = service::gc::Query::blob_states(conn, &hashes).await?;
        let gc_state = gc_states.get(hash).copied().unwrap_or_default();
        if !gc_state.is_retrievable() {
            debug!(
                collectable = gc_state.collectable_since.is_some(),
                quarantined = gc_state.quarantined,
                "referenced blob not retrievable → 410 gone"
            );
            return Ok(ServeResolution::Gone);
        }

        // Bytes on disk?
        let path = blob_store::blob_path(&self.upload_dir, hash);
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            trace!(role = %reference.role, "serving ciphertext blob");
            return Ok(ServeResolution::Serve { path });
        }

        // Missing bytes. A missing **original** on an `awaiting-original` asset is expected
        // staged-upload state (transient), not corruption; anything else is a dangling
        // reference the client must treat as gone.
        if reference.role == "original" && !reference.original_held {
            debug!(
                asset = %reference.asset_id,
                "original not yet uploaded (awaiting-original) → pending_upload"
            );
            return Ok(ServeResolution::PendingUpload);
        }

        debug!(
            role = %reference.role,
            asset = %reference.asset_id,
            "referenced blob missing from disk (dangling reference) → 410 gone"
        );
        Ok(ServeResolution::Gone)
    }

    /// Whether the asset the reference names is currently servable — the moderation
    /// `served` flag [`service::moderation::Takedown`] writes.
    ///
    /// A feed reference with **no** `assets` row is servable: the key-free serve path is
    /// indexed by the committed feed, and a takedown is recorded *on* an asset row (it cannot
    /// exist without one). Absence is therefore "never taken down", not "unknown → refuse".
    #[instrument(skip(conn), fields(asset = %asset_id))]
    async fn asset_served<C: ConnectionTrait>(conn: &C, asset_id: &str) -> Result<bool, DbErr> {
        let served: Option<bool> = asset::Entity::find_by_id(asset_id)
            .select_only()
            .column(asset::Column::Served)
            .into_tuple()
            .one(conn)
            .await?;
        Ok(served.unwrap_or(true))
    }
}
