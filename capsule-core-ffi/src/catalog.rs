//! The [`Catalog`] UniFFI object — a thread-safe handle over the `capsule-core`
//! SQLite `DatabaseDriver`.
//!
//! `rusqlite::Connection` is `Send` but not `Sync`, so the driver is wrapped in
//! a `Mutex`; that makes the `Arc`-shared `Catalog` `Send + Sync` as UniFFI
//! requires. The Swift side further confines all calls to a dedicated actor, so
//! the mutex is effectively uncontended.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use capsule_library::db::DatabaseDriver;
use capsule_library::library::{GateError, GateKeeper};

use crate::error::CatalogError;
use crate::gate::{ForeignAuthGate, GatedView, LocalAuthError, LocalAuthGate};
use crate::records::{AlbumRecord, AssetRecord, AssetStackRecord, StackMemberRecord};

/// A handle to a Capsule SQLite catalog database.
///
/// Carries the session's SR1 fresh-local-auth grants alongside the connection: the gated
/// views ([Local Gallery — SR1]) are read through [`GateKeeper`], which refuses without a
/// live grant. Grants are per-`Catalog`, so they die with the handle.
///
/// [Local Gallery — SR1]: https://docs/design/local-gallery/#security-requirements
#[derive(uniffi::Object)]
pub struct Catalog {
    inner: Mutex<DatabaseDriver>,
    /// SR1 grant state. A second mutex rather than one over a pair so an ungated read never
    /// contends with a gate operation. Only [`Catalog::query_trash`] holds both, and it
    /// always takes `gates` **before** `inner`; nothing else takes them in any order, so no
    /// cycle exists.
    gates: Mutex<GateKeeper>,
}

impl Catalog {
    /// Lock the driver, recovering from a poisoned mutex.
    ///
    /// Poisoning only means an earlier call panicked while holding the lock;
    /// the SQLite connection itself remains valid, so recovery is safe.
    fn driver(&self) -> MutexGuard<'_, DatabaseDriver> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Lock the gate keeper, recovering from a poisoned mutex. Recovery cannot forge a
    /// grant: the recovered state is whatever was last committed, and a grant is only ever
    /// inserted by a successful [`Catalog::unlock_view`].
    fn gates(&self) -> MutexGuard<'_, GateKeeper> {
        self.gates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[uniffi::export]
impl Catalog {
    /// Open (creating and migrating if necessary) the catalog at `path`.
    #[uniffi::constructor]
    pub fn open(path: String) -> Result<Arc<Self>, CatalogError> {
        tracing::info!(%path, "catalog: opening");
        let driver = DatabaseDriver::open(Path::new(&path))?;
        Ok(Arc::new(Self {
            inner: Mutex::new(driver),
            gates: Mutex::new(GateKeeper::new()),
        }))
    }

    /// Open an ephemeral in-memory catalog (used by tests and SwiftUI previews).
    #[uniffi::constructor]
    pub fn open_in_memory() -> Result<Arc<Self>, CatalogError> {
        tracing::debug!("catalog: opening in-memory");
        let driver = DatabaseDriver::open_in_memory()?;
        Ok(Arc::new(Self {
            inner: Mutex::new(driver),
            gates: Mutex::new(GateKeeper::new()),
        }))
    }

    /// The `PRAGMA user_version` of the open database.
    pub fn schema_version(&self) -> Result<u32, CatalogError> {
        Ok(self.driver().schema_version()?)
    }

    // ── Assets ───────────────────────────────────────────────────────────────

    pub fn insert_asset(&self, asset: AssetRecord) -> Result<(), CatalogError> {
        tracing::debug!(uuid = %asset.uuid, "catalog: insert_asset");
        self.driver().insert_asset(&asset.into())?;
        Ok(())
    }

    pub fn upsert_asset(&self, asset: AssetRecord) -> Result<(), CatalogError> {
        tracing::debug!(uuid = %asset.uuid, "catalog: upsert_asset");
        self.driver().upsert_asset(&asset.into())?;
        Ok(())
    }

    pub fn find_by_uuid(&self, uuid: String) -> Result<Option<AssetRecord>, CatalogError> {
        tracing::trace!(%uuid, "catalog: find_by_uuid");
        Ok(self.driver().find_by_uuid(&uuid)?.map(AssetRecord::from))
    }

    pub fn find_by_hash(&self, hash: String) -> Result<Option<AssetRecord>, CatalogError> {
        tracing::trace!("catalog: find_by_hash");
        Ok(self.driver().find_by_hash(&hash)?.map(AssetRecord::from))
    }

    pub fn query_timeline(
        &self,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<AssetRecord>, CatalogError> {
        tracing::trace!(offset, limit, "catalog: query_timeline");
        let rows = self
            .driver()
            .query_timeline(offset as usize, limit as usize)?;
        Ok(rows.into_iter().map(AssetRecord::from).collect())
    }

    /// Query the timeline filtered by asset type and/or capture-time window.
    /// Any filter left as `None` is not applied.
    pub fn query_timeline_filtered(
        &self,
        asset_type: Option<String>,
        after: Option<i64>,
        before: Option<i64>,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<AssetRecord>, CatalogError> {
        tracing::trace!(
            asset_type = ?asset_type,
            after = ?after,
            before = ?before,
            "catalog: query_timeline_filtered"
        );
        let rows = self.driver().query_timeline_filtered(
            asset_type.as_deref(),
            after,
            before,
            offset as usize,
            limit as usize,
        )?;
        Ok(rows.into_iter().map(AssetRecord::from).collect())
    }

    /// Set (or, with `None` or an empty string, clear) an asset's caption — the `caption`
    /// column the index projects from the sidecar's LWW register, read back as
    /// [`AssetRecord::caption`].
    ///
    /// An unknown `uuid` is [`CatalogError::InvalidArgument`] and nothing is written, so a
    /// caption is never reported saved against a row that does not exist.
    pub fn set_caption(&self, uuid: String, caption: Option<String>) -> Result<(), CatalogError> {
        tracing::debug!(%uuid, cleared = caption.as_deref().is_none_or(str::is_empty), "catalog: set_caption");
        if self
            .driver()
            .update_asset_caption(&uuid, caption.as_deref())?
        {
            Ok(())
        } else {
            tracing::debug!(%uuid, "catalog: set_caption refused, no such asset");
            Err(CatalogError::InvalidArgument {
                message: format!("no asset: {uuid}"),
            })
        }
    }

    pub fn soft_delete(&self, uuid: String, deleted_at: i64) -> Result<(), CatalogError> {
        tracing::debug!(%uuid, "catalog: soft_delete");
        self.driver().soft_delete(&uuid, deleted_at)?;
        Ok(())
    }

    pub fn restore_asset(&self, uuid: String) -> Result<(), CatalogError> {
        tracing::debug!(%uuid, "catalog: restore_asset");
        self.driver().restore_asset(&uuid)?;
        Ok(())
    }

    /// The **retention sweep** input: assets soft-deleted longer than `older_than_secs`
    /// ago, for the background purge job ([Filesystem — Maintenance]).
    ///
    /// Deliberately **ungated**: the sweep runs unattended, with no user present to
    /// authenticate, so putting it behind SR1 would only break maintenance. It is not the
    /// Recently Deleted *view* — do not build one on it; use
    /// [`query_trash`](Self::query_trash), which is gated.
    ///
    /// [Filesystem — Maintenance]: https://docs/design/filesystem/maintenance/
    pub fn query_expired_trash(
        &self,
        older_than_secs: i64,
    ) -> Result<Vec<AssetRecord>, CatalogError> {
        let rows = self.driver().query_expired_trash(older_than_secs)?;
        Ok(rows.into_iter().map(AssetRecord::from).collect())
    }

    /// All currently soft-deleted assets — the Recently Deleted listing, **gated** by SR1.
    ///
    /// Returns [`CatalogError::ViewLocked`] unless a live [`GatedView::RecentlyDeleted`]
    /// grant exists; call [`unlock_view`](Self::unlock_view) first. Ordering and paging are
    /// the driver's; the gate decision is [`GateKeeper`]'s.
    pub fn query_trash(&self, offset: u64, limit: u64) -> Result<Vec<AssetRecord>, CatalogError> {
        tracing::trace!(offset, limit, "catalog: query_trash");
        let gates = self.gates();
        let rows = gates.query_recently_deleted(&self.driver(), offset as usize, limit as usize)?;
        Ok(rows.into_iter().map(AssetRecord::from).collect())
    }

    // ── SR1 gated-view grants ────────────────────────────────────────────────

    /// Challenge the platform through `auth` and mint a fresh-auth grant for `view`.
    ///
    /// A grant already live within its grace window is reused — no second challenge — and
    /// is **not** slid forward, so re-tapping a view cannot hold it open indefinitely
    /// (the window is measured from the original mint). Errors are the platform's, passed
    /// through unchanged; a refusal mints nothing and leaves the view locked.
    pub fn unlock_view(
        &self,
        view: GatedView,
        auth: Arc<dyn LocalAuthGate>,
    ) -> Result<(), LocalAuthError> {
        tracing::debug!(?view, "catalog: unlock_view");
        match self.gates().open(view.into(), &ForeignAuthGate(auth)) {
            Ok(()) => Ok(()),
            Err(GateError::Auth(e)) => Err(e.into()),
            // `open` mints the grant, so it never reports one missing. Mapped rather than
            // asserted so the arm stays total if the core contract ever widens.
            Err(GateError::Locked) => Err(LocalAuthError::Failed),
        }
    }

    /// Whether `view` currently holds a live grant. Pure state check — challenges nothing.
    pub fn is_view_unlocked(&self, view: GatedView) -> bool {
        self.gates().is_open(view.into())
    }

    /// Revoke the grant for one view (e.g. the user left that surface).
    pub fn relock_view(&self, view: GatedView) {
        tracing::debug!(?view, "catalog: relock_view");
        self.gates().revoke(view.into());
    }

    /// Revoke every grant (e.g. the app was backgrounded or the device locked). The next
    /// [`unlock_view`](Self::unlock_view) re-authenticates.
    pub fn lock_views(&self) {
        tracing::debug!("catalog: lock_views");
        self.gates().lock();
    }

    /// Permanently remove an asset row (the file is deleted by the caller).
    pub fn purge_asset(&self, uuid: String) -> Result<(), CatalogError> {
        tracing::debug!(%uuid, "catalog: purge_asset");
        self.driver().purge_asset(&uuid)?;
        Ok(())
    }

    // ── Stacks ───────────────────────────────────────────────────────────────

    pub fn insert_stack(&self, stack: AssetStackRecord) -> Result<(), CatalogError> {
        tracing::debug!(id = %stack.id, "catalog: insert_stack");
        self.driver().insert_stack(&stack.into())?;
        Ok(())
    }

    pub fn insert_stack_member(&self, member: StackMemberRecord) -> Result<(), CatalogError> {
        self.driver().insert_stack_member(&member.into())?;
        Ok(())
    }

    pub fn update_stack_hidden(&self, uuid: String, hidden: bool) -> Result<(), CatalogError> {
        self.driver().update_stack_hidden(&uuid, hidden)?;
        Ok(())
    }

    pub fn update_stack_primary(
        &self,
        stack_id: String,
        primary_uuid: String,
    ) -> Result<(), CatalogError> {
        self.driver()
            .update_stack_primary(&stack_id, &primary_uuid)?;
        Ok(())
    }

    pub fn list_stack_members(
        &self,
        stack_id: String,
    ) -> Result<Vec<StackMemberRecord>, CatalogError> {
        let rows = self.driver().list_stack_members(&stack_id)?;
        Ok(rows.into_iter().map(StackMemberRecord::from).collect())
    }

    // ── Albums ───────────────────────────────────────────────────────────────

    pub fn insert_album(&self, album: AlbumRecord) -> Result<(), CatalogError> {
        tracing::debug!(id = %album.id, "catalog: insert_album");
        self.driver().insert_album(&album.into())?;
        Ok(())
    }

    pub fn update_album(&self, album: AlbumRecord) -> Result<(), CatalogError> {
        tracing::debug!(id = %album.id, "catalog: update_album");
        self.driver().update_album(&album.into())?;
        Ok(())
    }

    pub fn delete_album(&self, id: String) -> Result<(), CatalogError> {
        tracing::debug!(%id, "catalog: delete_album");
        self.driver().delete_album(&id)?;
        Ok(())
    }

    pub fn find_album(&self, id: String) -> Result<Option<AlbumRecord>, CatalogError> {
        Ok(self.driver().find_album(&id)?.map(AlbumRecord::from))
    }

    pub fn list_albums(&self) -> Result<Vec<AlbumRecord>, CatalogError> {
        let rows = self.driver().list_albums()?;
        Ok(rows.into_iter().map(AlbumRecord::from).collect())
    }

    pub fn set_asset_album(
        &self,
        uuid: String,
        album_id: Option<String>,
    ) -> Result<(), CatalogError> {
        tracing::debug!(%uuid, album = ?album_id, "catalog: set_asset_album");
        self.driver().set_asset_album(&uuid, album_id.as_deref())?;
        Ok(())
    }

    pub fn query_album_assets(
        &self,
        album_id: String,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<AssetRecord>, CatalogError> {
        let rows = self
            .driver()
            .query_album_assets(&album_id, offset as usize, limit as usize)?;
        Ok(rows.into_iter().map(AssetRecord::from).collect())
    }
}

// ── LQIP placeholder rendering ────────────────────────────────

/// A decoded LQIP placeholder handed to the native clients: packed RGBA8, ready for a
/// `CGImage` / `Bitmap`.
///
/// A record rather than raw bytes because the caller cannot know the dimensions in advance:
/// `decode_capped` returns the largest frame that fits *inside* the requested box while
/// preserving the source aspect ratio.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LqipPlaceholder {
    /// Frame width in pixels — at most the requested `max_width`.
    pub width: u32,
    /// Frame height in pixels — at most the requested `max_height`.
    pub height: u32,
    /// Packed RGBA8 samples, `width * height * 4` bytes long.
    pub rgba: Vec<u8>,
}

/// Render the three fields of a sidecar `lqip` record to a paintable placeholder, band-limited
/// to the box being painted.
///
/// The mirror of `capsule-wasm`'s `decodeLqip`, over the same
/// [`capsule_core::lqip::render`] the import pipeline encodes against — one implementation, so
/// a photo's placeholder does not depend on which client is painting it.
///
/// **A free function rather than a [`Catalog`] method, deliberately.** The `assets` table has
/// `chromahash` and `dominant_color` columns, and they are NULL: the signed sidecar is the
/// placeholder's home, and projecting it onto the index would have to be done identically by
/// `capsule_library::library::rebuild` or a rebuilt index would disagree with a freshly written
/// one. Until both sides move together, an accessor keyed on an asset id could only ever
/// return nothing — so this takes the record the caller already holds from the decrypted
/// sidecar instead of pretending the index has it.
///
/// **Infallible.** An unrecognised `format_version`, a payload the parser rejects, or a
/// `dominant_color` that is not three bytes all yield the 1x1 solid fill: a reader must never
/// misrender a placeholder, and a gallery must never fail to draw a cell over one.
#[uniffi::export]
#[must_use]
pub fn render_lqip(
    format_version: u16,
    chromahash: Vec<u8>,
    dominant_color: Vec<u8>,
    max_width: u32,
    max_height: u32,
) -> LqipPlaceholder {
    // A malformed fill is the one input `capsule_core::lqip::render` cannot take, and unlike the
    // wasm boundary there is nothing useful to throw across the FFI for it: black is the
    // conventional empty-cell fill and is what a caller would paint anyway.
    let fill: [u8; 3] = dominant_color.try_into().unwrap_or([0, 0, 0]);
    let image =
        capsule_core::lqip::render(format_version, &chromahash, fill, max_width, max_height);
    LqipPlaceholder {
        width: image.width,
        height: image.height,
        rgba: image.rgba,
    }
}

#[cfg(test)]
mod tests;
