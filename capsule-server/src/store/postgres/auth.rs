//! [`PostgresAuthState`] — `auth_sessions`, one row per session.
//!
//! The per-user index the port's docs forbid naming is `idx_auth_sessions_user`: an index on
//! the record's own `user_id` column, so it cannot hold an entry its record does not. That is
//! the structural version of what the Valkey adapter kept true with a script.
//!
//! One read per authenticated request lands here ([`AuthStateStore::read_session`]), a
//! primary-key lookup. The activity stamp the scheme coalesces to once a minute is an update of
//! an unindexed column, which the table's fill factor keeps heap-only.

use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};
use sea_orm::{ConnectionTrait, DatabaseConnection, QueryResult, Value};
use uuid::Uuid;

use super::{column, instant, statement};
use crate::postgres::error::Port;
use crate::postgres::time::to_micros;
use crate::store::auth::{AuthStateStore, DEFAULT_SESSION_HARD_CAP, SessionRecord};
use crate::store::{Clock, SessionId, StoreError, StoreFuture, UserId};

const PORT: Port = Port {
    store: "auth sessions",
    record: "SessionRecord",
};

/// The columns a [`SessionRecord`] reads back from, in one place so every statement agrees.
const COLUMNS: &str = "session_id, user_id, created_at, authenticated_at, signed_in_at, \
                       last_active_at, user_agent, ip_address, cohort_hash, device_id, expires_at";

/// The PostgreSQL [`AuthStateStore`].
#[derive(Debug, Clone)]
pub struct PostgresAuthState {
    connection: DatabaseConnection,
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
    hard_cap: SignedDuration,
}

impl PostgresAuthState {
    /// A store over `connection` on `clock` with the sliding window `ttl` and the default hard
    /// cap.
    pub fn new(connection: DatabaseConnection, clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self {
            connection,
            clock,
            ttl,
            hard_cap: DEFAULT_SESSION_HARD_CAP,
        }
    }

    /// The same store with its hard cap set to `hard_cap`.
    #[must_use]
    pub fn with_hard_cap(mut self, hard_cap: SignedDuration) -> Self {
        self.hard_cap = hard_cap;
        self
    }

    /// Write `record` until `expires_at`, replacing whatever the id held.
    async fn store(
        &self,
        record: SessionRecord,
        expires_at: Timestamp,
        verb: &'static str,
    ) -> Result<(), StoreError> {
        self.connection
            .execute_raw(statement(
                "INSERT INTO auth_sessions (session_id, user_id, created_at, authenticated_at, \
                   signed_in_at, last_active_at, user_agent, ip_address, cohort_hash, device_id, \
                   expires_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
                 ON CONFLICT (session_id) DO UPDATE SET \
                   user_id = EXCLUDED.user_id, created_at = EXCLUDED.created_at, \
                   authenticated_at = EXCLUDED.authenticated_at, \
                   signed_in_at = EXCLUDED.signed_in_at, \
                   last_active_at = EXCLUDED.last_active_at, user_agent = EXCLUDED.user_agent, \
                   ip_address = EXCLUDED.ip_address, cohort_hash = EXCLUDED.cohort_hash, \
                   device_id = EXCLUDED.device_id, expires_at = EXCLUDED.expires_at",
                [
                    Value::from(record.session_id.as_str().to_owned()),
                    Value::from(record.user_id.as_str().to_owned()),
                    Value::from(to_micros(record.created_at)),
                    Value::from(to_micros(record.authenticated_at)),
                    Value::from(to_micros(record.signed_in_at)),
                    Value::from(to_micros(record.last_active_at)),
                    Value::from(record.user_agent.clone()),
                    Value::from(record.ip_address.clone()),
                    Value::from(record.cohort_hash.clone()),
                    Value::from(record.device_id.map(|id| id.to_string())),
                    Value::from(to_micros(expires_at)),
                ],
            ))
            .await
            .map_err(PORT.failing("storing a session"))?;
        tracing::debug!(
            session_id = %record.session_id,
            user_id = %record.user_id,
            %expires_at,
            verb,
            "stored session"
        );
        Ok(())
    }

    /// Update one column of a live session and read the record back.
    async fn update_live(
        &self,
        session: &SessionId,
        assignment: &'static str,
        at: Timestamp,
        doing: &'static str,
    ) -> Result<Option<SessionRecord>, StoreError> {
        let now = self.clock.now();
        let row = self
            .connection
            .query_one_raw(statement(
                &format!(
                    "UPDATE auth_sessions SET {assignment} = $3 \
                     WHERE session_id = $1 AND expires_at > $2 RETURNING {COLUMNS}"
                ),
                [
                    Value::from(session.as_str().to_owned()),
                    Value::from(to_micros(now)),
                    Value::from(to_micros(at)),
                ],
            ))
            .await
            .map_err(PORT.failing(doing))?;
        row.as_ref().map(record_from).transpose()
    }
}

/// Read one `auth_sessions` row back.
fn record_from(row: &QueryResult) -> Result<SessionRecord, StoreError> {
    let device_id: Option<String> = column(PORT, row, "device_id")?;
    let device_id = device_id
        .map(|text| {
            Uuid::parse_str(&text)
                .map_err(|_| PORT.undecodable("`device_id` is not a UUID".to_owned()))
        })
        .transpose()?;
    Ok(SessionRecord {
        session_id: SessionId::new(column::<String>(PORT, row, "session_id")?),
        user_id: UserId::new(column::<String>(PORT, row, "user_id")?),
        created_at: instant(PORT, row, "created_at")?,
        authenticated_at: instant(PORT, row, "authenticated_at")?,
        signed_in_at: instant(PORT, row, "signed_in_at")?,
        last_active_at: instant(PORT, row, "last_active_at")?,
        user_agent: column(PORT, row, "user_agent")?,
        ip_address: column(PORT, row, "ip_address")?,
        cohort_hash: column(PORT, row, "cohort_hash")?,
        device_id,
    })
}

/// The live records among rows a `DELETE … RETURNING` removed, oldest first.
fn live_records(rows: &[QueryResult], now: Timestamp) -> Result<Vec<SessionRecord>, StoreError> {
    let mut live = Vec::with_capacity(rows.len());
    for row in rows {
        if instant(PORT, row, "expires_at")? > now {
            live.push(record_from(row)?);
        }
    }
    live.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    Ok(live)
}

impl AuthStateStore for PostgresAuthState {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn hard_cap(&self) -> SignedDuration {
        self.hard_cap
    }

    fn open_session(&self, record: SessionRecord) -> StoreFuture<'_, ()> {
        let expires_at = self.expires_at(&record, self.clock.now());
        Box::pin(self.store(record, expires_at, "opened"))
    }

    fn restore_session(&self, record: SessionRecord) -> StoreFuture<'_, ()> {
        let expires_at = self.expires_at(&record, record.created_at);
        Box::pin(self.store(record, expires_at, "restored"))
    }

    fn read_session<'a>(
        &'a self,
        session: &'a SessionId,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            let row = self
                .connection
                .query_one_raw(statement(
                    &format!(
                        "SELECT {COLUMNS} FROM auth_sessions \
                         WHERE session_id = $1 AND expires_at > $2"
                    ),
                    [
                        Value::from(session.as_str().to_owned()),
                        Value::from(to_micros(now)),
                    ],
                ))
                .await
                .map_err(PORT.failing("reading a session"))?;
            let found = row.as_ref().map(record_from).transpose()?;
            tracing::trace!(%session, hit = found.is_some(), "read session");
            Ok(found)
        })
    }

    fn touch_session<'a>(
        &'a self,
        session: &'a SessionId,
        last_active_at: Timestamp,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        Box::pin(async move {
            let touched = self
                .update_live(
                    session,
                    "last_active_at",
                    last_active_at,
                    "touching a session",
                )
                .await?;
            tracing::trace!(%session, hit = touched.is_some(), "touched session");
            Ok(touched)
        })
    }

    fn mark_authenticated<'a>(
        &'a self,
        session: &'a SessionId,
        at: Timestamp,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        Box::pin(async move {
            let marked = self
                .update_live(
                    session,
                    "authenticated_at",
                    at,
                    "re-authenticating a session",
                )
                .await?;
            if marked.is_some() {
                tracing::info!(%session, "a session re-authenticated");
            } else {
                tracing::trace!(%session, "re-authentication found no live session");
            }
            Ok(marked)
        })
    }

    fn close_session<'a>(
        &'a self,
        session: &'a SessionId,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            let rows = self
                .connection
                .query_all_raw(statement(
                    &format!("DELETE FROM auth_sessions WHERE session_id = $1 RETURNING {COLUMNS}"),
                    [Value::from(session.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("closing a session"))?;
            let closed = live_records(&rows, now)?.into_iter().next();
            if let Some(record) = &closed {
                tracing::info!(%session, user_id = %record.user_id, "closed session");
            } else {
                tracing::debug!(%session, "close found no live session");
            }
            Ok(closed)
        })
    }

    fn sessions_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<SessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            let rows = self
                .connection
                .query_all_raw(statement(
                    &format!(
                        "SELECT {COLUMNS} FROM auth_sessions \
                         WHERE user_id = $1 AND expires_at > $2 \
                         ORDER BY created_at, session_id COLLATE \"C\""
                    ),
                    [
                        Value::from(user.as_str().to_owned()),
                        Value::from(to_micros(now)),
                    ],
                ))
                .await
                .map_err(PORT.failing("listing a user's sessions"))?;
            let found = rows
                .iter()
                .map(record_from)
                .collect::<Result<Vec<_>, _>>()?;
            tracing::trace!(%user, count = found.len(), "listed sessions for user");
            Ok(found)
        })
    }

    fn close_all_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<SessionRecord>> {
        Box::pin(async move {
            let now = self.clock.now();
            // One statement: the rows it returns are the rows it removed, so the count a
            // revoke-all reports is exactly what went away. Expired rows go with them and are
            // not counted, as the double's purge-then-resolve does.
            let rows = self
                .connection
                .query_all_raw(statement(
                    &format!("DELETE FROM auth_sessions WHERE user_id = $1 RETURNING {COLUMNS}"),
                    [Value::from(user.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("revoking every session of a user"))?;
            let removed = live_records(&rows, now)?;
            tracing::info!(%user, revoked = removed.len(), "revoked every session for user");
            Ok(removed)
        })
    }
}
