//! The pulled quota total's arithmetic leaves the schema: migration `000030`'s statement-level
//! triggers and the functions they call are dropped, and every writer of
//! `federation_subscriptions`, `federation_pulled_metadata_blobs` and `quota_attributions` now
//! maintains `quota_pulled_holdings` and `quota_usage.pulled` itself, in its own transaction
//! (`capsule_server::quota::pulled_total`, the same statements the triggers ran). The table, the
//! column, their `CHECK`s and the rows all stay.
//!
//! The three `*_rekeyed` guards stay too. They hold no arithmetic: each refuses an `UPDATE` of a
//! column the total is keyed on, so a write nobody anticipated is refused rather than allowed to
//! leave the total silently wrong.
//!
//! `down` puts the dropped functions and triggers back exactly as `000030` created them, without
//! its backfill: the rows they maintain are already current.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

/// Drop `000030`'s maintenance triggers and their functions, keeping the re-key guards.
const UP: &str = "
DROP TRIGGER IF EXISTS quota_pulled_subscription_inserted ON federation_subscriptions;
DROP TRIGGER IF EXISTS quota_pulled_subscription_deleted ON federation_subscriptions;
DROP TRIGGER IF EXISTS quota_pulled_blob_inserted ON federation_pulled_metadata_blobs;
DROP TRIGGER IF EXISTS quota_pulled_blob_deleted ON federation_pulled_metadata_blobs;
DROP TRIGGER IF EXISTS quota_pulled_attribution_inserted ON quota_attributions;
DROP TRIGGER IF EXISTS quota_pulled_attribution_deleted ON quota_attributions;
DROP FUNCTION IF EXISTS quota_pulled_on_subscription();
DROP FUNCTION IF EXISTS quota_pulled_on_blob();
DROP FUNCTION IF EXISTS quota_pulled_on_attribution();
DROP FUNCTION IF EXISTS quota_pulled_hold(TEXT[], TEXT[], BIGINT[], BIGINT[]);
DROP FUNCTION IF EXISTS quota_pulled_drop(TEXT[], TEXT[], BIGINT[]);
DROP FUNCTION IF EXISTS quota_pulled_lock_job(TEXT, TEXT);
DROP FUNCTION IF EXISTS quota_pulled_lock_accounts(TEXT[]);
";

/// Recreate the maintenance functions and triggers `UP` dropped.
const DOWN: &str = r#"
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

CREATE TRIGGER quota_pulled_subscription_inserted
    AFTER INSERT ON federation_subscriptions REFERENCING NEW TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION quota_pulled_on_subscription();
CREATE TRIGGER quota_pulled_subscription_deleted
    AFTER DELETE ON federation_subscriptions REFERENCING OLD TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION quota_pulled_on_subscription();
CREATE TRIGGER quota_pulled_blob_inserted
    AFTER INSERT ON federation_pulled_metadata_blobs REFERENCING NEW TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION quota_pulled_on_blob();
CREATE TRIGGER quota_pulled_blob_deleted
    AFTER DELETE ON federation_pulled_metadata_blobs REFERENCING OLD TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION quota_pulled_on_blob();
CREATE TRIGGER quota_pulled_attribution_inserted
    AFTER INSERT ON quota_attributions REFERENCING NEW TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION quota_pulled_on_attribution();
CREATE TRIGGER quota_pulled_attribution_deleted
    AFTER DELETE ON quota_attributions REFERENCING OLD TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION quota_pulled_on_attribution();
"#;

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
