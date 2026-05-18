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

use crate::error::CatalogError;
use crate::records::{AlbumRecord, AssetRecord, AssetStackRecord, StackMemberRecord};

/// A handle to a Capsule SQLite catalog database.
#[derive(uniffi::Object)]
pub struct Catalog {
    inner: Mutex<DatabaseDriver>,
}

impl Catalog {
    /// Lock the driver, recovering from a poisoned mutex.
    ///
    /// Poisoning only means an earlier call panicked while holding the lock;
    /// the SQLite connection itself remains valid, so recovery is safe.
    fn driver(&self) -> MutexGuard<'_, DatabaseDriver> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[uniffi::export]
impl Catalog {
    /// Open (creating and migrating if necessary) the catalog at `path`.
    #[uniffi::constructor]
    pub fn open(path: String) -> Result<Arc<Self>, CatalogError> {
        log::info!("catalog: opening at {path}");
        let driver = DatabaseDriver::open(Path::new(&path))?;
        Ok(Arc::new(Self {
            inner: Mutex::new(driver),
        }))
    }

    /// Open an ephemeral in-memory catalog (used by tests and SwiftUI previews).
    #[uniffi::constructor]
    pub fn open_in_memory() -> Result<Arc<Self>, CatalogError> {
        log::debug!("catalog: opening in-memory");
        let driver = DatabaseDriver::open_in_memory()?;
        Ok(Arc::new(Self {
            inner: Mutex::new(driver),
        }))
    }

    /// The `PRAGMA user_version` of the open database.
    pub fn schema_version(&self) -> Result<u32, CatalogError> {
        Ok(self.driver().schema_version()?)
    }

    // ── Assets ───────────────────────────────────────────────────────────────

    pub fn insert_asset(&self, asset: AssetRecord) -> Result<(), CatalogError> {
        log::debug!("catalog: insert_asset uuid={}", asset.uuid);
        self.driver().insert_asset(&asset.into())?;
        Ok(())
    }

    pub fn upsert_asset(&self, asset: AssetRecord) -> Result<(), CatalogError> {
        log::debug!("catalog: upsert_asset uuid={}", asset.uuid);
        self.driver().upsert_asset(&asset.into())?;
        Ok(())
    }

    pub fn find_by_uuid(&self, uuid: String) -> Result<Option<AssetRecord>, CatalogError> {
        log::trace!("catalog: find_by_uuid uuid={uuid}");
        Ok(self.driver().find_by_uuid(&uuid)?.map(AssetRecord::from))
    }

    pub fn find_by_hash(&self, hash: String) -> Result<Option<AssetRecord>, CatalogError> {
        log::trace!("catalog: find_by_hash");
        Ok(self.driver().find_by_hash(&hash)?.map(AssetRecord::from))
    }

    pub fn query_timeline(
        &self,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<AssetRecord>, CatalogError> {
        log::trace!("catalog: query_timeline offset={offset} limit={limit}");
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
        log::trace!(
            "catalog: query_timeline_filtered type={asset_type:?} after={after:?} before={before:?}"
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
        log::debug!("catalog: soft_delete uuid={uuid}");
        self.driver().soft_delete(&uuid, deleted_at)?;
        Ok(())
    }

    pub fn restore_asset(&self, uuid: String) -> Result<(), CatalogError> {
        log::debug!("catalog: restore_asset uuid={uuid}");
        self.driver().restore_asset(&uuid)?;
        Ok(())
    }

    pub fn query_expired_trash(
        &self,
        older_than_secs: i64,
    ) -> Result<Vec<AssetRecord>, CatalogError> {
        let rows = self.driver().query_expired_trash(older_than_secs)?;
        Ok(rows.into_iter().map(AssetRecord::from).collect())
    }

    // ── Stacks ───────────────────────────────────────────────────────────────

    pub fn insert_stack(&self, stack: AssetStackRecord) -> Result<(), CatalogError> {
        log::debug!("catalog: insert_stack id={}", stack.id);
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
        log::debug!("catalog: insert_album id={}", album.id);
        self.driver().insert_album(&album.into())?;
        Ok(())
    }

    pub fn update_album(&self, album: AlbumRecord) -> Result<(), CatalogError> {
        log::debug!("catalog: update_album id={}", album.id);
        self.driver().update_album(&album.into())?;
        Ok(())
    }

    pub fn delete_album(&self, id: String) -> Result<(), CatalogError> {
        log::debug!("catalog: delete_album id={id}");
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
        log::debug!("catalog: set_asset_album uuid={uuid} album={album_id:?}");
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
