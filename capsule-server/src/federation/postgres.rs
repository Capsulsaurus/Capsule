//! [`PostgresCapabilities`] and [`PostgresPeers`] — the durable federation stores (`S-E2`).
//!
//! # Two tables behind one port, written in one transaction
//!
//! `federation_capabilities` holds every capability this server minted; `federation_revoked_jti`
//! is the list `/.well-known/capsule/revoked-jti` publishes. They are one port because "is this
//! `jti` revoked" must have one answer: revoking an issued capability sets its `revoked_at`
//! **and** publishes its `jti`, and doing one without the other is what a transaction is for.
//!
//! They are two *tables* because a revocation is also accepted for a `jti` no record backs — an
//! operator cutting a token named in a peer's report, or one that predates this store — so the
//! list cannot be a view over the records.
//!
//! # Why the lock is an advisory lock
//!
//! `revoke_issued` and `refresh` each read a record, decide, and write two tables. The row they
//! would `SELECT … FOR UPDATE` exists for `revoke_issued` but not for the successor `refresh`
//! inserts, and two concurrent refreshes of one predecessor must not both issue. A
//! transaction-scoped advisory lock keyed on the predecessor's `jti` serialises both, is
//! released by the commit or the rollback, and needs no row to exist — the same choice
//! [`PostgresMembership`](crate::membership::postgres::PostgresMembership) makes for the same
//! reason. Every transaction holds **one** such lock: `revoke_issued` follows a refresh chain
//! without locks and locks only its tail, the one link that can still be live, so a hash
//! collision between two keys can only serialise two operations, never deadlock them.
//!
//! # Pruning on read
//!
//! `published` deletes every entry past its expiry before it reads, exactly as the in-memory
//! adapter retains, so a list nobody fetches does not grow forever holding entries that already
//! mean nothing. The delete is the read's own statement rather than a background job: the record
//! is fetched often and pruned rarely, and a sweeper would be a second thing to run.

use jiff::Timestamp;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait, Value,
};

use super::PeerId;
use super::capability::Scope;
use super::peers::{BlockOutcome, PeerRecord, PeerStore, UnblockOutcome};
use super::store::{
    CapabilityFilter, CapabilityRecord, CapabilityStore, RefreshOutcome, RevokeOutcome,
};
use crate::discovery::revocation::{
    MAX_TOKEN_TTL, PublishedRevocations, RevocationError, RevocationList, RevokeFuture,
    RevokedToken,
};
use crate::federation::store::admissible;
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};
use crate::store::{AlbumId, Clock, StoreError, StoreFuture, UserId};

/// Which port is speaking, for every error the capability adapter raises.
const CAPABILITIES: Port = Port {
    store: "capabilities",
    record: "CapabilityRecord",
};

/// Which port is speaking, for every error the peer adapter raises.
const PEERS: Port = Port {
    store: "peers",
    record: "PeerRecord",
};

/// The columns every capability read selects, in the order [`record_from`] decodes them.
const CAPABILITY_COLUMNS: &str = "jti, album_id, peer_id, member_id, scope, granted_epoch, \
                                  min_protocol_version, issued_at, expires_at, not_after, \
                                  revoked_at, refreshed_to";

/// An epoch as the column holds it.
fn epoch_to_column(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::Rejected {
        store: CAPABILITIES.store,
        detail: format!("{value} is past what a BIGINT column holds"),
    })
}

/// An instant as the port speaks it.
fn instant(port: Port, micros: i64) -> Result<Timestamp, StoreError> {
    from_micros(micros)
        .ok_or_else(|| port.undecodable(format!("{micros}µs is not a representable instant")))
}

/// Decode one row of [`CAPABILITY_COLUMNS`].
fn record_from(row: &sea_orm::QueryResult) -> Result<CapabilityRecord, StoreError> {
    let failed = CAPABILITIES.failing("reading a capability");
    let jti: String = row.try_get("", "jti").map_err(&failed)?;
    let album_id: String = row.try_get("", "album_id").map_err(&failed)?;
    let peer_id: String = row.try_get("", "peer_id").map_err(&failed)?;
    let member_id: String = row.try_get("", "member_id").map_err(&failed)?;
    let scope: String = row.try_get("", "scope").map_err(&failed)?;
    let granted_epoch: i64 = row.try_get("", "granted_epoch").map_err(&failed)?;
    let min_protocol_version: String = row.try_get("", "min_protocol_version").map_err(&failed)?;
    let issued_at: i64 = row.try_get("", "issued_at").map_err(&failed)?;
    let expires_at: i64 = row.try_get("", "expires_at").map_err(&failed)?;
    let not_after: i64 = row.try_get("", "not_after").map_err(&failed)?;
    let revoked_at: Option<i64> = row.try_get("", "revoked_at").map_err(&failed)?;
    let refreshed_to: Option<String> = row.try_get("", "refreshed_to").map_err(&failed)?;
    Ok(CapabilityRecord {
        jti,
        album_id: AlbumId::new(album_id),
        peer_id: PeerId::new(peer_id),
        member: UserId::new(member_id),
        scope: Scope::from_token(&scope)
            .ok_or_else(|| CAPABILITIES.undecodable(format!("`{scope}` is not a scope")))?,
        granted_epoch: u64::try_from(granted_epoch)
            .map_err(|_| CAPABILITIES.undecodable(format!("{granted_epoch} is not an epoch")))?,
        min_protocol_version,
        issued_at: instant(CAPABILITIES, issued_at)?,
        expires_at: instant(CAPABILITIES, expires_at)?,
        not_after: instant(CAPABILITIES, not_after)?,
        revoked_at: revoked_at
            .map(|micros| instant(CAPABILITIES, micros))
            .transpose()?,
        refreshed_to,
    })
}

/// Begin a transaction, or say why not.
async fn begin(
    connection: &DatabaseConnection,
    port: Port,
) -> Result<DatabaseTransaction, StoreError> {
    connection
        .begin()
        .await
        .map_err(port.failing("opening a transaction"))
}

/// Commit, or say why not.
async fn commit(transaction: DatabaseTransaction, port: Port) -> Result<(), StoreError> {
    transaction
        .commit()
        .await
        .map_err(port.failing("committing a transaction"))
}

/// Take the transaction-scoped lock that serialises everything keyed on `key`.
async fn lock(transaction: &DatabaseTransaction, key: &str, port: Port) -> Result<(), StoreError> {
    transaction
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtext($1))",
            [Value::from(key.to_owned())],
        ))
        .await
        .map(|_| ())
        .map_err(port.failing("taking the federation lock"))
}

/// Read one capability under `connection`.
async fn record_of<C: ConnectionTrait>(
    connection: &C,
    jti: &str,
) -> Result<Option<CapabilityRecord>, StoreError> {
    let row = connection
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("SELECT {CAPABILITY_COLUMNS} FROM federation_capabilities WHERE jti = $1"),
            [Value::from(jti.to_owned())],
        ))
        .await
        .map_err(CAPABILITIES.failing("reading a capability"))?;
    row.as_ref().map(record_from).transpose()
}

/// Insert `record`, refusing a `jti` already recorded.
async fn insert<C: ConnectionTrait>(
    connection: &C,
    record: &CapabilityRecord,
) -> Result<(), StoreError> {
    let inserted = connection
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO federation_capabilities \
             (jti, album_id, peer_id, member_id, scope, granted_epoch, min_protocol_version, \
              issued_at, expires_at, not_after, revoked_at, refreshed_to) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NULL, NULL) \
             ON CONFLICT (jti) DO NOTHING",
            [
                Value::from(record.jti.clone()),
                Value::from(record.album_id.as_str().to_owned()),
                Value::from(record.peer_id.as_str().to_owned()),
                Value::from(record.member.as_str().to_owned()),
                Value::from(record.scope.as_str().to_owned()),
                Value::from(epoch_to_column(record.granted_epoch)?),
                Value::from(record.min_protocol_version.clone()),
                Value::from(to_micros(record.issued_at)),
                Value::from(to_micros(record.expires_at)),
                Value::from(to_micros(record.not_after)),
            ],
        ))
        .await
        .map_err(CAPABILITIES.failing("recording a capability"))?;
    if inserted.rows_affected() == 0 {
        return Err(StoreError::Rejected {
            store: CAPABILITIES.store,
            detail: format!("a capability with jti {} is already recorded", record.jti),
        });
    }
    Ok(())
}

/// Publish `jti` under `expires_at`, never shortening an entry already published.
async fn publish<C: ConnectionTrait>(
    connection: &C,
    jti: &str,
    expires_at: Timestamp,
    at: Timestamp,
) -> Result<(), StoreError> {
    connection
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO federation_revoked_jti (jti, expires_at, revoked_at) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (jti) DO UPDATE SET \
               expires_at = GREATEST(federation_revoked_jti.expires_at, EXCLUDED.expires_at)",
            [
                Value::from(jti.to_owned()),
                Value::from(to_micros(expires_at)),
                Value::from(to_micros(at)),
            ],
        ))
        .await
        .map(|_| ())
        .map_err(CAPABILITIES.failing("publishing a revocation"))?;
    Ok(())
}

/// Mark `jti` revoked at `at` if it is not already, and publish it under the record's own expiry.
///
/// The two halves in one call, so no path can do one without the other.
async fn revoke_and_publish<C: ConnectionTrait>(
    connection: &C,
    jti: &str,
    expires_at: Timestamp,
    at: Timestamp,
) -> Result<(), StoreError> {
    connection
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE federation_capabilities SET revoked_at = $2 \
             WHERE jti = $1 AND revoked_at IS NULL",
            [Value::from(jti.to_owned()), Value::from(to_micros(at))],
        ))
        .await
        .map_err(CAPABILITIES.failing("revoking a capability"))?;
    publish(connection, jti, expires_at, at).await
}

/// How many times a chain revocation starts over after a refresh moved the tail under it.
///
/// A peer would have to land a refresh in the gap between one statement and the next, every
/// time; running out is reported as `Unavailable` — nothing was written — rather than looped on.
const CHAIN_ATTEMPTS: usize = 8;

/// The longest refresh chain a revocation will follow.
///
/// A real chain cannot cycle — a successor's `jti` is new by construction — so this exists only
/// so that a corrupt one ends the recursion, and it is far beyond anything the refresh budget
/// lets a peer build inside the longest grant.
const CHAIN_DEPTH_LIMIT: i64 = 10_000_000;

/// The refresh chain from `jti` forward, oldest first, read in one statement and without locks.
///
/// Empty when nothing backs `jti`.
///
/// # Errors
///
/// Returns [`broken_chain`](super::store::broken_chain) when the last link names a successor
/// that is not recorded, when a link repeats, or when the chain runs past
/// [`CHAIN_DEPTH_LIMIT`].
async fn chain_of<C: ConnectionTrait>(
    connection: &C,
    jti: &str,
) -> Result<Vec<CapabilityRecord>, StoreError> {
    let rows = connection
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "WITH RECURSIVE chain(link, next, depth) AS ( \
                     SELECT jti, refreshed_to, 0::BIGINT FROM federation_capabilities \
                     WHERE jti = $1 \
                   UNION ALL \
                     SELECT c.jti, c.refreshed_to, chain.depth + 1 \
                     FROM federation_capabilities c JOIN chain ON c.jti = chain.next \
                     WHERE chain.depth < $2 \
                 ) \
                 SELECT {CAPABILITY_COLUMNS} FROM chain \
                 JOIN federation_capabilities ON federation_capabilities.jti = chain.link \
                 ORDER BY chain.depth"
            ),
            [Value::from(jti.to_owned()), Value::from(CHAIN_DEPTH_LIMIT)],
        ))
        .await
        .map_err(CAPABILITIES.failing("reading a refresh chain"))?;
    let chain = rows
        .iter()
        .map(record_from)
        .collect::<Result<Vec<_>, _>>()?;
    let mut seen = std::collections::HashSet::with_capacity(chain.len());
    for link in &chain {
        if !seen.insert(link.jti.as_str()) {
            return Err(super::store::broken_chain(
                jti,
                &link.jti,
                "it is visited twice",
            ));
        }
    }
    if let Some(next) = chain.last().and_then(|tail| tail.refreshed_to.as_deref()) {
        let why = if seen.contains(next) {
            "it is visited twice"
        } else if i64::try_from(chain.len()).unwrap_or(i64::MAX) > CHAIN_DEPTH_LIMIT {
            "the chain is longer than any refresh budget allows"
        } else {
            "it is not recorded"
        };
        return Err(super::store::broken_chain(jti, next, why));
    }
    Ok(chain)
}

/// The durable capability store, and the revocation list it publishes.
#[derive(Debug, Clone)]
pub struct PostgresCapabilities {
    connection: DatabaseConnection,
    clock: std::sync::Arc<dyn Clock>,
}

impl PostgresCapabilities {
    /// A store over `connection`, reading `clock` for pruning and for `generated_at`.
    pub fn new(connection: DatabaseConnection, clock: std::sync::Arc<dyn Clock>) -> Self {
        Self { connection, clock }
    }
}

impl RevocationList for PostgresCapabilities {
    fn revoke(&self, token: RevokedToken) -> RevokeFuture<'_> {
        Box::pin(async move {
            let now = self.clock.now();
            let ceiling = crate::store::deadline(now, MAX_TOKEN_TTL);
            if token.expires_at > ceiling {
                tracing::warn!(
                    jti = %token.jti,
                    expires_at = %token.expires_at,
                    "a revocation was refused: its expiry is beyond the capability TTL ceiling"
                );
                return Err(RevocationError::BeyondTtlCeiling {
                    expires_at: token.expires_at,
                    ceiling: MAX_TOKEN_TTL,
                }
                .into());
            }
            let transaction = begin(&self.connection, CAPABILITIES).await?;
            lock(&transaction, &token.jti, CAPABILITIES).await?;
            // The expiry an entry is published under is the **record's** when one backs it: a
            // caller's shorter `expires_at` would prune the entry while the token still
            // verifies, which is a peer honouring a revoked token.
            let expires_at = record_of(&transaction, &token.jti)
                .await?
                .map_or(token.expires_at, |record| record.expires_at);
            revoke_and_publish(&transaction, &token.jti, expires_at, now).await?;
            commit(transaction, CAPABILITIES).await?;
            tracing::info!(
                jti = %token.jti,
                expires_at = %token.expires_at,
                "a federation capability token was revoked"
            );
            Ok(())
        })
    }

    fn published(&self) -> StoreFuture<'_, PublishedRevocations> {
        Box::pin(async move {
            let now = self.clock.now();
            let transaction = begin(&self.connection, CAPABILITIES).await?;
            transaction
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "DELETE FROM federation_revoked_jti WHERE expires_at <= $1",
                    [Value::from(to_micros(now))],
                ))
                .await
                .map_err(CAPABILITIES.failing("pruning the revocation list"))?;
            let rows = transaction
                .query_all(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT jti, expires_at FROM federation_revoked_jti \
                     ORDER BY expires_at ASC, jti ASC",
                ))
                .await
                .map_err(CAPABILITIES.failing("reading the revocation list"))?;
            commit(transaction, CAPABILITIES).await?;

            let failed = CAPABILITIES.failing("reading the revocation list");
            let mut revoked = Vec::with_capacity(rows.len());
            for row in &rows {
                let jti: String = row.try_get("", "jti").map_err(&failed)?;
                let expires_at: i64 = row.try_get("", "expires_at").map_err(&failed)?;
                revoked.push(RevokedToken {
                    jti,
                    expires_at: instant(CAPABILITIES, expires_at)?,
                });
            }
            Ok(PublishedRevocations {
                generated_at: now,
                revoked,
            })
        })
    }
}

impl CapabilityStore for PostgresCapabilities {
    fn issue(&self, record: CapabilityRecord) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            admissible(&record)?;
            insert(&self.connection, &record).await?;
            tracing::info!(
                jti = %record.jti,
                peer = %record.peer_id,
                album = %record.album_id,
                member = %record.member,
                scope = %record.scope,
                granted_epoch = record.granted_epoch,
                expires_at = %record.expires_at,
                "a federation capability was recorded"
            );
            Ok(())
        })
    }

    fn find<'a>(&'a self, jti: &'a str) -> StoreFuture<'a, Option<CapabilityRecord>> {
        Box::pin(async move { record_of(&self.connection, jti).await })
    }

    fn live<'a>(
        &'a self,
        filter: &'a CapabilityFilter,
        now: Timestamp,
    ) -> StoreFuture<'a, Vec<CapabilityRecord>> {
        Box::pin(async move {
            let (column, key) = match filter {
                CapabilityFilter::Album(album) => ("album_id", album.as_str().to_owned()),
                CapabilityFilter::Peer(peer) => ("peer_id", peer.as_str().to_owned()),
            };
            let rows = self
                .connection
                .query_all(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    format!(
                        "SELECT {CAPABILITY_COLUMNS} FROM federation_capabilities \
                         WHERE {column} = $1 AND revoked_at IS NULL AND expires_at > $2 \
                         ORDER BY jti ASC"
                    ),
                    [Value::from(key), Value::from(to_micros(now))],
                ))
                .await
                .map_err(CAPABILITIES.failing("listing live capabilities"))?;
            rows.iter().map(record_from).collect()
        })
    }

    fn revoke_issued<'a>(&'a self, jti: &'a str, at: Timestamp) -> StoreFuture<'a, RevokeOutcome> {
        Box::pin(async move {
            // Every link but the newest was revoked by the refresh that superseded it, and a
            // refreshed link never changes again — `refresh` writes `refreshed_to` once, in the
            // same transaction that revokes it. So only the **tail** can be live, and only the
            // tail is locked: one advisory lock per transaction however long a peer has made
            // the chain, which is what keeps a revoke from growing with it.
            for attempt in 1..=CHAIN_ATTEMPTS {
                let transaction = begin(&self.connection, CAPABILITIES).await?;
                let chain = chain_of(&transaction, jti).await?;
                let Some(tail) = chain.last() else {
                    return Ok(RevokeOutcome::Unknown);
                };
                lock(&transaction, &tail.jti, CAPABILITIES).await?;
                let tail = record_of(&transaction, &tail.jti).await?.ok_or_else(|| {
                    super::store::broken_chain(jti, &tail.jti, "it is not recorded")
                })?;
                if let Some(next) = &tail.refreshed_to {
                    // A refresh committed between the read and the lock. Start over rather than
                    // take a second lock.
                    tracing::debug!(%jti, tail = %tail.jti, %next, attempt, "the refresh chain moved under a revoke; retrying");
                    continue;
                }
                let mut revoked = 0_usize;
                for link in chain
                    .iter()
                    .take(chain.len() - 1)
                    .chain(std::iter::once(&tail))
                {
                    if link.revoked_at.is_none() {
                        revoke_and_publish(&transaction, &link.jti, link.expires_at, at).await?;
                        revoked += 1;
                    }
                }
                if revoked == 0 {
                    tracing::debug!(%jti, links = chain.len(), "every capability in the refresh chain was already revoked");
                    return Ok(RevokeOutcome::AlreadyRevoked);
                }
                commit(transaction, CAPABILITIES).await?;
                tracing::info!(%jti, links = chain.len(), revoked, "an issued capability was revoked with its refresh chain");
                return Ok(RevokeOutcome::Revoked);
            }
            tracing::warn!(%jti, attempts = CHAIN_ATTEMPTS, "a revoke kept losing its refresh chain's tail to concurrent refreshes");
            Err(StoreError::Unavailable {
                store: CAPABILITIES.store,
                detail: format!(
                    "the refresh chain from {jti} was refreshed under {CHAIN_ATTEMPTS} consecutive \
                     revoke attempts"
                ),
            })
        })
    }

    fn refresh<'a>(
        &'a self,
        predecessor: &'a str,
        successor: CapabilityRecord,
        at: Timestamp,
    ) -> StoreFuture<'a, RefreshOutcome> {
        Box::pin(async move {
            admissible(&successor)?;
            let transaction = begin(&self.connection, CAPABILITIES).await?;
            lock(&transaction, predecessor, CAPABILITIES).await?;
            let Some(old) = record_of(&transaction, predecessor).await? else {
                return Ok(RefreshOutcome::Unknown);
            };
            super::store::continues(predecessor, &old, &successor)?;
            if let Some(next) = &old.refreshed_to {
                let existing =
                    record_of(&transaction, next)
                        .await?
                        .ok_or_else(|| StoreError::Corrupt {
                            store: CAPABILITIES.store,
                            record: CAPABILITIES.record,
                            detail: format!(
                                "{predecessor} was refreshed to {next}, which is not recorded"
                            ),
                        })?;
                return Ok(RefreshOutcome::AlreadyRefreshed(existing));
            }
            if old.revoked_at.is_some() {
                return Ok(RefreshOutcome::Revoked);
            }
            insert(&transaction, &successor).await?;
            revoke_and_publish(&transaction, predecessor, old.expires_at, at).await?;
            transaction
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE federation_capabilities SET refreshed_to = $2 WHERE jti = $1",
                    [
                        Value::from(predecessor.to_owned()),
                        Value::from(successor.jti.clone()),
                    ],
                ))
                .await
                .map_err(CAPABILITIES.failing("linking a refreshed capability"))?;
            commit(transaction, CAPABILITIES).await?;
            tracing::info!(
                predecessor = %predecessor,
                successor = %successor.jti,
                peer = %successor.peer_id,
                "a federation capability was refreshed"
            );
            Ok(RefreshOutcome::Issued(successor))
        })
    }
}

/// The durable peer store.
#[derive(Debug, Clone)]
pub struct PostgresPeers {
    connection: DatabaseConnection,
}

impl PostgresPeers {
    /// A store over `connection`.
    pub fn new(connection: DatabaseConnection) -> Self {
        Self { connection }
    }

    /// Read one peer under `connection`.
    async fn read_under<C: ConnectionTrait>(
        connection: &C,
        peer: &PeerId,
    ) -> Result<Option<PeerRecord>, StoreError> {
        let Some(row) = connection
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT server_id, signing_key, first_seen_at, blocked_at, note \
                 FROM federation_peers WHERE server_id = $1",
                [Value::from(peer.as_str().to_owned())],
            ))
            .await
            .map_err(PEERS.failing("reading a peer"))?
        else {
            return Ok(None);
        };
        let failed = PEERS.failing("reading a peer");
        let server_id: String = row.try_get("", "server_id").map_err(&failed)?;
        let signing_key: Option<Vec<u8>> = row.try_get("", "signing_key").map_err(&failed)?;
        let first_seen_at: i64 = row.try_get("", "first_seen_at").map_err(&failed)?;
        let blocked_at: Option<i64> = row.try_get("", "blocked_at").map_err(&failed)?;
        let note: Option<String> = row.try_get("", "note").map_err(&failed)?;
        let signing_key = signing_key
            .map(|bytes| {
                <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
                    PEERS.undecodable(format!(
                        "{server_id}'s signing key is {} bytes, not thirty-two",
                        bytes.len()
                    ))
                })
            })
            .transpose()?;
        Ok(Some(PeerRecord {
            server_id: PeerId::new(server_id),
            signing_key,
            first_seen_at: instant(PEERS, first_seen_at)?,
            blocked_at: blocked_at
                .map(|micros| instant(PEERS, micros))
                .transpose()?,
            note,
        }))
    }
}

impl PeerStore for PostgresPeers {
    fn pin<'a>(
        &'a self,
        peer: &'a PeerId,
        signing_key: [u8; 32],
        at: Timestamp,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            // A block already on the row is kept: pinning a key is not an opinion about whether
            // to talk to its owner.
            self.connection
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO federation_peers (server_id, signing_key, first_seen_at) \
                     VALUES ($1, $2, $3) \
                     ON CONFLICT (server_id) DO UPDATE SET signing_key = EXCLUDED.signing_key",
                    [
                        Value::from(peer.as_str().to_owned()),
                        Value::from(signing_key.to_vec()),
                        Value::from(to_micros(at)),
                    ],
                ))
                .await
                .map_err(PEERS.failing("pinning a peer's key"))?;
            tracing::info!(%peer, "a peer's signing key was pinned");
            Ok(())
        })
    }

    fn read<'a>(&'a self, peer: &'a PeerId) -> StoreFuture<'a, Option<PeerRecord>> {
        Box::pin(async move { Self::read_under(&self.connection, peer).await })
    }

    fn block<'a>(
        &'a self,
        peer: &'a PeerId,
        at: Timestamp,
        note: Option<String>,
    ) -> StoreFuture<'a, BlockOutcome> {
        Box::pin(async move {
            // Creates the row if the peer was never pinned: blocking a server nobody wanted to
            // hear from is legitimate, and it must not need a key first.
            let updated = self
                .connection
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO federation_peers (server_id, first_seen_at, blocked_at, note) \
                     VALUES ($1, $2, $2, $3) \
                     ON CONFLICT (server_id) DO UPDATE SET blocked_at = $2, note = $3 \
                     WHERE federation_peers.blocked_at IS NULL",
                    [
                        Value::from(peer.as_str().to_owned()),
                        Value::from(to_micros(at)),
                        Value::from(note),
                    ],
                ))
                .await
                .map_err(PEERS.failing("blocking a peer"))?;
            if updated.rows_affected() == 0 {
                return Ok(BlockOutcome::AlreadyBlocked);
            }
            tracing::warn!(%peer, "a peer server was blocked");
            Ok(BlockOutcome::Blocked)
        })
    }

    fn unblock<'a>(&'a self, peer: &'a PeerId) -> StoreFuture<'a, UnblockOutcome> {
        Box::pin(async move {
            let updated = self
                .connection
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE federation_peers SET blocked_at = NULL, note = NULL \
                     WHERE server_id = $1 AND blocked_at IS NOT NULL",
                    [Value::from(peer.as_str().to_owned())],
                ))
                .await
                .map_err(PEERS.failing("unblocking a peer"))?;
            if updated.rows_affected() == 0 {
                return Ok(UnblockOutcome::NotBlocked);
            }
            tracing::info!(%peer, "a peer server was unblocked");
            Ok(UnblockOutcome::Unblocked)
        })
    }
}

#[cfg(test)]
mod tests {
    /// The suite, against a real Postgres.
    mod postgres_conformance {
        use std::sync::Arc;

        use super::super::{PostgresCapabilities, PostgresPeers};
        use crate::federation::conformance::{self, Harness};
        use crate::federation::{CapabilityStore, PeerStore};
        use crate::postgres::testing;
        use crate::store::memory::ManualClock;

        /// Both stores over one container.
        #[derive(Debug)]
        struct PostgresHarness {
            clock: Arc<ManualClock>,
            capabilities: PostgresCapabilities,
            peers: PostgresPeers,
        }

        impl Harness for PostgresHarness {
            fn capabilities(&self) -> &dyn CapabilityStore {
                &self.capabilities
            }

            fn peers(&self) -> &dyn PeerStore {
                &self.peers
            }

            fn clock(&self) -> &ManualClock {
                &self.clock
            }
        }

        #[tokio::test]
        async fn the_postgres_federation_stores_conform() {
            let Some(database) = testing::start("the Postgres federation stores").await else {
                return;
            };
            let clock = Arc::new(ManualClock::default());
            let harness = PostgresHarness {
                capabilities: PostgresCapabilities::new(
                    database.connection().clone(),
                    clock.clone(),
                ),
                peers: PostgresPeers::new(database.connection().clone()),
                clock,
            };
            conformance::run_all(&harness).await;
        }
    }
}
