//! The pulled total's maintenance, run by each writer inside its own transaction.
//!
//! `quota_usage.pulled` is `SUM(size)` over an account's `quota_pulled_holdings` whose address
//! `quota_attributions` does not attribute to that same account, and a holding's `refs` is the
//! number of `federation_subscriptions` × `federation_pulled_metadata_blobs` pairs naming it
//! (design/quota.md). Three writers change those facts, and each calls one function here right
//! after its own statement, in its own transaction:
//!
//! - [`subscriptions_changed`] — the subscription store's insert and removal;
//! - [`blobs_changed`] — the pulled-album store's page commit;
//! - [`attributions_changed`] — the quota ledger's charge and every release.
//!
//! These were statement-level triggers until they moved here, so that the arithmetic is Rust a
//! reviewer reads beside the write that needs it rather than plpgsql in a migration, and so the
//! schema holds no logic another database would have to reproduce. The SQL each function runs
//! is the triggers' own, statement for statement. What the move gives up is that a trigger could
//! not be skipped by a new write path; the quota drift scrub (`quota/`, design/quota.md) is what
//! catches a path that forgets, and a conformance case recomputes the total from scratch.
//!
//! # Locks, and why the total cannot miss a concurrent writer
//!
//! Two locks, always in this order, exactly as before:
//!
//! 1. **The job lock**, `pg_advisory_xact_lock(hashtext('quota_pulled_job'), hashtext(peer ||
//!    '/' || album))`, taken by the subscription and pulled-blob paths after their own rows are
//!    written and before they read the other table, in the order of that second key. A
//!    subscription and a pulled page for one job therefore cannot both miss each other's rows:
//!    whichever takes the lock second reads, in a fresh `READ COMMITTED` snapshot, the rows the
//!    first committed.
//! 2. **The account rows**, `quota_usage … FOR UPDATE` in account order (created first where
//!    missing), taken by every path before it touches a holding or reads an attribution.
//!
//! No path takes a job lock after an account lock, so no cycle forms between them.

use sea_orm::{ConnectionTrait, DbBackend, Statement, Value};

use crate::postgres::error::Port;
use crate::store::StoreError;

const PORT: Port = Port {
    store: "quota",
    record: "pulled holding",
};

/// Whether a writer's rows arrived or left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change {
    /// The rows were inserted.
    Added,
    /// The rows were deleted.
    Removed,
}

fn statement(sql: &str, values: impl IntoIterator<Item = Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// Take the job lock of every `(peer, album)` in `jobs`, in the one order every path uses.
async fn lock_jobs(
    connection: &impl ConnectionTrait,
    peers: Vec<String>,
    albums: Vec<String>,
) -> Result<(), StoreError> {
    let ordered = connection
        .query_all_raw(statement(
            "SELECT peer, album FROM unnest($1::text[], $2::text[]) AS j(peer, album) \
             GROUP BY peer, album \
             ORDER BY hashtext(peer || '/' || album), peer COLLATE \"C\", album COLLATE \"C\"",
            [Value::from(peers), Value::from(albums)],
        ))
        .await
        .map_err(PORT.failing("ordering the pulled jobs to lock"))?;
    for row in ordered {
        let peer: String = row
            .try_get("", "peer")
            .map_err(PORT.failing("reading a job to lock"))?;
        let album: String = row
            .try_get("", "album")
            .map_err(PORT.failing("reading a job to lock"))?;
        connection
            .execute_raw(statement(
                "SELECT pg_advisory_xact_lock(hashtext('quota_pulled_job'), \
                                              hashtext($1 || '/' || $2))",
                [Value::from(peer), Value::from(album)],
            ))
            .await
            .map_err(PORT.failing("locking a pulled job's total"))?;
    }
    Ok(())
}

/// Lock `accounts`' usage rows in account order, creating the ones that do not exist yet.
///
/// The second of the two locks; [`crate::quota::PostgresQuota::repair_pulled`] takes it too.
pub(crate) async fn lock_accounts(
    connection: &impl ConnectionTrait,
    accounts: &[String],
) -> Result<(), StoreError> {
    connection
        .execute_raw(statement(
            "INSERT INTO quota_usage (user_id) \
             SELECT account FROM unnest($1::text[]) AS account \
             GROUP BY account ORDER BY account COLLATE \"C\" \
             ON CONFLICT (user_id) DO NOTHING",
            [Value::from(accounts.to_vec())],
        ))
        .await
        .map_err(PORT.failing("creating the accounts' usage rows"))?;
    connection
        .execute_raw(statement(
            "SELECT 1 FROM quota_usage WHERE user_id = ANY($1::text[]) \
             ORDER BY user_id COLLATE \"C\" FOR UPDATE",
            [Value::from(accounts.to_vec())],
        ))
        .await
        .map_err(PORT.failing("locking the accounts' usage rows"))?;
    Ok(())
}

/// The `(account, address, size, refs)` pairs a change touches, as four parallel columns.
#[derive(Debug, Default)]
struct Pairs {
    accounts: Vec<String>,
    addresses: Vec<String>,
    sizes: Vec<i64>,
    counts: Vec<i64>,
}

async fn pairs(connection: &impl ConnectionTrait, query: Statement) -> Result<Pairs, StoreError> {
    let rows = connection
        .query_all_raw(query)
        .await
        .map_err(PORT.failing("reading the holdings a change touches"))?;
    let mut pairs = Pairs::default();
    let failed = PORT.failing("reading a touched holding");
    for row in rows {
        pairs
            .accounts
            .push(row.try_get("", "user_id").map_err(&failed)?);
        pairs
            .addresses
            .push(row.try_get("", "address").map_err(&failed)?);
        pairs.sizes.push(row.try_get("", "size").map_err(&failed)?);
        pairs.counts.push(row.try_get("", "refs").map_err(&failed)?);
    }
    Ok(pairs)
}

/// Add or remove `pairs`' references, moving each account's total by the holdings that appear
/// or disappear and that the account is not itself attributed.
async fn apply(
    connection: &impl ConnectionTrait,
    pairs: Pairs,
    change: Change,
) -> Result<(), StoreError> {
    if pairs.accounts.is_empty() {
        return Ok(());
    }
    let mut accounts = pairs.accounts.clone();
    accounts.sort();
    accounts.dedup();
    lock_accounts(connection, &accounts).await?;
    let held = match change {
        Change::Added => statement(
            "WITH pairs AS ( \
                 SELECT * FROM unnest($1::text[], $2::text[], $3::bigint[], $4::bigint[]) \
                     AS p(user_id, address, size, refs) \
             ), \
             held AS ( \
                 INSERT INTO quota_pulled_holdings (user_id, address, size, refs) \
                     SELECT user_id, address, size, refs FROM pairs \
                     ORDER BY user_id COLLATE \"C\", address COLLATE \"C\" \
                 ON CONFLICT (user_id, address) \
                     DO UPDATE SET refs = quota_pulled_holdings.refs + EXCLUDED.refs \
                 RETURNING user_id, address, size, (xmax = 0) AS created \
             ), \
             grown AS ( \
                 SELECT h.user_id, SUM(h.size)::BIGINT AS bytes FROM held h \
                 WHERE h.created AND NOT EXISTS ( \
                     SELECT 1 FROM quota_attributions a \
                     WHERE a.address = h.address AND a.user_id = h.user_id) \
                 GROUP BY h.user_id \
             ) \
             UPDATE quota_usage q SET pulled = q.pulled + g.bytes FROM grown g \
             WHERE q.user_id = g.user_id",
            [
                Value::from(pairs.accounts),
                Value::from(pairs.addresses),
                Value::from(pairs.sizes),
                Value::from(pairs.counts),
            ],
        ),
        Change::Removed => statement(
            "WITH pairs AS ( \
                 SELECT * FROM unnest($1::text[], $2::text[], $3::bigint[]) \
                     AS p(user_id, address, refs) \
             ), \
             kept AS ( \
                 UPDATE quota_pulled_holdings h SET refs = h.refs - p.refs FROM pairs p \
                 WHERE h.user_id = p.user_id AND h.address = p.address AND h.refs > p.refs \
                 RETURNING h.user_id \
             ), \
             gone AS ( \
                 DELETE FROM quota_pulled_holdings h USING pairs p \
                 WHERE h.user_id = p.user_id AND h.address = p.address AND h.refs <= p.refs \
                 RETURNING h.user_id, h.address, h.size \
             ), \
             shrunk AS ( \
                 SELECT g.user_id, SUM(g.size)::BIGINT AS bytes FROM gone g \
                 WHERE NOT EXISTS ( \
                     SELECT 1 FROM quota_attributions a \
                     WHERE a.address = g.address AND a.user_id = g.user_id) \
                 GROUP BY g.user_id \
             ) \
             UPDATE quota_usage q SET pulled = q.pulled - s.bytes FROM shrunk s \
             WHERE q.user_id = s.user_id",
            [
                Value::from(pairs.accounts),
                Value::from(pairs.addresses),
                Value::from(pairs.counts),
            ],
        ),
    };
    connection
        .execute_raw(held)
        .await
        .map_err(PORT.failing("moving the pulled total"))?;
    tracing::debug!(
        accounts = accounts.len(),
        ?change,
        "moved the pulled quota total"
    );
    Ok(())
}

/// Subscriptions `(subscriber, peer, album)` were inserted or deleted by the caller's last
/// statement: the job's pulled blobs are held, or dropped, for each subscriber.
pub(crate) async fn subscriptions_changed(
    connection: &impl ConnectionTrait,
    rows: &[(String, String, String)],
    change: Change,
) -> Result<(), StoreError> {
    if rows.is_empty() {
        return Ok(());
    }
    let subscribers: Vec<String> = rows.iter().map(|row| row.0.clone()).collect();
    let peers: Vec<String> = rows.iter().map(|row| row.1.clone()).collect();
    let albums: Vec<String> = rows.iter().map(|row| row.2.clone()).collect();
    lock_jobs(connection, peers.clone(), albums.clone()).await?;
    let pairs = pairs(
        connection,
        statement(
            "SELECT c.subscriber_id AS user_id, b.address, MAX(b.size)::BIGINT AS size, \
                    COUNT(*)::BIGINT AS refs \
             FROM unnest($1::text[], $2::text[], $3::text[]) AS c(subscriber_id, peer_id, album_id) \
             JOIN federation_pulled_metadata_blobs b \
               ON b.peer_id = c.peer_id AND b.album_id = c.album_id \
             GROUP BY c.subscriber_id, b.address",
            [
                Value::from(subscribers),
                Value::from(peers),
                Value::from(albums),
            ],
        ),
    )
    .await?;
    apply(connection, pairs, change).await
}

/// Pulled metadata blobs `(peer, album, address, size)` were inserted or deleted by the caller's
/// last statement: each is held, or dropped, for every subscriber of its job.
pub(crate) async fn blobs_changed(
    connection: &impl ConnectionTrait,
    rows: &[(String, String, String, i64)],
    change: Change,
) -> Result<(), StoreError> {
    if rows.is_empty() {
        return Ok(());
    }
    let peers: Vec<String> = rows.iter().map(|row| row.0.clone()).collect();
    let albums: Vec<String> = rows.iter().map(|row| row.1.clone()).collect();
    let addresses: Vec<String> = rows.iter().map(|row| row.2.clone()).collect();
    let sizes: Vec<i64> = rows.iter().map(|row| row.3).collect();
    lock_jobs(connection, peers.clone(), albums.clone()).await?;
    let pairs = pairs(
        connection,
        statement(
            "SELECT s.subscriber_id AS user_id, c.address, MAX(c.size)::BIGINT AS size, \
                    COUNT(*)::BIGINT AS refs \
             FROM unnest($1::text[], $2::text[], $3::text[], $4::bigint[]) \
                 AS c(peer_id, album_id, address, size) \
             JOIN federation_subscriptions s \
               ON s.peer_id = c.peer_id AND s.album_id = c.album_id \
             GROUP BY s.subscriber_id, c.address",
            [
                Value::from(peers),
                Value::from(albums),
                Value::from(addresses),
                Value::from(sizes),
            ],
        ),
    )
    .await?;
    apply(connection, pairs, change).await
}

/// Attributions `(account, address)` were inserted or deleted by the caller's last statement:
/// a holding of the same account leaves the total when its address is attributed to it, and
/// rejoins it when the attribution goes.
pub(crate) async fn attributions_changed(
    connection: &impl ConnectionTrait,
    rows: &[(String, String)],
    change: Change,
) -> Result<(), StoreError> {
    if rows.is_empty() {
        return Ok(());
    }
    let users: Vec<String> = rows.iter().map(|row| row.0.clone()).collect();
    let addresses: Vec<String> = rows.iter().map(|row| row.1.clone()).collect();
    let mut accounts = users.clone();
    accounts.sort();
    accounts.dedup();
    lock_accounts(connection, &accounts).await?;
    let direction: i64 = match change {
        Change::Added => -1,
        Change::Removed => 1,
    };
    connection
        .execute_raw(statement(
            "UPDATE quota_usage q SET pulled = q.pulled + $3 * moved.bytes \
             FROM (SELECT c.user_id, SUM(h.size)::BIGINT AS bytes \
                   FROM unnest($1::text[], $2::text[]) AS c(user_id, address) \
                   JOIN quota_pulled_holdings h \
                     ON h.user_id = c.user_id AND h.address = c.address \
                   GROUP BY c.user_id) moved \
             WHERE q.user_id = moved.user_id",
            [
                Value::from(users),
                Value::from(addresses),
                Value::from(direction),
            ],
        ))
        .await
        .map_err(PORT.failing("moving the pulled total for an attribution"))?;
    tracing::debug!(
        accounts = accounts.len(),
        ?change,
        "an attribution moved the pulled total"
    );
    Ok(())
}
