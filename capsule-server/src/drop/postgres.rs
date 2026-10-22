//! [`PostgresDrops`] — the durable guest-drop store (`S-C5`, #446).
//!
//! # Three tables
//!
//! `drop_links` holds each provisioned link with its caps **and** its running totals,
//! `drop_pending` the guest declarations keyed by upload session, and `drop_inbox` the finished
//! drops waiting for their owner.
//!
//! # `charge` is a row lock around the shared rule
//!
//! The port's requirement is that the cap decision and the reservation are one operation. The
//! adapter opens a transaction, takes the link's row with `SELECT … FOR UPDATE`, hydrates the
//! whole [`UploadLinkRecord`], and runs `super::admit` — the same function the in-memory
//! adapter runs under its mutex — writing the totals back only on an admission. Two guests
//! charging one link serialize on the row, so neither can read room the other is about to
//! take. A link that does not exist has no row to lock and nothing to admit, so no advisory lock
//! is needed for the first-write case the membership adapter has to cover.
//!
//! # Every other write is one statement
//!
//! `revoke`, `refund`, `claim`, `settle`, `release` and `discard` each touch one row with one
//! statement whose row count (or `RETURNING` row) is the answer, so each check is inside its own
//! write. `claim` in particular is `UPDATE … WHERE NOT adopting RETURNING …`: two concurrent
//! adoptions cannot both be handed the row.
//!
//! # `provision` refuses a held id; `reserve` and `deposit` overwrite
//!
//! `provision` is `INSERT … ON CONFLICT (opaque_id) DO NOTHING`, and a row count of zero is
//! [`LinkProvisionOutcome::Conflict`]: a held link keeps its owner, key and totals (#553).
//! `reserve` and `deposit` are upserts that replace a row under the same key, as the in-memory adapter's
//! map inserts do; the conformance suite holds both adapters to one behaviour.
//!
//! # Byte counts and the `BIGINT` column
//!
//! A `u64` cap past `i64::MAX` is stored as `i64::MAX`. Nine exabytes is indistinguishable from
//! no cap at all for any link this server can fill, and clamping keeps a client-chosen
//! absurdity from turning `provision` into a storage failure. A running total or a declared size
//! past `i64::MAX` is a value no admission could have produced, and is refused as
//! [`StoreError::Rejected`] rather than truncated.

use jiff::Timestamp;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, QueryResult, Statement,
    TransactionTrait, Value,
};

use super::{
    Admission, DropStore, InboxEntry, LinkCaps, LinkProvisionOutcome, PendingDeposit,
    UploadLinkRecord, admit,
};
use crate::blob::ContentAddress;
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};
use crate::store::{StoreError, StoreFuture, UploadId, UserId};

/// Which port is speaking, for every error this adapter raises.
const PORT: Port = Port {
    store: "drop",
    record: "UploadLinkRecord",
};

/// The columns a link is read from, in one place so every `SELECT` agrees.
const LINK_COLUMNS: &str = "opaque_id, owner_id, drop_pubkey, crypto_suite_id, \
     protocol_version, expires_at, max_total_bytes, max_file_count, max_file_size, single_use, \
     passphrase_verifier, used_bytes, used_files, revoked_at";

/// The columns an inbox entry is read from.
const INBOX_COLUMNS: &str = "drop_id, owner_id, opaque_id, address, size, content_type, kem_ct, \
     suggested_filename, received_at, adopting";

/// The durable guest-drop store.
#[derive(Debug, Clone)]
pub struct PostgresDrops {
    connection: DatabaseConnection,
}

impl PostgresDrops {
    /// A store over `connection`.
    pub fn new(connection: DatabaseConnection) -> Self {
        Self { connection }
    }
}

/// A cap as its column: clamped, because past `i64::MAX` it means "no cap" in practice.
fn cap_to_column(cap: Option<u64>) -> Value {
    Value::BigInt(cap.map(|cap| i64::try_from(cap).unwrap_or(i64::MAX)))
}

/// A count or size as its column, refused rather than truncated past `i64::MAX`.
fn count_to_column(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::Rejected {
        store: PORT.store,
        detail: format!("{value} is past what a BIGINT column holds"),
    })
}

/// A column as an unsigned count.
fn count_from<T: TryFrom<i64>>(value: i64, what: &str) -> Result<T, StoreError> {
    T::try_from(value).map_err(|_| PORT.undecodable(format!("{value} is not a {what}")))
}

/// An optional column as an unsigned count.
fn optional_count<T: TryFrom<i64>>(
    value: Option<i64>,
    what: &str,
) -> Result<Option<T>, StoreError> {
    value.map(|value| count_from(value, what)).transpose()
}

/// An optional instant as its column.
fn optional_micros(at: Option<Timestamp>) -> Value {
    Value::BigInt(at.map(to_micros))
}

/// A column as an instant.
fn instant(micros: i64) -> Result<Timestamp, StoreError> {
    from_micros(micros)
        .ok_or_else(|| PORT.undecodable(format!("{micros}µs is not a representable instant")))
}

/// An optional column as an instant.
fn optional_instant(micros: Option<i64>) -> Result<Option<Timestamp>, StoreError> {
    micros.map(instant).transpose()
}

/// Begin a transaction, or say why not.
async fn begin(connection: &DatabaseConnection) -> Result<DatabaseTransaction, StoreError> {
    connection
        .begin()
        .await
        .map_err(PORT.failing("opening a transaction"))
}

/// A `drop_links` row as the record it holds.
fn link_from(row: &QueryResult, doing: &'static str) -> Result<UploadLinkRecord, StoreError> {
    let failed = PORT.failing(doing);
    let opaque_id: String = row.try_get("", "opaque_id").map_err(&failed)?;
    let owner_id: String = row.try_get("", "owner_id").map_err(&failed)?;
    let drop_pubkey: Vec<u8> = row.try_get("", "drop_pubkey").map_err(&failed)?;
    let crypto_suite_id: i32 = row.try_get("", "crypto_suite_id").map_err(&failed)?;
    let protocol_version: String = row.try_get("", "protocol_version").map_err(&failed)?;
    let expires_at: Option<i64> = row.try_get("", "expires_at").map_err(&failed)?;
    let max_total_bytes: Option<i64> = row.try_get("", "max_total_bytes").map_err(&failed)?;
    let max_file_count: Option<i64> = row.try_get("", "max_file_count").map_err(&failed)?;
    let max_file_size: Option<i64> = row.try_get("", "max_file_size").map_err(&failed)?;
    let single_use: bool = row.try_get("", "single_use").map_err(&failed)?;
    let passphrase_verifier: Option<Vec<u8>> =
        row.try_get("", "passphrase_verifier").map_err(&failed)?;
    let used_bytes: i64 = row.try_get("", "used_bytes").map_err(&failed)?;
    let used_files: i64 = row.try_get("", "used_files").map_err(&failed)?;
    let revoked_at: Option<i64> = row.try_get("", "revoked_at").map_err(&failed)?;
    Ok(UploadLinkRecord {
        opaque_id,
        owner_id: UserId::new(owner_id),
        drop_pubkey,
        crypto_suite_id: u16::try_from(crypto_suite_id)
            .map_err(|_| PORT.undecodable(format!("{crypto_suite_id} is not a crypto suite id")))?,
        protocol_version,
        caps: LinkCaps {
            expires_at: optional_instant(expires_at)?,
            max_total_bytes: optional_count(max_total_bytes, "byte cap")?,
            max_file_count: optional_count(max_file_count, "file-count cap")?,
            max_file_size: optional_count(max_file_size, "file-size cap")?,
            single_use,
        },
        passphrase_verifier,
        used_bytes: count_from(used_bytes, "byte total")?,
        used_files: count_from(used_files, "file total")?,
        revoked_at: optional_instant(revoked_at)?,
    })
}

/// A `drop_inbox` row as the entry it holds.
fn entry_from(row: &QueryResult, doing: &'static str) -> Result<InboxEntry, StoreError> {
    let failed = PORT.failing(doing);
    let drop_id: String = row.try_get("", "drop_id").map_err(&failed)?;
    let owner_id: String = row.try_get("", "owner_id").map_err(&failed)?;
    let opaque_id: String = row.try_get("", "opaque_id").map_err(&failed)?;
    let address: String = row.try_get("", "address").map_err(&failed)?;
    let size: i64 = row.try_get("", "size").map_err(&failed)?;
    let content_type: String = row.try_get("", "content_type").map_err(&failed)?;
    let kem_ct: Vec<u8> = row.try_get("", "kem_ct").map_err(&failed)?;
    let suggested_filename: Option<String> =
        row.try_get("", "suggested_filename").map_err(&failed)?;
    let received_at: i64 = row.try_get("", "received_at").map_err(&failed)?;
    let adopting: bool = row.try_get("", "adopting").map_err(&failed)?;
    Ok(InboxEntry {
        drop_id,
        owner_id: UserId::new(owner_id),
        opaque_id,
        address: ContentAddress::parse(&address)
            .map_err(|_| PORT.undecodable(format!("`{address}` is not a content address")))?,
        size: count_from(size, "size")?,
        content_type,
        kem_ct,
        suggested_filename,
        received_at: instant(received_at)?,
        adopting,
    })
}

impl DropStore for PostgresDrops {
    fn provision(&self, record: UploadLinkRecord) -> StoreFuture<'_, LinkProvisionOutcome> {
        Box::pin(async move {
            let used_bytes = count_to_column(record.used_bytes)?;
            let inserted = self
                .connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    format!(
                        "INSERT INTO drop_links ({LINK_COLUMNS}) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
                         ON CONFLICT (opaque_id) DO NOTHING"
                    ),
                    [
                        Value::from(record.opaque_id.clone()),
                        Value::from(record.owner_id.as_str().to_owned()),
                        Value::from(record.drop_pubkey),
                        Value::from(i32::from(record.crypto_suite_id)),
                        Value::from(record.protocol_version),
                        optional_micros(record.caps.expires_at),
                        cap_to_column(record.caps.max_total_bytes),
                        Value::BigInt(record.caps.max_file_count.map(i64::from)),
                        cap_to_column(record.caps.max_file_size),
                        Value::from(record.caps.single_use),
                        Value::Bytes(record.passphrase_verifier),
                        Value::from(used_bytes),
                        Value::from(i64::from(record.used_files)),
                        optional_micros(record.revoked_at),
                    ],
                ))
                .await
                .map_err(PORT.failing("provisioning an upload link"))?;
            if inserted.rows_affected() == 0 {
                tracing::info!(
                    owner = %record.owner_id,
                    "an upload link was refused: its opaque id is already held"
                );
                return Ok(LinkProvisionOutcome::Conflict);
            }
            tracing::info!(owner = %record.owner_id, "an upload link was provisioned");
            Ok(LinkProvisionOutcome::Provisioned)
        })
    }

    fn resolve<'a>(&'a self, opaque_id: &'a str) -> StoreFuture<'a, Option<UploadLinkRecord>> {
        Box::pin(async move {
            let row = self
                .connection
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    format!("SELECT {LINK_COLUMNS} FROM drop_links WHERE opaque_id = $1"),
                    [Value::from(opaque_id.to_owned())],
                ))
                .await
                .map_err(PORT.failing("resolving an upload link"))?;
            row.map(|row| link_from(&row, "resolving an upload link"))
                .transpose()
        })
    }

    fn revoke<'a>(
        &'a self,
        owner: &'a UserId,
        opaque_id: &'a str,
        at: Timestamp,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let result = self
                .connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE drop_links SET revoked_at = $3 \
                     WHERE opaque_id = $1 AND owner_id = $2 AND revoked_at IS NULL",
                    [
                        Value::from(opaque_id.to_owned()),
                        Value::from(owner.as_str().to_owned()),
                        Value::from(to_micros(at)),
                    ],
                ))
                .await
                .map_err(PORT.failing("revoking an upload link"))?;
            let revoked = result.rows_affected() == 1;
            if revoked {
                tracing::info!(%owner, "an upload link was revoked");
            }
            Ok(revoked)
        })
    }

    fn charge<'a>(
        &'a self,
        opaque_id: &'a str,
        size: u64,
        at: Timestamp,
    ) -> StoreFuture<'a, Admission> {
        Box::pin(async move {
            let transaction = begin(&self.connection).await?;
            let Some(row) = transaction
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    format!(
                        "SELECT {LINK_COLUMNS} FROM drop_links WHERE opaque_id = $1 FOR UPDATE"
                    ),
                    [Value::from(opaque_id.to_owned())],
                ))
                .await
                .map_err(PORT.failing("locking an upload link"))?
            else {
                // Nothing to lock and nothing to admit; the rollback is the drop.
                return Ok(Admission::NotLive);
            };
            let mut link = link_from(&row, "locking an upload link")?;
            let admission = admit(&mut link, size, at);
            if matches!(admission, Admission::Admitted { .. }) {
                transaction
                    .execute_raw(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "UPDATE drop_links SET used_bytes = $2, used_files = $3 \
                         WHERE opaque_id = $1",
                        [
                            Value::from(opaque_id.to_owned()),
                            Value::from(count_to_column(link.used_bytes)?),
                            Value::from(i64::from(link.used_files)),
                        ],
                    ))
                    .await
                    .map_err(PORT.failing("reserving under an upload link's caps"))?;
                transaction
                    .commit()
                    .await
                    .map_err(PORT.failing("committing a drop reservation"))?;
            }
            Ok(admission)
        })
    }

    fn reserve(&self, pending: PendingDeposit, upload: &UploadId) -> StoreFuture<'_, ()> {
        let upload = upload.clone();
        Box::pin(async move {
            let size = count_to_column(pending.size)?;
            self.connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO drop_pending \
                       (upload_id, drop_id, opaque_id, owner_id, kem_ct, content_type, \
                        suggested_filename, size) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                     ON CONFLICT (upload_id) DO UPDATE SET \
                       drop_id = EXCLUDED.drop_id, \
                       opaque_id = EXCLUDED.opaque_id, \
                       owner_id = EXCLUDED.owner_id, \
                       kem_ct = EXCLUDED.kem_ct, \
                       content_type = EXCLUDED.content_type, \
                       suggested_filename = EXCLUDED.suggested_filename, \
                       size = EXCLUDED.size",
                    [
                        Value::from(upload.as_str().to_owned()),
                        Value::from(pending.drop_id),
                        Value::from(pending.opaque_id),
                        Value::from(pending.owner_id.as_str().to_owned()),
                        Value::from(pending.kem_ct),
                        Value::from(pending.content_type),
                        Value::String(pending.suggested_filename),
                        Value::from(size),
                    ],
                ))
                .await
                .map_err(PORT.failing("holding a drop declaration"))?;
            tracing::debug!(%upload, "a drop declaration is held until its bytes land");
            Ok(())
        })
    }

    fn take_reservation<'a>(
        &'a self,
        upload: &'a UploadId,
    ) -> StoreFuture<'a, Option<PendingDeposit>> {
        Box::pin(async move {
            let Some(row) = self
                .connection
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "DELETE FROM drop_pending WHERE upload_id = $1 \
                     RETURNING drop_id, opaque_id, owner_id, kem_ct, content_type, \
                               suggested_filename, size",
                    [Value::from(upload.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("taking a drop declaration"))?
            else {
                return Ok(None);
            };
            let failed = PORT.failing("taking a drop declaration");
            let drop_id: String = row.try_get("", "drop_id").map_err(&failed)?;
            let opaque_id: String = row.try_get("", "opaque_id").map_err(&failed)?;
            let owner_id: String = row.try_get("", "owner_id").map_err(&failed)?;
            let kem_ct: Vec<u8> = row.try_get("", "kem_ct").map_err(&failed)?;
            let content_type: String = row.try_get("", "content_type").map_err(&failed)?;
            let suggested_filename: Option<String> =
                row.try_get("", "suggested_filename").map_err(&failed)?;
            let size: i64 = row.try_get("", "size").map_err(&failed)?;
            Ok(Some(PendingDeposit {
                drop_id,
                opaque_id,
                owner_id: UserId::new(owner_id),
                kem_ct,
                content_type,
                suggested_filename,
                size: count_from(size, "size")?,
            }))
        })
    }

    fn refund<'a>(&'a self, opaque_id: &'a str, size: u64) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            // Saturating in both columns, as the in-memory adapter's `saturating_sub` is: a
            // refund larger than what was charged floors at zero rather than going negative.
            let size = i64::try_from(size).unwrap_or(i64::MAX);
            let result = self
                .connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE drop_links SET \
                       used_bytes = GREATEST(used_bytes - $2, 0), \
                       used_files = GREATEST(used_files - 1, 0) \
                     WHERE opaque_id = $1",
                    [Value::from(opaque_id.to_owned()), Value::from(size)],
                ))
                .await
                .map_err(PORT.failing("refunding a drop reservation"))?;
            if result.rows_affected() == 1 {
                tracing::debug!("an unused drop reservation was refunded to its link");
            }
            Ok(())
        })
    }

    fn deposit(&self, entry: InboxEntry) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let size = count_to_column(entry.size)?;
            self.connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    format!(
                        "INSERT INTO drop_inbox ({INBOX_COLUMNS}) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
                         ON CONFLICT (drop_id) DO UPDATE SET \
                           owner_id = EXCLUDED.owner_id, \
                           opaque_id = EXCLUDED.opaque_id, \
                           address = EXCLUDED.address, \
                           size = EXCLUDED.size, \
                           content_type = EXCLUDED.content_type, \
                           kem_ct = EXCLUDED.kem_ct, \
                           suggested_filename = EXCLUDED.suggested_filename, \
                           received_at = EXCLUDED.received_at, \
                           adopting = EXCLUDED.adopting"
                    ),
                    [
                        Value::from(entry.drop_id.clone()),
                        Value::from(entry.owner_id.as_str().to_owned()),
                        Value::from(entry.opaque_id),
                        Value::from(entry.address.as_str().to_owned()),
                        Value::from(size),
                        Value::from(entry.content_type),
                        Value::from(entry.kem_ct),
                        Value::String(entry.suggested_filename),
                        Value::from(to_micros(entry.received_at)),
                        Value::from(entry.adopting),
                    ],
                ))
                .await
                .map_err(PORT.failing("filing a drop in an inbox"))?;
            tracing::info!(
                owner = %entry.owner_id,
                size = entry.size,
                "a drop landed in an inbox"
            );
            Ok(())
        })
    }

    fn file<'a>(
        &'a self,
        upload: &'a UploadId,
        address: &'a ContentAddress,
        received_at: Timestamp,
    ) -> StoreFuture<'a, Option<InboxEntry>> {
        Box::pin(async move {
            // One statement, so the declaration leaves `drop_pending` exactly when its row lands
            // in `drop_inbox`. A row already holding the drop id is the drop, filed: the
            // declaration goes and nothing is returned.
            let row = self
                .connection
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    format!(
                        "WITH taken AS ( \
                           DELETE FROM drop_pending WHERE upload_id = $1 \
                           RETURNING drop_id, opaque_id, owner_id, kem_ct, content_type, \
                                     suggested_filename, size \
                         ) \
                         INSERT INTO drop_inbox ({INBOX_COLUMNS}) \
                         SELECT drop_id, owner_id, opaque_id, $2, size, content_type, kem_ct, \
                                suggested_filename, $3, FALSE \
                         FROM taken \
                         ON CONFLICT (drop_id) DO NOTHING \
                         RETURNING {INBOX_COLUMNS}"
                    ),
                    [
                        Value::from(upload.as_str().to_owned()),
                        Value::from(address.as_str().to_owned()),
                        Value::from(to_micros(received_at)),
                    ],
                ))
                .await
                .map_err(PORT.failing("filing a drop declaration in an inbox"))?;
            let Some(row) = row else {
                tracing::debug!(%upload, "no drop declaration is held; nothing to file");
                return Ok(None);
            };
            let entry = entry_from(&row, "filing a drop declaration in an inbox")?;
            tracing::info!(
                owner = %entry.owner_id,
                size = entry.size,
                "a drop landed in an inbox"
            );
            Ok(Some(entry))
        })
    }

    fn inbox<'a>(&'a self, owner: &'a UserId) -> StoreFuture<'a, Vec<InboxEntry>> {
        Box::pin(async move {
            // Oldest first, ties broken by the id's own bytes so the order is total and is the
            // in-memory adapter's.
            let rows = self
                .connection
                .query_all_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    format!(
                        "SELECT {INBOX_COLUMNS} FROM drop_inbox WHERE owner_id = $1 \
                         ORDER BY received_at ASC, drop_id COLLATE \"C\" ASC"
                    ),
                    [Value::from(owner.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("listing an inbox"))?;
            rows.iter()
                .map(|row| entry_from(row, "listing an inbox"))
                .collect()
        })
    }

    fn claim<'a>(
        &'a self,
        owner: &'a UserId,
        drop_id: &'a str,
    ) -> StoreFuture<'a, Option<InboxEntry>> {
        Box::pin(async move {
            // Another account's row and an already-claimed one answer identically, and the
            // check is inside the write: two concurrent adoptions cannot both be handed it.
            let row = self
                .connection
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    format!(
                        "UPDATE drop_inbox SET adopting = TRUE \
                         WHERE drop_id = $1 AND owner_id = $2 AND NOT adopting \
                         RETURNING {INBOX_COLUMNS}"
                    ),
                    [
                        Value::from(drop_id.to_owned()),
                        Value::from(owner.as_str().to_owned()),
                    ],
                ))
                .await
                .map_err(PORT.failing("claiming a drop for adoption"))?;
            row.map(|row| entry_from(&row, "claiming a drop for adoption"))
                .transpose()
        })
    }

    fn settle<'a>(&'a self, drop_id: &'a str) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            self.connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "DELETE FROM drop_inbox WHERE drop_id = $1",
                    [Value::from(drop_id.to_owned())],
                ))
                .await
                .map_err(PORT.failing("settling an adopted drop"))?;
            tracing::info!(%drop_id, "an adopted drop left the inbox");
            Ok(())
        })
    }

    fn release<'a>(&'a self, drop_id: &'a str) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let result = self
                .connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE drop_inbox SET adopting = FALSE WHERE drop_id = $1",
                    [Value::from(drop_id.to_owned())],
                ))
                .await
                .map_err(PORT.failing("releasing a refused adoption"))?;
            if result.rows_affected() == 1 {
                tracing::info!(%drop_id, "a refused adoption returned its drop to the inbox");
            }
            Ok(())
        })
    }

    fn discard<'a>(&'a self, owner: &'a UserId, drop_id: &'a str) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let result = self
                .connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "DELETE FROM drop_inbox WHERE drop_id = $1 AND owner_id = $2",
                    [
                        Value::from(drop_id.to_owned()),
                        Value::from(owner.as_str().to_owned()),
                    ],
                ))
                .await
                .map_err(PORT.failing("discarding a pending drop"))?;
            let discarded = result.rows_affected() == 1;
            if discarded {
                tracing::info!(%owner, "a pending drop was discarded");
            }
            Ok(discarded)
        })
    }
}

#[cfg(test)]
mod tests {
    /// The suite, against a real Postgres.
    mod postgres_conformance {
        use super::super::PostgresDrops;
        use crate::drop::DropStore;
        use crate::drop::conformance::{self, Harness};
        use crate::postgres::testing;

        /// A store over one container.
        #[derive(Debug)]
        struct PostgresHarness {
            drops: PostgresDrops,
        }

        impl Harness for PostgresHarness {
            fn drops(&self) -> &dyn DropStore {
                &self.drops
            }
        }

        #[tokio::test]
        async fn the_postgres_drop_store_conforms() {
            let Some(database) = testing::start("the Postgres drop store").await else {
                return;
            };
            let harness = PostgresHarness {
                drops: PostgresDrops::new(database.connection().clone()),
            };
            conformance::run_all(&harness).await;
        }
    }
}
