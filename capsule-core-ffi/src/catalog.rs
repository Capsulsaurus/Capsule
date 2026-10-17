//! The [`Catalog`] UniFFI object — a thread-safe handle over the `capsule-core`
//! SQLite `DatabaseDriver`.
//!
//! `rusqlite::Connection` is `Send` but not `Sync`, so the driver is wrapped in
//! a `Mutex`; that makes the `Arc`-shared `Catalog` `Send + Sync` as UniFFI
//! requires. The Swift side further confines all calls to a dedicated actor, so
//! the mutex is effectively uncontended.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use capsule_core::db::DatabaseDriver;
use capsule_core::library::{GateError, GateKeeper};

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

// ── LQIP placeholder rendering (slice S-B14) ────────────────────────────────

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
/// a photo's placeholder does not depend on which client is painting it (slice `S-B14`).
///
/// **A free function rather than a [`Catalog`] method, deliberately.** The `assets` table has
/// `chromahash` and `dominant_color` columns, and they are NULL: the signed sidecar is the
/// placeholder's home, and projecting it onto the index would have to be done identically by
/// `capsule_core::library::rebuild` or a rebuilt index would disagree with a freshly written
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
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A platform gate that always succeeds, counting the challenges it saw.
    struct AllowGate {
        calls: AtomicU32,
    }
    impl AllowGate {
        fn new() -> Self {
            Self {
                calls: AtomicU32::new(0),
            }
        }
        fn calls(&self) -> u32 {
            self.calls.load(Ordering::Relaxed)
        }
    }
    impl LocalAuthGate for AllowGate {
        fn authenticate(&self, _view: GatedView) -> Result<(), LocalAuthError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    /// A platform gate that always refuses with a fixed error.
    struct DenyGate(LocalAuthError);
    impl LocalAuthGate for DenyGate {
        fn authenticate(&self, _view: GatedView) -> Result<(), LocalAuthError> {
            Err(self.0.clone())
        }
    }

    /// Unlock a view with a gate that always allows, for tests about something else.
    fn unlock(cat: &Catalog, view: GatedView) {
        cat.unlock_view(view, Arc::new(AllowGate::new()))
            .expect("AllowGate never refuses");
    }

    fn asset(uuid: &str, hash: &str) -> AssetRecord {
        AssetRecord {
            uuid: uuid.to_string(),
            asset_type: "photo".to_string(),
            capture_timestamp: 1_720_000_000,
            capture_utc: Some(1_719_997_200),
            capture_tz_source: Some("offset_exif".to_string()),
            import_timestamp: 1_720_000_000,
            hash_sha256: hash.to_string(),
            width: Some(4032),
            height: Some(3024),
            duration_ms: None,
            stack_id: None,
            is_stack_hidden: false,
            chromahash: None,
            dominant_color: None,
            album_id: None,
            rating: 0,
            is_deleted: false,
            deleted_at: None,
            is_hidden: false,
        }
    }

    #[test]
    fn test_open_in_memory_and_insert() {
        let cat = Catalog::open_in_memory().unwrap();
        assert!(cat.schema_version().unwrap() >= 2);

        cat.insert_asset(asset("u1", &"a".repeat(64))).unwrap();
        let found = cat.find_by_hash("a".repeat(64)).unwrap();
        assert_eq!(found.unwrap().uuid, "u1");

        let timeline = cat.query_timeline(0, 100).unwrap();
        assert_eq!(timeline.len(), 1);
    }

    #[test]
    fn test_soft_delete_hides_from_timeline() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.insert_asset(asset("u1", &"a".repeat(64))).unwrap();
        cat.soft_delete("u1".to_string(), 1_720_000_100).unwrap();
        assert!(cat.query_timeline(0, 100).unwrap().is_empty());
        cat.restore_asset("u1".to_string()).unwrap();
        assert_eq!(cat.query_timeline(0, 100).unwrap().len(), 1);
    }

    #[test]
    fn test_query_trash_and_purge() {
        let cat = Catalog::open_in_memory().unwrap();
        unlock(&cat, GatedView::RecentlyDeleted);
        cat.insert_asset(asset("u1", &"a".repeat(64))).unwrap();
        cat.insert_asset(asset("u2", &"b".repeat(64))).unwrap();
        assert!(cat.query_trash(0, 100).unwrap().is_empty());

        cat.soft_delete("u1".to_string(), 1_720_000_100).unwrap();
        let trash = cat.query_trash(0, 100).unwrap();
        assert_eq!(trash.len(), 1);
        assert_eq!(trash[0].uuid, "u1");
        // Still listed in trash but gone from the timeline.
        assert_eq!(cat.query_timeline(0, 100).unwrap().len(), 1);

        cat.purge_asset("u1".to_string()).unwrap();
        assert!(cat.query_trash(0, 100).unwrap().is_empty());
        assert!(cat.find_by_uuid("u1".to_string()).unwrap().is_none());
    }

    #[test]
    fn test_album_membership() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.insert_album(AlbumRecord {
            id: "alb-1".to_string(),
            name: "Trip".to_string(),
            created_at: 1_720_000_000,
            modified_at: 1_720_000_000,
            cover_asset_id: None,
        })
        .unwrap();
        cat.insert_asset(asset("u1", &"a".repeat(64))).unwrap();
        cat.set_asset_album("u1".to_string(), Some("alb-1".to_string()))
            .unwrap();

        assert_eq!(
            cat.query_album_assets("alb-1".to_string(), 0, 100)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(cat.list_albums().unwrap().len(), 1);

        cat.delete_album("alb-1".to_string()).unwrap();
        assert!(cat.find_album("alb-1".to_string()).unwrap().is_none());
        // The asset itself survives album deletion.
        assert!(cat.find_by_uuid("u1".to_string()).unwrap().is_some());
    }

    #[test]
    fn test_query_timeline_filtered_by_type() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.insert_asset(asset("p1", &"a".repeat(64))).unwrap();
        let mut video = asset("v1", &"b".repeat(64));
        video.asset_type = "video".to_string();
        cat.insert_asset(video).unwrap();

        let videos = cat
            .query_timeline_filtered(Some("video".to_string()), None, None, 0, 100)
            .unwrap();
        assert_eq!(videos.len(), 1);
        assert_eq!(videos[0].uuid, "v1");
    }

    // ── SR1 gated views (slice S-D22) ────────────────────────────────────────
    //
    // The FFI mirror of `capsule-core`'s
    // `gated_hidden_query_refuses_without_grant_and_serves_with_one`: the surface the
    // native apps actually call must enforce the same gate the Rust surface does.

    /// The acceptance case: the FFI trash listing refuses without a grant, serves with
    /// one, and a grant for the *other* gated view is not a substitute.
    #[test]
    fn trash_listing_refuses_without_grant_and_serves_with_one() {
        let cat = Catalog::open_in_memory().unwrap();
        // One soft-deleted asset, plus a live one that must never appear in the trash.
        cat.insert_asset(asset("u1", &"a".repeat(64))).unwrap();
        cat.insert_asset(asset("u2", &"b".repeat(64))).unwrap();
        cat.soft_delete("u1".to_string(), 1_720_000_100).unwrap();

        // Refuses before a grant.
        assert!(!cat.is_view_unlocked(GatedView::RecentlyDeleted));
        assert!(matches!(
            cat.query_trash(0, 100),
            Err(CatalogError::ViewLocked)
        ));

        // Serves the soft-deleted asset once a grant exists.
        unlock(&cat, GatedView::RecentlyDeleted);
        assert!(cat.is_view_unlocked(GatedView::RecentlyDeleted));
        let trash = cat.query_trash(0, 100).unwrap();
        assert_eq!(trash.len(), 1);
        assert_eq!(trash[0].uuid, "u1");
        assert!(trash[0].is_deleted);

        // A Hidden grant does NOT unlock the trash — grants are per-view.
        cat.relock_view(GatedView::RecentlyDeleted);
        unlock(&cat, GatedView::Hidden);
        assert!(cat.is_view_unlocked(GatedView::Hidden));
        assert!(matches!(
            cat.query_trash(0, 100),
            Err(CatalogError::ViewLocked)
        ));
    }

    /// A platform refusal is passed through unchanged, mints no grant, and leaves the
    /// listing locked — the FFI mirror of
    /// `open_propagates_platform_denial_and_stays_locked`.
    #[test]
    fn platform_denial_is_passed_through_and_leaves_the_view_locked() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.insert_asset(asset("u1", &"a".repeat(64))).unwrap();
        cat.soft_delete("u1".to_string(), 1_720_000_100).unwrap();

        for refusal in [
            LocalAuthError::Cancelled,
            LocalAuthError::Unavailable,
            LocalAuthError::Failed,
        ] {
            let err = cat
                .unlock_view(
                    GatedView::RecentlyDeleted,
                    Arc::new(DenyGate(refusal.clone())),
                )
                .unwrap_err();
            assert_eq!(err, refusal);
            assert!(!cat.is_view_unlocked(GatedView::RecentlyDeleted));
            assert!(matches!(
                cat.query_trash(0, 100),
                Err(CatalogError::ViewLocked)
            ));
        }
    }

    /// Re-unlocking inside the grace window reuses the live grant instead of challenging
    /// the platform again; `lock_views` drops every grant and re-locks the listing.
    #[test]
    fn grant_is_reused_within_grace_and_dropped_by_lock_views() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.insert_asset(asset("u1", &"a".repeat(64))).unwrap();
        cat.soft_delete("u1".to_string(), 1_720_000_100).unwrap();

        let gate = Arc::new(AllowGate::new());
        cat.unlock_view(GatedView::RecentlyDeleted, gate.clone())
            .unwrap();
        cat.unlock_view(GatedView::RecentlyDeleted, gate.clone())
            .unwrap();
        assert_eq!(gate.calls(), 1, "re-unlock within grace does not re-auth");
        assert_eq!(cat.query_trash(0, 100).unwrap().len(), 1);

        // Backgrounding the app drops every grant.
        unlock(&cat, GatedView::Hidden);
        cat.lock_views();
        assert!(!cat.is_view_unlocked(GatedView::RecentlyDeleted));
        assert!(!cat.is_view_unlocked(GatedView::Hidden));
        assert!(matches!(
            cat.query_trash(0, 100),
            Err(CatalogError::ViewLocked)
        ));

        // A fresh unlock re-authenticates and serves again.
        cat.unlock_view(GatedView::RecentlyDeleted, gate.clone())
            .unwrap();
        assert_eq!(gate.calls(), 2, "unlock after lock_views re-auths");
        assert_eq!(cat.query_trash(0, 100).unwrap().len(), 1);
    }

    /// The audit's other half: the *default* projections stay ungated and must therefore
    /// never leak what the gates protect. Deleted and hidden assets are excluded from the
    /// timeline, the filtered timeline, and album listings with no grant in sight.
    #[test]
    fn default_projections_are_ungated_and_leak_neither_deleted_nor_hidden() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.insert_album(AlbumRecord {
            id: "alb-1".to_string(),
            name: "Trip".to_string(),
            created_at: 1_720_000_000,
            modified_at: 1_720_000_000,
            cover_asset_id: None,
        })
        .unwrap();

        cat.insert_asset(asset("visible", &"a".repeat(64))).unwrap();
        cat.insert_asset(asset("deleted", &"b".repeat(64))).unwrap();
        let mut hidden = asset("hidden", &"c".repeat(64));
        hidden.is_hidden = true;
        cat.insert_asset(hidden).unwrap();
        for uuid in ["visible", "deleted", "hidden"] {
            cat.set_asset_album(uuid.to_string(), Some("alb-1".to_string()))
                .unwrap();
        }
        cat.soft_delete("deleted".to_string(), 1_720_000_100)
            .unwrap();

        // No grant anywhere.
        assert!(!cat.is_view_unlocked(GatedView::RecentlyDeleted));
        assert!(!cat.is_view_unlocked(GatedView::Hidden));

        let only_visible = |rows: Vec<AssetRecord>| {
            assert_eq!(rows.len(), 1, "expected only the visible asset");
            assert_eq!(rows[0].uuid, "visible");
        };
        only_visible(cat.query_timeline(0, 100).unwrap());
        only_visible(
            cat.query_timeline_filtered(Some("photo".to_string()), None, None, 0, 100)
                .unwrap(),
        );
        only_visible(cat.query_album_assets("alb-1".to_string(), 0, 100).unwrap());

        // Point lookups are addressed by an identifier the caller already holds, not
        // listings, so they stay ungated by design — asserted so the choice is explicit.
        assert!(cat.find_by_uuid("deleted".to_string()).unwrap().is_some());
        assert!(cat.find_by_uuid("hidden".to_string()).unwrap().is_some());
    }

    // ── LQIP placeholder rendering ──────────────────────────────────────────

    /// The native surface decodes the same bytes the import pipeline encoded, to the same
    /// pixels `capsule_core::lqip` produces — the `S-B14` cross-surface criterion at the FFI
    /// boundary.
    #[test]
    fn render_lqip_matches_the_core_decoder() {
        use capsule_core::lqip::{Gamut, LQIP_FORMAT_V1, Lqip};

        let (w, h) = (120u32, 90u32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&[(x * 2) as u8, (y * 2) as u8, 128, 255]);
            }
        }
        let lqip = Lqip::encode(w, h, &rgba, Gamut::Srgb).expect("encode");
        assert_eq!(lqip.as_bytes().len(), 32, "the committed tier is 32 bytes");

        let expected = lqip.decode_capped(48, 48);
        let got = render_lqip(
            LQIP_FORMAT_V1,
            lqip.as_bytes().to_vec(),
            lqip.dominant_color().to_vec(),
            48,
            48,
        );
        assert_eq!((got.width, got.height), (expected.width, expected.height));
        assert_eq!(got.rgba, expected.rgba, "byte-identical to the core");
        assert_eq!(got.rgba.len() as u32, got.width * got.height * 4);
    }

    /// Every malformed input paints the fallback rather than failing: an unknown version, a
    /// corrupt payload, and a `dominant_color` that is not three bytes.
    #[test]
    fn render_lqip_never_fails_on_a_malformed_record() {
        use capsule_core::lqip::LQIP_FORMAT_V1;

        let fill = vec![9u8, 8, 7];
        let unknown = render_lqip(LQIP_FORMAT_V1 + 42, vec![1, 2, 3], fill.clone(), 32, 32);
        assert_eq!((unknown.width, unknown.height), (1, 1));
        assert_eq!(unknown.rgba, vec![9, 8, 7, 255]);

        let corrupt = render_lqip(LQIP_FORMAT_V1, vec![0xDE, 0xAD], fill, 32, 32);
        assert_eq!(corrupt.rgba, vec![9, 8, 7, 255]);

        // No usable fill: black, the conventional empty-cell colour.
        let no_fill = render_lqip(LQIP_FORMAT_V1, vec![0xDE, 0xAD], vec![1, 2], 32, 32);
        assert_eq!((no_fill.width, no_fill.height), (1, 1));
        assert_eq!(no_fill.rgba, vec![0, 0, 0, 255]);
    }

    /// The retention sweep is deliberately ungated (it runs unattended) — pinned here so
    /// a future change to that decision is a deliberate one, not an accident.
    #[test]
    fn retention_sweep_stays_ungated() {
        let cat = Catalog::open_in_memory().unwrap();
        cat.insert_asset(asset("u1", &"a".repeat(64))).unwrap();
        cat.soft_delete("u1".to_string(), 1_720_000_100).unwrap();

        assert!(!cat.is_view_unlocked(GatedView::RecentlyDeleted));
        // Deleted long before the cutoff, so the sweep sees it without any grant.
        assert_eq!(cat.query_expired_trash(30 * 86_400).unwrap().len(), 1);
    }
}
