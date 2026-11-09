//! The quota ledger's materialised pulled total (#1171): `quota_pulled_holdings`, one row per
//! `(account, address)` the account holds through its subscriptions, and
//! `quota_usage.pulled`, the bytes of those rows the account is not itself attributed.
//!
//! # What the two hold
//!
//! A holding's `refs` is the number of `federation_subscriptions` × `federation_pulled_metadata_blobs`
//! pairs that name it: one per album, on any peer, the account subscribes to and that holds the
//! address. The row exists exactly while `refs` is positive, so an address held through two
//! albums is one row, counted once. Its `size` is the size the first pair recorded; an address
//! names its bytes, so every pair agrees.
//!
//! `pulled` is `SUM(size)` over the account's holdings whose address `quota_attributions` does
//! not attribute to that same account: an address the account uploaded is counted there and not
//! again here, and one another account uploaded is still counted here.
//!
//! # Maintained by triggers, in the writer's own transaction
//!
//! Every statement that changes one of the three facts the total is derived from — a
//! subscription, a pulled blob row, an attribution — fires a statement-level trigger that
//! applies the difference its own rows make, before that statement returns. So the total moves
//! in the same transaction as the pulled page commit, the unsubscribe, the cascade that forgets a
//! job, the charge, and the collector's release, and no write path can skip it: the subscription
//! store, the pull store and the quota ledger never write it themselves.
//!
//! # Locks, and why the total cannot miss a concurrent writer
//!
//! Two locks, always in this order:
//!
//! 1. **The job lock**, `pg_advisory_xact_lock(hashtext('quota_pulled_job'), hashtext(peer ||
//!    '/' || album))`, taken by the subscription and pulled blob triggers in the order of that
//!    second key (so two statements naming several jobs cannot take colliding keys in opposite
//!    orders), after
//!    their own rows are written and before they read the other table. A subscription and a
//!    pulled page for one job therefore cannot both miss each other's rows: whichever takes the
//!    lock second reads, in a fresh READ COMMITTED snapshot, the rows the first committed. The
//!    two-key form keeps it out of the one-key space the pull store's per-address locks use.
//! 2. **The account rows**, `quota_usage … FOR UPDATE` in account order (created first where
//!    missing), taken by every trigger before it touches a holding or reads an attribution. The
//!    attribution triggers take it after the attribution row is written, which is the order
//!    `PostgresQuota::charge` already takes it in, so an upload and a pulled page for one address
//!    cannot both miss each other either.
//!
//! No trigger takes a job lock after an account lock, so no cycle forms between them.
//!
//! # Refusals
//!
//! A column the difference is keyed on — an attribution's address or account, a subscription's
//! or a pulled blob row's key or size — is refused on `UPDATE`: no adapter changes one, and a
//! change the triggers did not see would leave the total wrong. `refs > 0` and `pulled >= 0` are
//! `CHECK`s, so an arithmetic slip refuses the write rather than storing a negative total.
//!
//! # The backfill
//!
//! `up` locks the three tables against writes (`SHARE ROW EXCLUSIVE`) for the length of the
//! migration's transaction, so a server still running the previous binary cannot write a row
//! between the backfill's read and the triggers taking over; it waits, then is counted by them.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

/// The triggers, their functions, and the backfill, run after the table and column exist.
const UP: &str = r#"
LOCK TABLE federation_subscriptions, federation_pulled_metadata_blobs, quota_attributions
    IN SHARE ROW EXCLUSIVE MODE;

-- Lock `accounts`' usage rows in account order, creating the ones that do not exist yet.
CREATE FUNCTION quota_pulled_lock_accounts(accounts TEXT[]) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO quota_usage (user_id)
        SELECT account FROM unnest(accounts) AS account
        GROUP BY account ORDER BY account COLLATE "C"
        ON CONFLICT (user_id) DO NOTHING;
    PERFORM 1 FROM quota_usage WHERE user_id = ANY(accounts)
        ORDER BY user_id COLLATE "C" FOR UPDATE;
END;
$$;

-- Lock the job `(peer, album)` against the other pulled-total trigger.
CREATE FUNCTION quota_pulled_lock_job(peer TEXT, album TEXT) RETURNS void
LANGUAGE sql AS $$
    SELECT pg_advisory_xact_lock(hashtext('quota_pulled_job'), hashtext(peer || '/' || album));
$$;

-- Add `refs` pairs per (account, address), unique per pair; the accounts are locked already.
CREATE FUNCTION quota_pulled_hold(accounts TEXT[], addresses TEXT[], sizes BIGINT[], counts BIGINT[])
RETURNS void LANGUAGE sql AS $$
    WITH pairs AS (
        SELECT * FROM unnest(accounts, addresses, sizes, counts) AS p(user_id, address, size, refs)
    ),
    held AS (
        INSERT INTO quota_pulled_holdings (user_id, address, size, refs)
            SELECT user_id, address, size, refs FROM pairs
            ORDER BY user_id COLLATE "C", address COLLATE "C"
        ON CONFLICT (user_id, address)
            DO UPDATE SET refs = quota_pulled_holdings.refs + EXCLUDED.refs
        RETURNING user_id, address, size, (xmax = 0) AS created
    ),
    grown AS (
        SELECT h.user_id, SUM(h.size)::BIGINT AS bytes FROM held h
        WHERE h.created AND NOT EXISTS (
            SELECT 1 FROM quota_attributions a WHERE a.address = h.address AND a.user_id = h.user_id)
        GROUP BY h.user_id
    )
    UPDATE quota_usage q SET pulled = q.pulled + g.bytes FROM grown g WHERE q.user_id = g.user_id;
$$;

-- Remove `refs` pairs per (account, address), unique per pair; the accounts are locked already.
CREATE FUNCTION quota_pulled_drop(accounts TEXT[], addresses TEXT[], counts BIGINT[])
RETURNS void LANGUAGE sql AS $$
    WITH pairs AS (
        SELECT * FROM unnest(accounts, addresses, counts) AS p(user_id, address, refs)
    ),
    kept AS (
        UPDATE quota_pulled_holdings h SET refs = h.refs - p.refs FROM pairs p
        WHERE h.user_id = p.user_id AND h.address = p.address AND h.refs > p.refs
        RETURNING h.user_id
    ),
    gone AS (
        DELETE FROM quota_pulled_holdings h USING pairs p
        WHERE h.user_id = p.user_id AND h.address = p.address AND h.refs <= p.refs
        RETURNING h.user_id, h.address, h.size
    ),
    shrunk AS (
        SELECT g.user_id, SUM(g.size)::BIGINT AS bytes FROM gone g
        WHERE NOT EXISTS (
            SELECT 1 FROM quota_attributions a WHERE a.address = g.address AND a.user_id = g.user_id)
        GROUP BY g.user_id
    )
    UPDATE quota_usage q SET pulled = q.pulled - s.bytes FROM shrunk s WHERE q.user_id = s.user_id;
$$;

-- A subscription inserted or deleted: the job's pulled blobs, held or dropped for its subscriber.
CREATE FUNCTION quota_pulled_on_subscription() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    job RECORD;
    accounts TEXT[];
    addresses TEXT[];
    sizes BIGINT[];
    counts BIGINT[];
BEGIN
    FOR job IN SELECT peer_id, album_id FROM changed GROUP BY peer_id, album_id
               ORDER BY hashtext(peer_id || '/' || album_id), peer_id COLLATE "C",
                        album_id COLLATE "C" LOOP
        PERFORM quota_pulled_lock_job(job.peer_id, job.album_id);
    END LOOP;
    SELECT array_agg(user_id), array_agg(address), array_agg(size), array_agg(refs)
      INTO accounts, addresses, sizes, counts
      FROM (SELECT c.subscriber_id AS user_id, b.address, MAX(b.size)::BIGINT AS size,
                   COUNT(*)::BIGINT AS refs
            FROM changed c
            JOIN federation_pulled_metadata_blobs b
              ON b.peer_id = c.peer_id AND b.album_id = c.album_id
            GROUP BY c.subscriber_id, b.address) pairs;
    IF accounts IS NULL THEN
        RETURN NULL;
    END IF;
    PERFORM quota_pulled_lock_accounts(accounts);
    IF TG_OP = 'INSERT' THEN
        PERFORM quota_pulled_hold(accounts, addresses, sizes, counts);
    ELSE
        PERFORM quota_pulled_drop(accounts, addresses, counts);
    END IF;
    RETURN NULL;
END;
$$;

-- Pulled blob rows inserted or deleted: held or dropped for every subscriber of their job.
CREATE FUNCTION quota_pulled_on_blob() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    job RECORD;
    accounts TEXT[];
    addresses TEXT[];
    sizes BIGINT[];
    counts BIGINT[];
BEGIN
    FOR job IN SELECT peer_id, album_id FROM changed GROUP BY peer_id, album_id
               ORDER BY hashtext(peer_id || '/' || album_id), peer_id COLLATE "C",
                        album_id COLLATE "C" LOOP
        PERFORM quota_pulled_lock_job(job.peer_id, job.album_id);
    END LOOP;
    SELECT array_agg(user_id), array_agg(address), array_agg(size), array_agg(refs)
      INTO accounts, addresses, sizes, counts
      FROM (SELECT s.subscriber_id AS user_id, c.address, MAX(c.size)::BIGINT AS size,
                   COUNT(*)::BIGINT AS refs
            FROM changed c
            JOIN federation_subscriptions s
              ON s.peer_id = c.peer_id AND s.album_id = c.album_id
            GROUP BY s.subscriber_id, c.address) pairs;
    IF accounts IS NULL THEN
        RETURN NULL;
    END IF;
    PERFORM quota_pulled_lock_accounts(accounts);
    IF TG_OP = 'INSERT' THEN
        PERFORM quota_pulled_hold(accounts, addresses, sizes, counts);
    ELSE
        PERFORM quota_pulled_drop(accounts, addresses, counts);
    END IF;
    RETURN NULL;
END;
$$;

-- Attributions inserted or deleted: a holding of the same account leaves or rejoins the total.
CREATE FUNCTION quota_pulled_on_attribution() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    accounts TEXT[];
    direction BIGINT := CASE WHEN TG_OP = 'INSERT' THEN -1 ELSE 1 END;
BEGIN
    SELECT array_agg(DISTINCT user_id) INTO accounts FROM changed;
    IF accounts IS NULL THEN
        RETURN NULL;
    END IF;
    PERFORM quota_pulled_lock_accounts(accounts);
    UPDATE quota_usage q SET pulled = q.pulled + direction * moved.bytes
      FROM (SELECT c.user_id, SUM(h.size)::BIGINT AS bytes
            FROM changed c
            JOIN quota_pulled_holdings h ON h.user_id = c.user_id AND h.address = c.address
            GROUP BY c.user_id) moved
     WHERE q.user_id = moved.user_id;
    RETURN NULL;
END;
$$;

CREATE FUNCTION quota_pulled_refuse_rekey() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'the pulled quota total is keyed on %''s changed columns: an UPDATE of them is refused', TG_TABLE_NAME;
END;
$$;

CREATE TRIGGER quota_pulled_subscription_inserted
    AFTER INSERT ON federation_subscriptions REFERENCING NEW TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION quota_pulled_on_subscription();
CREATE TRIGGER quota_pulled_subscription_deleted
    AFTER DELETE ON federation_subscriptions REFERENCING OLD TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION quota_pulled_on_subscription();
CREATE TRIGGER quota_pulled_subscription_rekeyed
    BEFORE UPDATE ON federation_subscriptions FOR EACH ROW
    WHEN (OLD.subscriber_id IS DISTINCT FROM NEW.subscriber_id
          OR OLD.peer_id IS DISTINCT FROM NEW.peer_id
          OR OLD.album_id IS DISTINCT FROM NEW.album_id)
    EXECUTE FUNCTION quota_pulled_refuse_rekey();

CREATE TRIGGER quota_pulled_blob_inserted
    AFTER INSERT ON federation_pulled_metadata_blobs REFERENCING NEW TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION quota_pulled_on_blob();
CREATE TRIGGER quota_pulled_blob_deleted
    AFTER DELETE ON federation_pulled_metadata_blobs REFERENCING OLD TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION quota_pulled_on_blob();
CREATE TRIGGER quota_pulled_blob_rekeyed
    BEFORE UPDATE ON federation_pulled_metadata_blobs FOR EACH ROW
    WHEN (OLD.peer_id IS DISTINCT FROM NEW.peer_id
          OR OLD.album_id IS DISTINCT FROM NEW.album_id
          OR OLD.address IS DISTINCT FROM NEW.address
          OR OLD.size IS DISTINCT FROM NEW.size)
    EXECUTE FUNCTION quota_pulled_refuse_rekey();

CREATE TRIGGER quota_pulled_attribution_inserted
    AFTER INSERT ON quota_attributions REFERENCING NEW TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION quota_pulled_on_attribution();
CREATE TRIGGER quota_pulled_attribution_deleted
    AFTER DELETE ON quota_attributions REFERENCING OLD TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION quota_pulled_on_attribution();
CREATE TRIGGER quota_pulled_attribution_rekeyed
    BEFORE UPDATE ON quota_attributions FOR EACH ROW
    WHEN (OLD.address IS DISTINCT FROM NEW.address OR OLD.user_id IS DISTINCT FROM NEW.user_id)
    EXECUTE FUNCTION quota_pulled_refuse_rekey();

-- The backfill: every holding the existing rows imply, and each account's total of them.
INSERT INTO quota_pulled_holdings (user_id, address, size, refs)
    SELECT s.subscriber_id, b.address, MAX(b.size), COUNT(*)
    FROM federation_subscriptions s
    JOIN federation_pulled_metadata_blobs b ON b.peer_id = s.peer_id AND b.album_id = s.album_id
    GROUP BY s.subscriber_id, b.address;
INSERT INTO quota_usage (user_id)
    SELECT DISTINCT user_id FROM quota_pulled_holdings
    ON CONFLICT (user_id) DO NOTHING;
UPDATE quota_usage q SET pulled = t.bytes
    FROM (SELECT h.user_id, SUM(h.size)::BIGINT AS bytes FROM quota_pulled_holdings h
          WHERE NOT EXISTS (SELECT 1 FROM quota_attributions a
                            WHERE a.address = h.address AND a.user_id = h.user_id)
          GROUP BY h.user_id) t
    WHERE q.user_id = t.user_id;
"#;

/// What `down` drops before the table and the column.
const DOWN: &str = "
DROP TRIGGER IF EXISTS quota_pulled_subscription_inserted ON federation_subscriptions;
DROP TRIGGER IF EXISTS quota_pulled_subscription_deleted ON federation_subscriptions;
DROP TRIGGER IF EXISTS quota_pulled_subscription_rekeyed ON federation_subscriptions;
DROP TRIGGER IF EXISTS quota_pulled_blob_inserted ON federation_pulled_metadata_blobs;
DROP TRIGGER IF EXISTS quota_pulled_blob_deleted ON federation_pulled_metadata_blobs;
DROP TRIGGER IF EXISTS quota_pulled_blob_rekeyed ON federation_pulled_metadata_blobs;
DROP TRIGGER IF EXISTS quota_pulled_attribution_inserted ON quota_attributions;
DROP TRIGGER IF EXISTS quota_pulled_attribution_deleted ON quota_attributions;
DROP TRIGGER IF EXISTS quota_pulled_attribution_rekeyed ON quota_attributions;
DROP FUNCTION IF EXISTS quota_pulled_on_subscription();
DROP FUNCTION IF EXISTS quota_pulled_on_blob();
DROP FUNCTION IF EXISTS quota_pulled_on_attribution();
DROP FUNCTION IF EXISTS quota_pulled_refuse_rekey();
DROP FUNCTION IF EXISTS quota_pulled_hold(TEXT[], TEXT[], BIGINT[], BIGINT[]);
DROP FUNCTION IF EXISTS quota_pulled_drop(TEXT[], TEXT[], BIGINT[]);
DROP FUNCTION IF EXISTS quota_pulled_lock_job(TEXT, TEXT);
DROP FUNCTION IF EXISTS quota_pulled_lock_accounts(TEXT[]);
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(QuotaPulledHoldings::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(QuotaPulledHoldings::UserId).text().not_null())
                    .col(ColumnDef::new(QuotaPulledHoldings::Address).text().not_null())
                    .col(
                        ColumnDef::new(QuotaPulledHoldings::Size)
                            .big_integer()
                            .not_null()
                            .check(Expr::col(QuotaPulledHoldings::Size).gte(0)),
                    )
                    .col(
                        ColumnDef::new(QuotaPulledHoldings::Refs)
                            .big_integer()
                            .not_null()
                            .check(Expr::col(QuotaPulledHoldings::Refs).gt(0)),
                    )
                    .primary_key(
                        Index::create()
                            .col(QuotaPulledHoldings::UserId)
                            .col(QuotaPulledHoldings::Address),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(QuotaUsage::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(QuotaUsage::Pulled)
                            .big_integer()
                            .not_null()
                            .default(0)
                            .check(Expr::col(QuotaUsage::Pulled).gte(0)),
                    )
                    .to_owned(),
            )
            .await?;
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        manager
            .alter_table(
                Table::alter()
                    .table(QuotaUsage::Table)
                    .drop_column(QuotaUsage::Pulled)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(QuotaPulledHoldings::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum QuotaPulledHoldings {
    Table,
    UserId,
    Address,
    Size,
    Refs,
}

#[derive(DeriveIden)]
enum QuotaUsage {
    Table,
    Pulled,
}
