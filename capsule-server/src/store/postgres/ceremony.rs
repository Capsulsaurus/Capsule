//! The ceremony stores on PostgreSQL: the revoke-all challenge, device enrollment and its relay
//! channel, and the two OIDC ceremonies.
//!
//! Every consume-once read is a `DELETE … RETURNING`: the read *is* the removal, so a record is
//! burned on every attempt whether or not it was still live, and two racing consumers cannot
//! both get it — the second `DELETE` finds the row already gone.

use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};
use sea_orm::{ConnectionTrait, DatabaseConnection, QueryResult, TransactionTrait, Value};

use super::{column, duration_micros, instant, statement};
use crate::postgres::error::Port;
use crate::postgres::time::to_micros;
use crate::store::ceremony::{
    ChallengeStore, ChannelStore, DEVICE_SLOW_DOWN_STEP, DevicePollSlot, Direction, DrainOutcome,
    EnrollmentStore, OidcAuthorizationStore, OidcDeviceStore, PendingAuthorization,
    PendingDeviceAuthorization, PendingEnrollment, RelayChannel, RelayOutcome, RelayPayload,
    RevokeAllChallenge,
};
use crate::store::{
    ChallengeToken, ChannelId, Clock, DeviceCode, EnrollmentCode, OidcDeviceHandle, OidcNonce,
    OidcState, PkceVerifier, StoreError, StoreFuture, UserId, deadline,
};

/// Whether the row's `expires_at` is still ahead of `now`.
fn is_live(port: Port, row: &QueryResult, now: Timestamp) -> Result<bool, StoreError> {
    Ok(instant(port, row, "expires_at")? > now)
}

// ===========================================================================================
// Revoke-all challenge
// ===========================================================================================

const CHALLENGES: Port = Port {
    store: "revoke-all challenges",
    record: "RevokeAllChallenge",
};

/// The PostgreSQL [`ChallengeStore`].
#[derive(Debug, Clone)]
pub struct PostgresChallenges {
    connection: DatabaseConnection,
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
}

impl PostgresChallenges {
    /// A store over `connection` on `clock`, each challenge living `ttl`.
    pub fn new(connection: DatabaseConnection, clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self {
            connection,
            clock,
            ttl,
        }
    }
}

impl ChallengeStore for PostgresChallenges {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn issue<'a>(
        &'a self,
        token: &'a ChallengeToken,
        record: RevokeAllChallenge,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let expires_at = deadline(self.clock.now(), self.ttl);
            self.connection
                .execute_raw(statement(
                    "INSERT INTO revoke_all_challenges (token, user_id, issued_at, expires_at) \
                     VALUES ($1, $2, $3, $4) \
                     ON CONFLICT (token) DO UPDATE SET user_id = EXCLUDED.user_id, \
                       issued_at = EXCLUDED.issued_at, expires_at = EXCLUDED.expires_at",
                    [
                        Value::from(token.as_str().to_owned()),
                        Value::from(record.user_id.as_str().to_owned()),
                        Value::from(to_micros(record.issued_at)),
                        Value::from(to_micros(expires_at)),
                    ],
                ))
                .await
                .map_err(CHALLENGES.failing("issuing a revoke-all challenge"))?;
            tracing::debug!(user_id = %record.user_id, "issued a revoke-all challenge");
            Ok(())
        })
    }

    fn consume<'a>(
        &'a self,
        token: &'a ChallengeToken,
    ) -> StoreFuture<'a, Option<RevokeAllChallenge>> {
        Box::pin(async move {
            let now = self.clock.now();
            let row = self
                .connection
                .query_one_raw(statement(
                    "DELETE FROM revoke_all_challenges WHERE token = $1 \
                     RETURNING user_id, issued_at, expires_at",
                    [Value::from(token.as_str().to_owned())],
                ))
                .await
                .map_err(CHALLENGES.failing("consuming a revoke-all challenge"))?;
            let Some(row) = row.filter(|row| is_live(CHALLENGES, row, now).unwrap_or(false)) else {
                tracing::debug!("revoke-all challenge unknown, expired or already consumed");
                return Ok(None);
            };
            Ok(Some(RevokeAllChallenge {
                user_id: UserId::new(column::<String>(CHALLENGES, &row, "user_id")?),
                issued_at: instant(CHALLENGES, &row, "issued_at")?,
            }))
        })
    }
}

// ===========================================================================================
// Device enrollment
// ===========================================================================================

const ENROLLMENTS: Port = Port {
    store: "enrollments",
    record: "PendingEnrollment",
};

/// The PostgreSQL [`EnrollmentStore`].
///
/// One row per **spelling**: a pending enrollment is written under its code and its text
/// fallback, as one fact, so either spelling finds it and redeeming either removes both.
#[derive(Debug, Clone)]
pub struct PostgresEnrollments {
    connection: DatabaseConnection,
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
}

impl PostgresEnrollments {
    /// A store over `connection` on `clock`, each code living `ttl`.
    pub fn new(connection: DatabaseConnection, clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self {
            connection,
            clock,
            ttl,
        }
    }
}

impl EnrollmentStore for PostgresEnrollments {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn issue(&self, record: PendingEnrollment) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let expires_at = deadline(self.clock.now(), self.ttl);
            let mut spellings = vec![record.code.as_str(), record.text_fallback.as_str()];
            // One row per distinct spelling: an upsert naming one key twice is refused.
            spellings.dedup();
            for spelling in spellings {
                self.connection
                    .execute_raw(statement(
                        "INSERT INTO enrollment_codes \
                           (spelling, user_id, code, text_fallback, issued_at, expires_at) \
                         VALUES ($1, $2, $3, $4, $5, $6) \
                         ON CONFLICT (spelling) DO UPDATE SET user_id = EXCLUDED.user_id, \
                           code = EXCLUDED.code, text_fallback = EXCLUDED.text_fallback, \
                           issued_at = EXCLUDED.issued_at, expires_at = EXCLUDED.expires_at",
                        [
                            Value::from(spelling.to_owned()),
                            Value::from(record.user_id.as_str().to_owned()),
                            Value::from(record.code.as_str().to_owned()),
                            Value::from(record.text_fallback.as_str().to_owned()),
                            Value::from(to_micros(record.issued_at)),
                            Value::from(to_micros(expires_at)),
                        ],
                    ))
                    .await
                    .map_err(ENROLLMENTS.failing("issuing an enrollment code"))?;
            }
            tracing::info!(user_id = %record.user_id, "issued enrollment code");
            Ok(())
        })
    }

    fn is_taken<'a>(&'a self, code: &'a EnrollmentCode) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let row = self
                .connection
                .query_one_raw(statement(
                    "SELECT 1 AS taken FROM enrollment_codes \
                     WHERE spelling = $1 AND expires_at > $2",
                    [
                        Value::from(code.as_str().to_owned()),
                        Value::from(to_micros(self.clock.now())),
                    ],
                ))
                .await
                .map_err(ENROLLMENTS.failing("checking an enrollment code collision"))?;
            tracing::trace!(taken = row.is_some(), "checked enrollment code collision");
            Ok(row.is_some())
        })
    }

    fn redeem<'a>(
        &'a self,
        code: &'a EnrollmentCode,
    ) -> StoreFuture<'a, Option<PendingEnrollment>> {
        Box::pin(async move {
            let now = self.clock.now();
            // Whichever spelling was presented, both die, in one statement.
            let rows = self
                .connection
                .query_all_raw(statement(
                    "WITH hit AS (SELECT code, text_fallback FROM enrollment_codes \
                                  WHERE spelling = $1) \
                     DELETE FROM enrollment_codes AS e USING hit \
                     WHERE e.spelling IN ($1, hit.code, hit.text_fallback) \
                     RETURNING e.spelling, e.user_id, e.code, e.text_fallback, \
                               e.issued_at, e.expires_at",
                    [Value::from(code.as_str().to_owned())],
                ))
                .await
                .map_err(ENROLLMENTS.failing("redeeming an enrollment code"))?;
            let mut presented = None;
            for row in &rows {
                if column::<String>(ENROLLMENTS, row, "spelling")? == code.as_str() {
                    presented = Some(row);
                }
            }
            let Some(row) = presented.filter(|row| is_live(ENROLLMENTS, row, now).unwrap_or(false))
            else {
                tracing::debug!("enrollment code unknown, expired or already redeemed");
                return Ok(None);
            };
            let record = PendingEnrollment {
                user_id: UserId::new(column::<String>(ENROLLMENTS, row, "user_id")?),
                code: EnrollmentCode::new(column::<String>(ENROLLMENTS, row, "code")?),
                text_fallback: EnrollmentCode::new(column::<String>(
                    ENROLLMENTS,
                    row,
                    "text_fallback",
                )?),
                issued_at: instant(ENROLLMENTS, row, "issued_at")?,
            };
            tracing::info!(user_id = %record.user_id, "redeemed enrollment code");
            Ok(Some(record))
        })
    }
}

// ===========================================================================================
// Enrollment relay channel
// ===========================================================================================

const CHANNELS: Port = Port {
    store: "enrollment channels",
    record: "RelayChannel",
};

/// The PostgreSQL [`ChannelStore`].
///
/// The channel is a row in `enrollment_channels`; each direction's mailbox is the rows of
/// `enrollment_mailbox` naming it, in insertion order, deleted with the channel by the foreign
/// key's cascade. Appending and draining lock the channel row `FOR SHARE` first, so neither can
/// interleave with the channel closing or expiring between its liveness check and its write.
#[derive(Debug, Clone)]
pub struct PostgresChannels {
    connection: DatabaseConnection,
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
}

impl PostgresChannels {
    /// A store over `connection` on `clock`, each channel living `ttl`.
    pub fn new(connection: DatabaseConnection, clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self {
            connection,
            clock,
            ttl,
        }
    }
}

/// Lock `channel` against closing for the rest of `transaction`, if it is live at `now`.
async fn hold_live_channel(
    transaction: &impl ConnectionTrait,
    channel: &ChannelId,
    now: Timestamp,
) -> Result<bool, StoreError> {
    let row = transaction
        .query_one_raw(statement(
            "SELECT 1 AS live FROM enrollment_channels \
             WHERE channel_id = $1 AND expires_at > $2 FOR SHARE",
            [
                Value::from(channel.as_str().to_owned()),
                Value::from(to_micros(now)),
            ],
        ))
        .await
        .map_err(CHANNELS.failing("locking a relay channel"))?;
    Ok(row.is_some())
}

impl ChannelStore for PostgresChannels {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn open<'a>(&'a self, channel: &'a ChannelId, record: RelayChannel) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let expires_at = deadline(self.clock.now(), self.ttl);
            // Re-opening an id must not resurrect an earlier ceremony's undelivered payloads, so
            // the old mailbox goes in the same statement as the new channel arrives.
            self.connection
                .execute_raw(statement(
                    "WITH emptied AS (DELETE FROM enrollment_mailbox WHERE channel_id = $1) \
                     INSERT INTO enrollment_channels \
                       (channel_id, initiator_user_id, opened_at, expires_at) \
                     VALUES ($1, $2, $3, $4) \
                     ON CONFLICT (channel_id) DO UPDATE SET \
                       initiator_user_id = EXCLUDED.initiator_user_id, \
                       opened_at = EXCLUDED.opened_at, expires_at = EXCLUDED.expires_at",
                    [
                        Value::from(channel.as_str().to_owned()),
                        Value::from(record.initiator_user_id.as_str().to_owned()),
                        Value::from(to_micros(record.opened_at)),
                        Value::from(to_micros(expires_at)),
                    ],
                ))
                .await
                .map_err(CHANNELS.failing("opening a relay channel"))?;
            tracing::debug!(initiator = %record.initiator_user_id, "opened relay channel");
            Ok(())
        })
    }

    fn lookup<'a>(&'a self, channel: &'a ChannelId) -> StoreFuture<'a, Option<RelayChannel>> {
        Box::pin(async move {
            let row = self
                .connection
                .query_one_raw(statement(
                    "SELECT initiator_user_id, opened_at FROM enrollment_channels \
                     WHERE channel_id = $1 AND expires_at > $2",
                    [
                        Value::from(channel.as_str().to_owned()),
                        Value::from(to_micros(self.clock.now())),
                    ],
                ))
                .await
                .map_err(CHANNELS.failing("looking up a relay channel"))?;
            row.map(|row| {
                Ok(RelayChannel {
                    initiator_user_id: UserId::new(column::<String>(
                        CHANNELS,
                        &row,
                        "initiator_user_id",
                    )?),
                    opened_at: instant(CHANNELS, &row, "opened_at")?,
                })
            })
            .transpose()
        })
    }

    fn enqueue<'a>(
        &'a self,
        channel: &'a ChannelId,
        direction: Direction,
        payload: RelayPayload,
    ) -> StoreFuture<'a, RelayOutcome> {
        Box::pin(async move {
            let now = self.clock.now();
            let transaction = self
                .connection
                .begin()
                .await
                .map_err(CHANNELS.failing("beginning a relay append"))?;
            if !hold_live_channel(&transaction, channel, now).await? {
                tracing::debug!("relay append found no live channel");
                return Ok(RelayOutcome::NoChannel);
            }
            let depth = transaction
                .query_one_raw(statement(
                    "WITH appended AS (INSERT INTO enrollment_mailbox \
                                         (channel_id, direction, payload) \
                                       VALUES ($1, $2, $3) RETURNING 1) \
                     SELECT (SELECT count(*) FROM enrollment_mailbox \
                             WHERE channel_id = $1 AND direction = $2) \
                            + (SELECT count(*) FROM appended) AS depth",
                    [
                        Value::from(channel.as_str().to_owned()),
                        Value::from(direction.as_str()),
                        Value::from(payload.as_str().to_owned()),
                    ],
                ))
                .await
                .map_err(CHANNELS.failing("appending to a relay mailbox"))?
                .ok_or_else(|| StoreError::Rejected {
                    store: CHANNELS.store,
                    detail: "the relay append returned no depth".to_owned(),
                })?;
            let depth: i64 = column(CHANNELS, &depth, "depth")?;
            transaction
                .commit()
                .await
                .map_err(CHANNELS.failing("committing a relay append"))?;
            let depth = usize::try_from(depth).unwrap_or(usize::MAX);
            tracing::debug!(
                direction = direction.as_str(),
                depth,
                "enqueued relay payload"
            );
            Ok(RelayOutcome::Enqueued { depth })
        })
    }

    fn drain<'a>(
        &'a self,
        channel: &'a ChannelId,
        direction: Direction,
    ) -> StoreFuture<'a, DrainOutcome> {
        Box::pin(async move {
            let now = self.clock.now();
            let transaction = self
                .connection
                .begin()
                .await
                .map_err(CHANNELS.failing("beginning a relay drain"))?;
            if !hold_live_channel(&transaction, channel, now).await? {
                tracing::debug!("relay drain found no live channel");
                return Ok(DrainOutcome::NoChannel);
            }
            let rows = transaction
                .query_all_raw(statement(
                    "DELETE FROM enrollment_mailbox WHERE channel_id = $1 AND direction = $2 \
                     RETURNING seq, payload",
                    [
                        Value::from(channel.as_str().to_owned()),
                        Value::from(direction.as_str()),
                    ],
                ))
                .await
                .map_err(CHANNELS.failing("draining a relay mailbox"))?;
            transaction
                .commit()
                .await
                .map_err(CHANNELS.failing("committing a relay drain"))?;
            let mut items = Vec::with_capacity(rows.len());
            for row in &rows {
                let seq: i64 = column(CHANNELS, row, "seq")?;
                let payload: String = column(CHANNELS, row, "payload")?;
                items.push((seq, RelayPayload::new(payload)));
            }
            // `RETURNING` promises no order; insertion order is the identity's order.
            items.sort_by_key(|(seq, _)| *seq);
            let items: Vec<RelayPayload> = items.into_iter().map(|(_, payload)| payload).collect();
            tracing::debug!(
                direction = direction.as_str(),
                count = items.len(),
                "drained relay mailbox"
            );
            Ok(DrainOutcome::Drained(items))
        })
    }

    fn close<'a>(&'a self, channel: &'a ChannelId) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let now = self.clock.now();
            let row = self
                .connection
                .query_one_raw(statement(
                    "DELETE FROM enrollment_channels WHERE channel_id = $1 RETURNING expires_at",
                    [Value::from(channel.as_str().to_owned())],
                ))
                .await
                .map_err(CHANNELS.failing("closing a relay channel"))?;
            let was_live = match row {
                Some(row) => is_live(CHANNELS, &row, now)?,
                None => false,
            };
            tracing::debug!(was_live, "closed relay channel");
            Ok(was_live)
        })
    }
}

// ===========================================================================================
// OIDC authorization
// ===========================================================================================

const OIDC_AUTHORIZATIONS: Port = Port {
    store: "oidc authorizations",
    record: "PendingAuthorization",
};

/// The PostgreSQL [`OidcAuthorizationStore`].
#[derive(Debug, Clone)]
pub struct PostgresOidcAuthorizations {
    connection: DatabaseConnection,
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
}

impl PostgresOidcAuthorizations {
    /// A store over `connection` on `clock`, each ceremony living `ttl`.
    pub fn new(connection: DatabaseConnection, clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self {
            connection,
            clock,
            ttl,
        }
    }
}

impl OidcAuthorizationStore for PostgresOidcAuthorizations {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn begin<'a>(
        &'a self,
        state: &'a OidcState,
        record: PendingAuthorization,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let expires_at = deadline(self.clock.now(), self.ttl);
            self.connection
                .execute_raw(statement(
                    "INSERT INTO oidc_authorizations (state, nonce, verifier, redirect_uri, \
                       binding_challenge, issued_at, expires_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7) \
                     ON CONFLICT (state) DO UPDATE SET nonce = EXCLUDED.nonce, \
                       verifier = EXCLUDED.verifier, redirect_uri = EXCLUDED.redirect_uri, \
                       binding_challenge = EXCLUDED.binding_challenge, \
                       issued_at = EXCLUDED.issued_at, expires_at = EXCLUDED.expires_at",
                    [
                        Value::from(state.as_str().to_owned()),
                        Value::from(record.nonce.as_str().to_owned()),
                        Value::from(record.verifier.as_str().to_owned()),
                        Value::from(record.redirect_uri.clone()),
                        Value::from(record.binding_challenge.clone()),
                        Value::from(to_micros(record.issued_at)),
                        Value::from(to_micros(expires_at)),
                    ],
                ))
                .await
                .map_err(OIDC_AUTHORIZATIONS.failing("recording an OIDC authorization"))?;
            tracing::debug!("recorded a pending OIDC authorization");
            Ok(())
        })
    }

    fn consume<'a>(
        &'a self,
        state: &'a OidcState,
    ) -> StoreFuture<'a, Option<PendingAuthorization>> {
        Box::pin(async move {
            let now = self.clock.now();
            let row = self
                .connection
                .query_one_raw(statement(
                    "DELETE FROM oidc_authorizations WHERE state = $1 \
                     RETURNING nonce, verifier, redirect_uri, binding_challenge, issued_at, \
                               expires_at",
                    [Value::from(state.as_str().to_owned())],
                ))
                .await
                .map_err(OIDC_AUTHORIZATIONS.failing("consuming an OIDC authorization"))?;
            let Some(row) =
                row.filter(|row| is_live(OIDC_AUTHORIZATIONS, row, now).unwrap_or(false))
            else {
                tracing::debug!("OIDC authorization unknown, expired or already consumed");
                return Ok(None);
            };
            Ok(Some(PendingAuthorization {
                nonce: OidcNonce::new(column::<String>(OIDC_AUTHORIZATIONS, &row, "nonce")?),
                verifier: PkceVerifier::new(column::<String>(
                    OIDC_AUTHORIZATIONS,
                    &row,
                    "verifier",
                )?),
                redirect_uri: column(OIDC_AUTHORIZATIONS, &row, "redirect_uri")?,
                binding_challenge: column(OIDC_AUTHORIZATIONS, &row, "binding_challenge")?,
                issued_at: instant(OIDC_AUTHORIZATIONS, &row, "issued_at")?,
            }))
        })
    }
}

// ===========================================================================================
// OIDC device authorization
// ===========================================================================================

const OIDC_DEVICES: Port = Port {
    store: "oidc devices",
    record: "PendingDeviceAuthorization",
};

/// The PostgreSQL [`OidcDeviceStore`].
///
/// The poll schedule is two columns, `polled_at` and `interval_micros`. Claiming a slot reads and
/// moves them under the row's lock in one transaction, so two racing polls cannot both find it
/// free.
#[derive(Debug, Clone)]
pub struct PostgresOidcDevices {
    connection: DatabaseConnection,
    clock: Arc<dyn Clock>,
    ttl: SignedDuration,
}

impl PostgresOidcDevices {
    /// A store over `connection` on `clock`, each ceremony living `ttl`.
    pub fn new(connection: DatabaseConnection, clock: Arc<dyn Clock>, ttl: SignedDuration) -> Self {
        Self {
            connection,
            clock,
            ttl,
        }
    }
}

const DEVICE_COLUMNS: &str = "device_code, binding_challenge, interval_micros, polled_at, \
                              issued_at, expires_at";

fn device_from(row: &QueryResult) -> Result<PendingDeviceAuthorization, StoreError> {
    let interval: i64 = column(OIDC_DEVICES, row, "interval_micros")?;
    Ok(PendingDeviceAuthorization {
        device_code: DeviceCode::new(column::<String>(OIDC_DEVICES, row, "device_code")?),
        binding_challenge: column(OIDC_DEVICES, row, "binding_challenge")?,
        interval: SignedDuration::from_micros(interval),
        issued_at: instant(OIDC_DEVICES, row, "issued_at")?,
    })
}

impl OidcDeviceStore for PostgresOidcDevices {
    fn ttl(&self) -> SignedDuration {
        self.ttl
    }

    fn begin<'a>(
        &'a self,
        handle: &'a OidcDeviceHandle,
        record: PendingDeviceAuthorization,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let now = self.clock.now();
            let expires_at = deadline(now, self.ttl);
            self.connection
                .execute_raw(statement(
                    "INSERT INTO oidc_device_authorizations (handle, device_code, \
                       binding_challenge, interval_micros, polled_at, issued_at, expires_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7) \
                     ON CONFLICT (handle) DO UPDATE SET device_code = EXCLUDED.device_code, \
                       binding_challenge = EXCLUDED.binding_challenge, \
                       interval_micros = EXCLUDED.interval_micros, \
                       polled_at = EXCLUDED.polled_at, issued_at = EXCLUDED.issued_at, \
                       expires_at = EXCLUDED.expires_at",
                    [
                        Value::from(handle.as_str().to_owned()),
                        Value::from(record.device_code.as_str().to_owned()),
                        Value::from(record.binding_challenge.clone()),
                        Value::from(duration_micros(record.interval).max(0)),
                        Value::from(to_micros(now)),
                        Value::from(to_micros(record.issued_at)),
                        Value::from(to_micros(expires_at)),
                    ],
                ))
                .await
                .map_err(OIDC_DEVICES.failing("recording an OIDC device authorization"))?;
            tracing::debug!("recorded a pending OIDC device authorization");
            Ok(())
        })
    }

    fn claim<'a>(&'a self, handle: &'a OidcDeviceHandle) -> StoreFuture<'a, DevicePollSlot> {
        Box::pin(async move {
            let now = self.clock.now();
            let transaction = self
                .connection
                .begin()
                .await
                .map_err(OIDC_DEVICES.failing("beginning a device poll claim"))?;
            let row = transaction
                .query_one_raw(statement(
                    &format!(
                        "SELECT {DEVICE_COLUMNS} FROM oidc_device_authorizations \
                         WHERE handle = $1 AND expires_at > $2 FOR UPDATE"
                    ),
                    [
                        Value::from(handle.as_str().to_owned()),
                        Value::from(to_micros(now)),
                    ],
                ))
                .await
                .map_err(OIDC_DEVICES.failing("reading a device poll slot"))?;
            let Some(row) = row else {
                tracing::debug!("claimed a poll on no live OIDC device authorization");
                return Ok(DevicePollSlot::Unknown);
            };
            let mut record = device_from(&row)?;
            let polled_at = instant(OIDC_DEVICES, &row, "polled_at")?;
            let too_soon = now < deadline(polled_at, record.interval);
            if too_soon {
                record.interval += DEVICE_SLOW_DOWN_STEP;
            }
            transaction
                .execute_raw(statement(
                    "UPDATE oidc_device_authorizations SET polled_at = $2, interval_micros = $3 \
                     WHERE handle = $1",
                    [
                        Value::from(handle.as_str().to_owned()),
                        Value::from(to_micros(now)),
                        Value::from(duration_micros(record.interval).max(0)),
                    ],
                ))
                .await
                .map_err(OIDC_DEVICES.failing("moving a device poll slot"))?;
            transaction
                .commit()
                .await
                .map_err(OIDC_DEVICES.failing("committing a device poll claim"))?;
            if too_soon {
                tracing::debug!(
                    interval_secs = record.interval.as_secs(),
                    "an OIDC device authorization was polled too soon"
                );
                return Ok(DevicePollSlot::TooSoon {
                    interval: record.interval,
                });
            }
            tracing::debug!("claimed a poll on an OIDC device authorization");
            Ok(DevicePollSlot::Due(record))
        })
    }

    fn slow_down<'a>(
        &'a self,
        handle: &'a OidcDeviceHandle,
    ) -> StoreFuture<'a, Option<SignedDuration>> {
        Box::pin(async move {
            let row = self
                .connection
                .query_one_raw(statement(
                    "UPDATE oidc_device_authorizations \
                     SET interval_micros = interval_micros + $3 \
                     WHERE handle = $1 AND expires_at > $2 RETURNING interval_micros",
                    [
                        Value::from(handle.as_str().to_owned()),
                        Value::from(to_micros(self.clock.now())),
                        Value::from(duration_micros(DEVICE_SLOW_DOWN_STEP)),
                    ],
                ))
                .await
                .map_err(OIDC_DEVICES.failing("raising a device poll interval"))?;
            let raised = row
                .map(|row| column::<i64>(OIDC_DEVICES, &row, "interval_micros"))
                .transpose()?
                .map(SignedDuration::from_micros);
            tracing::debug!(
                hit = raised.is_some(),
                "raised an OIDC device authorization's interval"
            );
            Ok(raised)
        })
    }

    fn finish<'a>(
        &'a self,
        handle: &'a OidcDeviceHandle,
    ) -> StoreFuture<'a, Option<PendingDeviceAuthorization>> {
        Box::pin(async move {
            let now = self.clock.now();
            let row = self
                .connection
                .query_one_raw(statement(
                    &format!(
                        "DELETE FROM oidc_device_authorizations WHERE handle = $1 \
                         RETURNING {DEVICE_COLUMNS}"
                    ),
                    [Value::from(handle.as_str().to_owned())],
                ))
                .await
                .map_err(OIDC_DEVICES.failing("finishing an OIDC device authorization"))?;
            let taken = row
                .filter(|row| is_live(OIDC_DEVICES, row, now).unwrap_or(false))
                .map(|row| device_from(&row))
                .transpose()?;
            tracing::debug!(
                hit = taken.is_some(),
                "finished an OIDC device authorization"
            );
            Ok(taken)
        })
    }
}
