//! Peering's durable local state: the anti-rollback floor on the account's device directory and
//! the tombstones a peer may never resurrect ([Peering § Trust Model], [Peering § Sync]).
//!
//! Both are security state, so the store itself refuses to weaken them, whatever writes to it:
//! the floor's version never decreases and its row is never deleted, and a tombstone is never
//! updated or deleted. Each refusal aborts the statement that attempted it.
//!
//! [Peering § Trust Model]: https://docs/design/peering/#trust-model
//! [Peering § Sync]: https://docs/design/peering/#sync

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

/// The tables and the triggers that keep them monotonic. SQLite only, as the CLI store is.
const UP: &str = r"
CREATE TABLE IF NOT EXISTS peering_floor (
    device_id TEXT NOT NULL PRIMARY KEY,
    directory_version BIGINT NOT NULL CHECK (directory_version >= 0)
);

CREATE TRIGGER IF NOT EXISTS peering_floor_never_lowers
BEFORE UPDATE ON peering_floor
WHEN NEW.directory_version < OLD.directory_version OR NEW.device_id <> OLD.device_id
BEGIN
    SELECT RAISE(ABORT, 'the peering directory floor never lowers');
END;

CREATE TRIGGER IF NOT EXISTS peering_floor_never_deleted
BEFORE DELETE ON peering_floor
BEGIN
    SELECT RAISE(ABORT, 'the peering directory floor is never deleted');
END;

CREATE TABLE IF NOT EXISTS peering_tombstones (
    asset_id TEXT NOT NULL PRIMARY KEY,
    recorded_at TEXT NOT NULL
);

CREATE TRIGGER IF NOT EXISTS peering_tombstones_never_updated
BEFORE UPDATE ON peering_tombstones
BEGIN
    SELECT RAISE(ABORT, 'a peering tombstone is never updated');
END;

CREATE TRIGGER IF NOT EXISTS peering_tombstones_never_deleted
BEFORE DELETE ON peering_tombstones
BEGIN
    SELECT RAISE(ABORT, 'a peering tombstone is never deleted');
END;
";

const DOWN: &str = r"
DROP TABLE IF EXISTS peering_tombstones;
DROP TABLE IF EXISTS peering_floor;
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
