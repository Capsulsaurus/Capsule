//! [`PostgresUploadSessions`] — `upload_sessions`, one row per upload.
//!
//! # The hot path: one asynchronously committed update per chunk
//!
//! [`UploadSessionStore::record_range`] runs once for every chunk accepted. It reads the row
//! `FOR UPDATE`, merges the range into the committed set in Rust (the one implementation of the
//! merge, [`crate::store::CommittedSet::with_range`]), and writes it back — in a transaction
//! with `SET LOCAL synchronous_commit = off` (ADR-0015). The commit returns once the change is
//! in the write-ahead log's buffer rather than after it is flushed, so a chunk costs the blob's
//! own `fsync` and not a second one.
//!
//! What that can lose is bounded and safe: a crash within the log writer's flush delay can
//! forget the last ranges recorded. Their bytes are already durable in the stage; the session
//! simply reports them uncommitted, the client's next `HEAD` sees that, and it sends them again.
//! Finalize re-hashes the whole stage before anything becomes durable, so nothing a lost range
//! record could do reaches an asset. Every other transition — open, claim, fail, discard — is
//! committed synchronously, because losing one of those *would* be visible.
//!
//! None of the columns a chunk changes (`committed`, `last_progress_at`, the one-time
//! `pending → uploading` aside) is indexed, and the table's fill factor leaves room on the page,
//! so the update is heap-only.

use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};
use sea_orm::{ConnectionTrait, DatabaseConnection, QueryResult, TransactionTrait, Value};

use super::{column, instant, statement};
use crate::postgres::error::Port;
use crate::postgres::time::to_micros;
use crate::store::upload::{
    BlobRole, CommittedRange, CommittedSet, FinalizeClaim, UnclaimedDiscard, UploadSessionRecord,
    UploadSessionStatus, UploadSessionStore,
};
use crate::store::{
    AlbumId, AssetId, Clock, OwnerId, StoreError, StoreFuture, UploadId, UserId, deadline,
};

const PORT: Port = Port {
    store: "upload sessions",
    record: "UploadSessionRecord",
};

/// The columns an [`UploadSessionRecord`] reads back from.
const COLUMNS: &str = "upload_id, asset_id, owner_id, upload_user_id, album_id, content_type, \
                       expected_hash, crypto_suite_id, protocol_version, blob_role, intent_id, \
                       manifest_envelope, committed, in_flight_cap, total_size, status, \
                       failure_code, created_at, last_progress_at";

/// The statuses a session can still be recorded against, claimed or discarded from.
const EVICTABLE: &str = "('pending', 'uploading')";

/// The statuses that are not terminal.
const ACTIVE: &str = "('pending', 'uploading', 'waiting_for_processing')";

/// The PostgreSQL [`UploadSessionStore`].
#[derive(Debug, Clone)]
pub struct PostgresUploadSessions {
    connection: DatabaseConnection,
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
}

impl PostgresUploadSessions {
    /// A store over `connection` on `clock`, each session living `ttl` from opening.
    pub fn new(connection: DatabaseConnection, clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self {
            connection,
            clock,
            ttl,
        }
    }

    fn now(&self) -> Value {
        Value::from(to_micros(self.clock.now()))
    }

    /// Run an `UPDATE … RETURNING` against one live session and read it back.
    async fn update_live(
        &self,
        upload: &UploadId,
        assignments: &str,
        extra: Vec<Value>,
        doing: &'static str,
    ) -> Result<Option<UploadSessionRecord>, StoreError> {
        let mut values = vec![Value::from(upload.as_str().to_owned()), self.now()];
        values.extend(extra);
        let row = self
            .connection
            .query_one_raw(statement(
                &format!(
                    "UPDATE upload_sessions SET {assignments} \
                     WHERE upload_id = $1 AND expires_at > $2 RETURNING {COLUMNS}"
                ),
                values,
            ))
            .await
            .map_err(PORT.failing(doing))?;
        row.as_ref().map(record_from).transpose()
    }

    /// The ids a `SELECT upload_id …` returned.
    async fn ids(
        &self,
        sql: &str,
        values: Vec<Value>,
        doing: &'static str,
    ) -> Result<Vec<UploadId>, StoreError> {
        let rows = self
            .connection
            .query_all_raw(statement(sql, values))
            .await
            .map_err(PORT.failing(doing))?;
        rows.iter()
            .map(|row| column::<String>(PORT, row, "upload_id").map(UploadId::new))
            .collect()
    }
}

/// Read one `upload_sessions` row back.
fn record_from(row: &QueryResult) -> Result<UploadSessionRecord, StoreError> {
    let blob_role: String = column(PORT, row, "blob_role")?;
    let status: String = column(PORT, row, "status")?;
    let committed: String = column(PORT, row, "committed")?;
    let crypto_suite_id: i32 = column(PORT, row, "crypto_suite_id")?;
    let in_flight_cap: i16 = column(PORT, row, "in_flight_cap")?;
    let total_size: i64 = column(PORT, row, "total_size")?;
    let album_id: Option<String> = column(PORT, row, "album_id")?;
    Ok(UploadSessionRecord {
        upload_id: UploadId::new(column::<String>(PORT, row, "upload_id")?),
        asset_id: AssetId::new(column::<String>(PORT, row, "asset_id")?),
        owner_id: OwnerId::new(column::<String>(PORT, row, "owner_id")?),
        upload_user_id: UserId::new(column::<String>(PORT, row, "upload_user_id")?),
        album_id: album_id.map(AlbumId::new),
        content_type: column(PORT, row, "content_type")?,
        expected_hash: column(PORT, row, "expected_hash")?,
        crypto_suite_id: u16::try_from(crypto_suite_id)
            .map_err(|_| PORT.undecodable(format!("crypto_suite_id {crypto_suite_id}")))?,
        protocol_version: column(PORT, row, "protocol_version")?,
        blob_role: BlobRole::from_token(&blob_role)
            .ok_or_else(|| PORT.undecodable(format!("`{blob_role}` is not a blob role")))?,
        intent_id: column(PORT, row, "intent_id")?,
        manifest_envelope: column(PORT, row, "manifest_envelope")?,
        committed: CommittedSet::from_stored(&committed)
            .ok_or_else(|| PORT.undecodable("committed is not a normal range set".to_owned()))?,
        in_flight_cap: u8::try_from(in_flight_cap)
            .map_err(|_| PORT.undecodable(format!("in_flight_cap {in_flight_cap}")))?,
        total_size: u64::try_from(total_size)
            .map_err(|_| PORT.undecodable(format!("total_size {total_size}")))?,
        status: UploadSessionStatus::from_token(&status)
            .ok_or_else(|| PORT.undecodable(format!("`{status}` is not a session status")))?,
        failure_code: column(PORT, row, "failure_code")?,
        created_at: instant(PORT, row, "created_at")?,
        last_progress_at: instant(PORT, row, "last_progress_at")?,
    })
}

/// A size a record carries, as the `BIGINT` column holds it.
fn stored_size(size: u64) -> Result<i64, StoreError> {
    i64::try_from(size).map_err(|_| StoreError::Rejected {
        store: PORT.store,
        detail: format!("{size} bytes does not fit the total_size column"),
    })
}

impl UploadSessionStore for PostgresUploadSessions {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn open(&self, record: UploadSessionRecord) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let expires_at = deadline(self.clock.now(), self.ttl);
            self.connection
                .execute_raw(statement(
                    &format!(
                        "INSERT INTO upload_sessions ({COLUMNS}, expires_at) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, \
                                 $15, $16, $17, $18, $19, $20) \
                         ON CONFLICT (upload_id) DO UPDATE SET asset_id = EXCLUDED.asset_id, \
                           owner_id = EXCLUDED.owner_id, \
                           upload_user_id = EXCLUDED.upload_user_id, \
                           album_id = EXCLUDED.album_id, content_type = EXCLUDED.content_type, \
                           expected_hash = EXCLUDED.expected_hash, \
                           crypto_suite_id = EXCLUDED.crypto_suite_id, \
                           protocol_version = EXCLUDED.protocol_version, \
                           blob_role = EXCLUDED.blob_role, intent_id = EXCLUDED.intent_id, \
                           manifest_envelope = EXCLUDED.manifest_envelope, \
                           committed = EXCLUDED.committed, \
                           in_flight_cap = EXCLUDED.in_flight_cap, \
                           total_size = EXCLUDED.total_size, status = EXCLUDED.status, \
                           failure_code = EXCLUDED.failure_code, \
                           created_at = EXCLUDED.created_at, \
                           last_progress_at = EXCLUDED.last_progress_at, \
                           expires_at = EXCLUDED.expires_at"
                    ),
                    [
                        Value::from(record.upload_id.as_str().to_owned()),
                        Value::from(record.asset_id.as_str().to_owned()),
                        Value::from(record.owner_id.as_str().to_owned()),
                        Value::from(record.upload_user_id.as_str().to_owned()),
                        Value::from(record.album_id.as_ref().map(|id| id.as_str().to_owned())),
                        Value::from(record.content_type.clone()),
                        Value::from(record.expected_hash.clone()),
                        Value::from(i32::from(record.crypto_suite_id)),
                        Value::from(record.protocol_version.clone()),
                        Value::from(record.blob_role.as_str()),
                        Value::from(record.intent_id.clone()),
                        Value::from(record.manifest_envelope.clone()),
                        Value::from(record.committed.to_stored()),
                        Value::from(i16::from(record.in_flight_cap)),
                        Value::from(stored_size(record.total_size)?),
                        Value::from(record.status.as_str()),
                        Value::from(record.failure_code.clone()),
                        Value::from(to_micros(record.created_at)),
                        Value::from(to_micros(record.last_progress_at)),
                        Value::from(to_micros(expires_at)),
                    ],
                ))
                .await
                .map_err(PORT.failing("opening an upload session"))?;
            tracing::debug!(
                upload_id = %record.upload_id,
                uploader = %record.upload_user_id,
                "opened upload session"
            );
            Ok(())
        })
    }

    fn read<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        Box::pin(async move {
            let row = self
                .connection
                .query_one_raw(statement(
                    &format!(
                        "SELECT {COLUMNS} FROM upload_sessions \
                         WHERE upload_id = $1 AND expires_at > $2"
                    ),
                    [Value::from(upload.as_str().to_owned()), self.now()],
                ))
                .await
                .map_err(PORT.failing("reading an upload session"))?;
            let found = row.as_ref().map(record_from).transpose()?;
            tracing::trace!(%upload, hit = found.is_some(), "read upload session");
            Ok(found)
        })
    }

    fn sessions_for_uploader<'a>(
        &'a self,
        uploader: &'a UserId,
    ) -> StoreFuture<'a, Vec<UploadSessionRecord>> {
        Box::pin(async move {
            let rows = self
                .connection
                .query_all_raw(statement(
                    &format!(
                        "SELECT {COLUMNS} FROM upload_sessions \
                         WHERE upload_user_id = $1 AND expires_at > $2 \
                         ORDER BY created_at, upload_id COLLATE \"C\""
                    ),
                    [Value::from(uploader.as_str().to_owned()), self.now()],
                ))
                .await
                .map_err(PORT.failing("listing an uploader's sessions"))?;
            let found = rows
                .iter()
                .map(record_from)
                .collect::<Result<Vec<_>, _>>()?;
            tracing::trace!(%uploader, count = found.len(), "listed upload sessions");
            Ok(found)
        })
    }

    fn record_range<'a>(
        &'a self,
        upload: &'a UploadId,
        range: CommittedRange,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        Box::pin(async move {
            let transaction = self
                .connection
                .begin()
                .await
                .map_err(PORT.failing("beginning a range record"))?;
            transaction
                .execute_unprepared("SET LOCAL synchronous_commit = off")
                .await
                .map_err(PORT.failing("relaxing a range record's commit"))?;
            let row = transaction
                .query_one_raw(statement(
                    &format!(
                        "SELECT committed FROM upload_sessions \
                         WHERE upload_id = $1 AND expires_at > $2 AND status IN {ACTIVE} \
                         FOR UPDATE"
                    ),
                    [Value::from(upload.as_str().to_owned()), self.now()],
                ))
                .await
                .map_err(PORT.failing("locking a session to record a range"))?;
            let Some(row) = row else {
                tracing::debug!(%upload, "a range found no active session to record against");
                return Ok(None);
            };
            let committed: String = column(PORT, &row, "committed")?;
            let merged = CommittedSet::from_stored(&committed)
                .ok_or_else(|| PORT.undecodable("committed is not a normal range set".to_owned()))?
                .with_range(range.start, range.end);
            let updated = transaction
                .query_one_raw(statement(
                    &format!(
                        "UPDATE upload_sessions SET committed = $2, last_progress_at = $3, \
                           status = CASE WHEN status = 'pending' THEN 'uploading' ELSE status END \
                         WHERE upload_id = $1 RETURNING {COLUMNS}"
                    ),
                    [
                        Value::from(upload.as_str().to_owned()),
                        Value::from(merged.to_stored()),
                        Value::from(to_micros(range.accepted_at)),
                    ],
                ))
                .await
                .map_err(PORT.failing("recording a committed range"))?;
            transaction
                .commit()
                .await
                .map_err(PORT.failing("committing a range record"))?;
            let updated = updated.as_ref().map(record_from).transpose()?;
            if let Some(record) = &updated {
                tracing::debug!(
                    %upload,
                    start = range.start,
                    end = range.end,
                    fragments = record.committed.fragments(),
                    prefix = record.committed.prefix(),
                    "recorded a committed range"
                );
            }
            Ok(updated)
        })
    }

    fn set_status<'a>(
        &'a self,
        upload: &'a UploadId,
        status: UploadSessionStatus,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        Box::pin(async move {
            let updated = self
                .update_live(
                    upload,
                    "status = $3",
                    vec![Value::from(status.as_str())],
                    "setting an upload status",
                )
                .await?;
            tracing::debug!(%upload, status = status.as_str(), hit = updated.is_some(), "set upload status");
            Ok(updated)
        })
    }

    fn fail_processing<'a>(
        &'a self,
        upload: &'a UploadId,
        failure_code: &'a str,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        Box::pin(async move {
            let updated = self
                .update_live(
                    upload,
                    "status = 'failed_processing', failure_code = $3",
                    vec![Value::from(failure_code.to_owned())],
                    "failing an upload session",
                )
                .await?;
            tracing::debug!(%upload, failure_code, hit = updated.is_some(), "failed an upload session");
            Ok(updated)
        })
    }

    fn claim_finalize<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, FinalizeClaim> {
        Box::pin(async move {
            let won = self
                .connection
                .query_one_raw(statement(
                    &format!(
                        "UPDATE upload_sessions SET status = 'waiting_for_processing' \
                         WHERE upload_id = $1 AND expires_at > $2 AND status IN {EVICTABLE} \
                         RETURNING {COLUMNS}"
                    ),
                    [Value::from(upload.as_str().to_owned()), self.now()],
                ))
                .await
                .map_err(PORT.failing("claiming finalization"))?;
            if let Some(row) = won {
                tracing::info!(%upload, "claimed finalization");
                return Ok(FinalizeClaim::Won(Box::new(record_from(&row)?)));
            }
            let held = self
                .connection
                .query_one_raw(statement(
                    "SELECT status FROM upload_sessions WHERE upload_id = $1 AND expires_at > $2",
                    [Value::from(upload.as_str().to_owned()), self.now()],
                ))
                .await
                .map_err(PORT.failing("reading a refused finalization claim"))?;
            if held.is_some() {
                tracing::debug!(%upload, "finalize already claimed");
                Ok(FinalizeClaim::AlreadyClaimed)
            } else {
                tracing::debug!(%upload, "finalize claim found no session");
                Ok(FinalizeClaim::NotFound)
            }
        })
    }

    fn claimed(&self) -> StoreFuture<'_, Vec<UploadSessionRecord>> {
        Box::pin(async move {
            let rows = self
                .connection
                .query_all_raw(statement(
                    &format!(
                        "SELECT {COLUMNS} FROM upload_sessions \
                         WHERE status = 'waiting_for_processing' AND expires_at > $1 \
                         ORDER BY created_at, upload_id COLLATE \"C\""
                    ),
                    [self.now()],
                ))
                .await
                .map_err(PORT.failing("listing claimed upload sessions"))?;
            let found = rows
                .iter()
                .map(record_from)
                .collect::<Result<Vec<_>, _>>()?;
            tracing::debug!(count = found.len(), "listed claimed upload sessions");
            Ok(found)
        })
    }

    fn discard<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, Option<UploadSessionRecord>> {
        Box::pin(async move {
            let row = self
                .connection
                .query_one_raw(statement(
                    &format!(
                        "DELETE FROM upload_sessions WHERE upload_id = $1 \
                         RETURNING {COLUMNS}, expires_at"
                    ),
                    [Value::from(upload.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("discarding an upload session"))?;
            let now = self.clock.now();
            let removed = match row {
                Some(row) if instant(PORT, &row, "expires_at")? > now => Some(record_from(&row)?),
                _ => None,
            };
            tracing::info!(%upload, hit = removed.is_some(), "discarded upload session");
            Ok(removed)
        })
    }

    fn discard_unclaimed<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, UnclaimedDiscard> {
        Box::pin(async move {
            let transaction = self
                .connection
                .begin()
                .await
                .map_err(PORT.failing("beginning an unclaimed discard"))?;
            let held = transaction
                .query_one_raw(statement(
                    "SELECT status FROM upload_sessions \
                     WHERE upload_id = $1 AND expires_at > $2 FOR UPDATE",
                    [Value::from(upload.as_str().to_owned()), self.now()],
                ))
                .await
                .map_err(PORT.failing("locking a session to discard it"))?;
            let Some(held) = held else {
                tracing::debug!(%upload, "an unclaimed discard found no session");
                return Ok(UnclaimedDiscard::NotFound);
            };
            let status: String = column(PORT, &held, "status")?;
            let evictable = UploadSessionStatus::from_token(&status)
                .is_some_and(UploadSessionStatus::is_evictable);
            if !evictable {
                tracing::debug!(%upload, status, "an unclaimed discard found the session claimed");
                return Ok(UnclaimedDiscard::Claimed);
            }
            let removed = transaction
                .query_one_raw(statement(
                    &format!(
                        "DELETE FROM upload_sessions WHERE upload_id = $1 RETURNING {COLUMNS}"
                    ),
                    [Value::from(upload.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("discarding an unclaimed session"))?;
            transaction
                .commit()
                .await
                .map_err(PORT.failing("committing an unclaimed discard"))?;
            let Some(removed) = removed else {
                return Ok(UnclaimedDiscard::NotFound);
            };
            tracing::info!(%upload, "discarded an unclaimed upload session");
            Ok(UnclaimedDiscard::Discarded(Box::new(record_from(
                &removed,
            )?)))
        })
    }

    fn pending_for_address<'a>(
        &'a self,
        uploader: &'a UserId,
        expected_hash: &'a str,
    ) -> StoreFuture<'a, Option<UploadId>> {
        Box::pin(async move {
            let found = self
                .ids(
                    &format!(
                        "SELECT upload_id FROM upload_sessions \
                         WHERE upload_user_id = $1 AND expected_hash = $2 AND expires_at > $3 \
                           AND status IN {ACTIVE} \
                         ORDER BY upload_id COLLATE \"C\" LIMIT 1"
                    ),
                    vec![
                        Value::from(uploader.as_str().to_owned()),
                        Value::from(expected_hash.to_owned()),
                        self.now(),
                    ],
                    "looking up an uploader's session for an address",
                )
                .await?;
            Ok(found.into_iter().next())
        })
    }

    fn live_for_address<'a>(&'a self, expected_hash: &'a str) -> StoreFuture<'a, Vec<UploadId>> {
        Box::pin(async move {
            let found = self
                .ids(
                    &format!(
                        "SELECT upload_id FROM upload_sessions \
                         WHERE expected_hash = $1 AND expires_at > $2 AND status IN {ACTIVE} \
                         ORDER BY upload_id COLLATE \"C\""
                    ),
                    vec![Value::from(expected_hash.to_owned()), self.now()],
                    "listing live sessions for an address",
                )
                .await?;
            tracing::debug!(count = found.len(), "listed live sessions for an address");
            Ok(found)
        })
    }

    fn in_flight_for_album<'a>(&'a self, album: &'a AlbumId) -> StoreFuture<'a, u64> {
        Box::pin(async move {
            let row = self
                .connection
                .query_one_raw(statement(
                    &format!(
                        "SELECT count(*) AS in_flight FROM upload_sessions \
                         WHERE album_id = $1 AND expires_at > $2 AND status IN {ACTIVE}"
                    ),
                    [Value::from(album.as_str().to_owned()), self.now()],
                ))
                .await
                .map_err(PORT.failing("counting an album's sessions in flight"))?;
            let count: i64 = match row {
                Some(row) => column(PORT, &row, "in_flight")?,
                None => 0,
            };
            Ok(u64::try_from(count).unwrap_or(0))
        })
    }

    fn least_recently_progressed(
        &self,
        not_progressed_since: Timestamp,
        limit: usize,
    ) -> StoreFuture<'_, Vec<UploadId>> {
        Box::pin(async move {
            let picked = self
                .ids(
                    &format!(
                        "SELECT upload_id FROM upload_sessions \
                         WHERE status IN {EVICTABLE} AND last_progress_at < $1 \
                           AND expires_at > $2 \
                         ORDER BY last_progress_at, upload_id COLLATE \"C\" LIMIT $3"
                    ),
                    vec![
                        Value::from(to_micros(not_progressed_since)),
                        self.now(),
                        Value::from(i64::try_from(limit).unwrap_or(i64::MAX)),
                    ],
                    "listing eviction candidates",
                )
                .await?;
            tracing::debug!(count = picked.len(), "listed eviction candidates");
            Ok(picked)
        })
    }
}
