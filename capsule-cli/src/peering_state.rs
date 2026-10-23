//! Peering's durable local state in the CLI database: the anti-rollback floor on the account's
//! device directory and the tombstones a peer may never resurrect ([Peering § Trust Model],
//! [Peering § Sync]).
//!
//! Both are security state and fail closed:
//!
//! - **The floor** ([`DbDirectoryVersions`]) is the highest `directory_version` a device has
//!   verified. It is raised by one SQLite upsert taking the maximum of the stored and the new
//!   value, so a crash leaves the old floor or the new one, and the store's own triggers refuse
//!   to lower or delete it. A read error or a value out of range is an error, never a floor of
//!   zero: only a device that has never recorded a floor starts at zero.
//! - **Tombstones** ([`load_tombstones`]) are every asset the sync feed delivered as deleted,
//!   kept past purge in a table the store refuses to update or delete from. The set is what
//!   [`capsule_sdk::peering::ingest`] takes as [`LocalLibrary::tombstones`]; a row that does not
//!   decode fails the whole load rather than leaving a gap a peer could resurrect through.
//!
//! [`LocalLibrary::tombstones`]: capsule_sdk::peering::LocalLibrary::tombstones
//! [Peering § Trust Model]: https://docs/design/peering/#trust-model
//! [Peering § Sync]: https://docs/design/peering/#sync

use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use capsule_sdk::peering::{DirectoryVersions, PeeringError};
use entity::{peering_floor, peering_tombstone, synced_asset};
use jiff::Timestamp;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, DbErr, EntityTrait, QueryFilter,
    Statement,
};
use thiserror::Error;
use tracing::instrument;
use uuid::Uuid;

use crate::syncstore::{SyncStoreError, decode_id};

/// What the peering store can fail with. Every variant refuses: none is read as an empty floor
/// or an empty tombstone set.
#[derive(Debug, Error)]
pub enum PeeringStateError {
    /// A database operation failed.
    #[error("peering state database error: {0}")]
    Db(#[from] DbErr),
    /// A stored value does not decode.
    #[error("peering state is corrupt: {0}")]
    Corrupt(String),
    /// A directory version beyond what the store holds (`i64::MAX`).
    #[error("directory version {0} is out of range")]
    OutOfRange(u64),
    /// The floor a previous build kept in a file could not be read.
    #[error("the legacy directory floor at {path}: {error}")]
    Legacy {
        /// The file.
        path: String,
        /// Why it could not be read.
        error: String,
    },
}

impl From<SyncStoreError> for PeeringStateError {
    fn from(error: SyncStoreError) -> Self {
        match error {
            SyncStoreError::Db(error) => Self::Db(error),
            corrupt @ SyncStoreError::CorruptId { .. } => Self::Corrupt(corrupt.to_string()),
        }
    }
}

impl From<PeeringStateError> for PeeringError {
    fn from(error: PeeringStateError) -> Self {
        match error {
            PeeringStateError::Corrupt(_) | PeeringStateError::OutOfRange(_) => {
                Self::Codec(error.to_string())
            }
            PeeringStateError::Db(_) | PeeringStateError::Legacy { .. } => {
                Self::Io(error.to_string())
            }
        }
    }
}

// ─── The anti-rollback floor ─────────────────────────────────────────────────

/// The floor stored for `device`; `0` when it has never recorded one.
///
/// # Errors
///
/// [`PeeringStateError::Db`] when the store cannot be read, [`PeeringStateError::Corrupt`] when
/// the stored value is negative.
#[instrument(level = "debug", skip(db), err)]
pub async fn load_floor<C: ConnectionTrait>(
    db: &C,
    device: Uuid,
) -> Result<u64, PeeringStateError> {
    let floor = peering_floor::Entity::find_by_id(device.hyphenated().to_string())
        .one(db)
        .await?
        .map_or(Ok(0), |row| {
            u64::try_from(row.directory_version).map_err(|_| {
                PeeringStateError::Corrupt(format!(
                    "the directory floor of device {device} is {}",
                    row.directory_version
                ))
            })
        })?;
    tracing::debug!(%device, floor, "peering: the directory floor is loaded");
    Ok(floor)
}

/// Raise the floor stored for `device` to at least `version`, in one statement, and return the
/// floor now stored. A lower `version` leaves it where it is.
///
/// # Errors
///
/// [`PeeringStateError::OutOfRange`] for a version the store cannot hold, or the errors of
/// [`load_floor`].
#[instrument(level = "debug", skip(db), err)]
pub async fn raise_floor<C: ConnectionTrait>(
    db: &C,
    device: Uuid,
    version: u64,
) -> Result<u64, PeeringStateError> {
    let stored = i64::try_from(version).map_err(|_| PeeringStateError::OutOfRange(version))?;
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO peering_floor (device_id, directory_version) VALUES (?, ?) \
         ON CONFLICT (device_id) DO UPDATE \
         SET directory_version = MAX(directory_version, excluded.directory_version)",
        [device.hyphenated().to_string().into(), stored.into()],
    ))
    .await?;
    let floor = load_floor(db, device).await?;
    tracing::info!(%device, version, floor, "peering: the directory floor is raised");
    Ok(floor)
}

/// A [`DirectoryVersions`] over the CLI database, for one device.
///
/// [`DirectoryVersions`] is synchronous and the database is not, so the floor is admitted
/// through [`admit`](Self::admit), which persists before it returns, and the trait serves the
/// floor persisted so far. [`record`](DirectoryVersions::record) accepts only a version at or
/// below it: a raise that has not been persisted is refused, so this adapter never reports a
/// floor a restart would forget.
#[derive(Debug)]
pub struct DbDirectoryVersions {
    db: DatabaseConnection,
    device: Uuid,
    persisted: AtomicU64,
}

impl DbDirectoryVersions {
    /// The floor `device` has persisted in `db`.
    ///
    /// # Errors
    ///
    /// The errors of [`load_floor`]: an unreadable store is refused, never read as zero.
    pub async fn open(db: DatabaseConnection, device: Uuid) -> Result<Self, PeeringStateError> {
        let floor = load_floor(&db, device).await?;
        Ok(Self {
            db,
            device,
            persisted: AtomicU64::new(floor),
        })
    }

    /// Adopt the floor a previous build kept in the file at `path`, then remove the file. The
    /// floor is raised before the file goes, so a crash between the two re-adopts it next time,
    /// which changes nothing. No file is nothing to adopt.
    ///
    /// # Errors
    ///
    /// [`PeeringStateError::Legacy`] when the file exists but cannot be read,
    /// [`PeeringStateError::Corrupt`] when it does not parse, or the errors of [`raise_floor`].
    #[instrument(level = "debug", skip_all, fields(device = %self.device, path = %path.display()), err)]
    pub async fn adopt_legacy_file(&self, path: &Path) -> Result<(), PeeringStateError> {
        let legacy = |error: io::Error| PeeringStateError::Legacy {
            path: path.display().to_string(),
            error: error.to_string(),
        };
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(legacy(error)),
        };
        let version: u64 = text.trim().parse().map_err(|_| {
            PeeringStateError::Corrupt(format!(
                "the legacy directory floor at {} does not parse",
                path.display()
            ))
        })?;
        self.raise(version).await?;
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(legacy(error)),
        }
        tracing::info!(device = %self.device, version, "peering: the legacy directory floor is adopted");
        Ok(())
    }

    /// Admit a verified directory at `version`: refused as a rollback when it is below the
    /// floor the store holds now (re-read, so another process's raise counts), otherwise
    /// persisted as the new floor before this returns.
    ///
    /// # Errors
    ///
    /// [`PeeringError::DirectoryRollback`] for a stale directory; [`PeeringError::Io`] or
    /// [`PeeringError::Codec`] when the store cannot be read or written.
    #[instrument(level = "debug", skip(self), fields(device = %self.device), err)]
    pub async fn admit(&self, version: u64) -> Result<(), PeeringError> {
        let floor = load_floor(&self.db, self.device).await?;
        self.persisted.fetch_max(floor, Ordering::AcqRel);
        if version < floor {
            tracing::warn!(device = %self.device, version, floor, "peering: a stale directory is refused");
            return Err(PeeringError::DirectoryRollback {
                pinned: version,
                highest: floor,
            });
        }
        self.raise(version).await?;
        Ok(())
    }

    async fn raise(&self, version: u64) -> Result<(), PeeringStateError> {
        let floor = raise_floor(&self.db, self.device, version).await?;
        self.persisted.fetch_max(floor, Ordering::AcqRel);
        Ok(())
    }
}

impl DirectoryVersions for DbDirectoryVersions {
    fn highest(&self) -> Result<u64, PeeringError> {
        Ok(self.persisted.load(Ordering::Acquire))
    }

    fn record(&self, version: u64) -> Result<(), PeeringError> {
        let persisted = self.persisted.load(Ordering::Acquire);
        if version <= persisted {
            return Ok(());
        }
        tracing::error!(device = %self.device, version, persisted, "peering: an unpersisted directory floor raise is refused");
        Err(PeeringError::Io(format!(
            "directory version {version} is above the persisted floor {persisted}; it must be \
             admitted durably first"
        )))
    }
}

// ─── Tombstones ──────────────────────────────────────────────────────────────

/// The peering asset id a sync feed id names: its 16 raw bytes, or its UTF-8 text when that is a
/// UUID. Any other id cannot be offered by a peer, which names assets by UUID.
#[must_use]
pub fn feed_asset_uuid(id: &[u8]) -> Option<Uuid> {
    Uuid::from_slice(id)
        .ok()
        .or_else(|| std::str::from_utf8(id).ok()?.parse().ok())
}

/// Record a tombstone for each of `assets`, recorded at `at`. An asset already tombstoned keeps
/// its first record. Called inside the transaction that lands the deletes.
///
/// # Errors
///
/// When a row cannot be written.
pub async fn record_tombstones<C: ConnectionTrait>(
    db: &C,
    assets: &[Uuid],
    at: Timestamp,
) -> Result<(), DbErr> {
    let recorded_at = at.to_string();
    for asset in assets {
        db.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO peering_tombstones (asset_id, recorded_at) VALUES (?, ?) \
             ON CONFLICT (asset_id) DO NOTHING",
            [
                asset.hyphenated().to_string().into(),
                recorded_at.clone().into(),
            ],
        ))
        .await?;
        tracing::debug!(%asset, "peering: tombstone recorded");
    }
    Ok(())
}

/// Every asset this device holds a tombstone for: the recorded tombstones, and every asset the
/// sync store marks deleted (which covers deletes landed before tombstones were recorded).
///
/// # Errors
///
/// [`PeeringStateError::Db`] when the store cannot be read, [`PeeringStateError::Corrupt`] when
/// a stored id does not decode: the set is refused whole rather than returned with a gap.
#[instrument(level = "debug", skip(db), err)]
pub async fn load_tombstones<C: ConnectionTrait>(
    db: &C,
) -> Result<BTreeSet<Uuid>, PeeringStateError> {
    let mut tombstones = BTreeSet::new();
    for row in peering_tombstone::Entity::find().all(db).await? {
        let asset = row.asset_id.parse::<Uuid>().map_err(|_| {
            PeeringStateError::Corrupt(format!("tombstone asset id {:?}", row.asset_id))
        })?;
        tombstones.insert(asset);
    }
    let recorded = tombstones.len();
    for row in synced_asset::Entity::find()
        .filter(synced_asset::Column::Tombstoned.eq(true))
        .all(db)
        .await?
    {
        if let Some(asset) = feed_asset_uuid(&decode_id(&row.asset_id)?) {
            tombstones.insert(asset);
        }
    }
    tracing::debug!(
        recorded,
        total = tombstones.len(),
        "peering: tombstones are loaded"
    );
    Ok(tombstones)
}

#[cfg(test)]
mod tests;
