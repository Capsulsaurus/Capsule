//! The state Valkey held until ADR-0015: sessions, upload sessions, the four ceremonies and the
//! enrollment relay, rate-budget windows, and the federation pull schedule.
//!
//! # One table per port, each record carrying its own expiry
//!
//! Every table has an `expires_at` column of epoch microseconds, written by the adapter from the
//! clock it was given. A read treats a row at or past it as absent; the sweeper
//! (`capsule_server::store::sweep`) deletes such rows in bounded batches, and that is the only
//! thing that deletes them for being old. There is no shared "TTL" table: each port's columns
//! are its record's, so a bug in one cannot read as another's state.
//!
//! # Fill factor
//!
//! `auth_sessions` (activity stamps) and `upload_sessions` (one committed-range write per chunk)
//! are updated in place far more often than they are inserted. A fill factor below 100 leaves
//! room on each page for the new row version, so those updates stay heap-only: none of the
//! columns they change is indexed, and the index entries are not rewritten.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

const UP: &str = r"
CREATE TABLE auth_sessions (
    session_id       TEXT    PRIMARY KEY,
    user_id          TEXT    NOT NULL,
    created_at       BIGINT  NOT NULL,
    authenticated_at BIGINT  NOT NULL,
    signed_in_at     BIGINT  NOT NULL,
    last_active_at   BIGINT  NOT NULL,
    user_agent       TEXT,
    ip_address       TEXT,
    cohort_hash      TEXT,
    device_id        TEXT,
    expires_at       BIGINT  NOT NULL
) WITH (fillfactor = 80);
CREATE INDEX idx_auth_sessions_user ON auth_sessions (user_id);
CREATE INDEX idx_auth_sessions_expires_at ON auth_sessions (expires_at);

CREATE TABLE revoke_all_challenges (
    token      TEXT   PRIMARY KEY,
    user_id    TEXT   NOT NULL,
    issued_at  BIGINT NOT NULL,
    expires_at BIGINT NOT NULL
);
CREATE INDEX idx_revoke_all_challenges_expires_at ON revoke_all_challenges (expires_at);

CREATE TABLE enrollment_codes (
    spelling      TEXT   PRIMARY KEY,
    user_id       TEXT   NOT NULL,
    code          TEXT   NOT NULL,
    text_fallback TEXT   NOT NULL,
    issued_at     BIGINT NOT NULL,
    expires_at    BIGINT NOT NULL
);
CREATE INDEX idx_enrollment_codes_expires_at ON enrollment_codes (expires_at);

CREATE TABLE enrollment_channels (
    channel_id        TEXT   PRIMARY KEY,
    initiator_user_id TEXT   NOT NULL,
    opened_at         BIGINT NOT NULL,
    expires_at        BIGINT NOT NULL
);
CREATE INDEX idx_enrollment_channels_expires_at ON enrollment_channels (expires_at);

CREATE TABLE enrollment_mailbox (
    channel_id TEXT   NOT NULL REFERENCES enrollment_channels (channel_id) ON DELETE CASCADE,
    direction  TEXT   NOT NULL CHECK (direction IN ('a', 'b')),
    seq        BIGINT GENERATED ALWAYS AS IDENTITY,
    payload    TEXT   NOT NULL,
    PRIMARY KEY (channel_id, direction, seq)
);

CREATE TABLE oidc_authorizations (
    state             TEXT   PRIMARY KEY,
    nonce             TEXT   NOT NULL,
    verifier          TEXT   NOT NULL,
    redirect_uri      TEXT   NOT NULL,
    binding_challenge TEXT   NOT NULL,
    issued_at         BIGINT NOT NULL,
    expires_at        BIGINT NOT NULL
);
CREATE INDEX idx_oidc_authorizations_expires_at ON oidc_authorizations (expires_at);

CREATE TABLE oidc_device_authorizations (
    handle            TEXT   PRIMARY KEY,
    device_code       TEXT   NOT NULL,
    binding_challenge TEXT   NOT NULL,
    interval_micros   BIGINT NOT NULL CHECK (interval_micros >= 0),
    polled_at         BIGINT NOT NULL,
    issued_at         BIGINT NOT NULL,
    expires_at        BIGINT NOT NULL
);
CREATE INDEX idx_oidc_device_authorizations_expires_at
    ON oidc_device_authorizations (expires_at);

CREATE TABLE upload_sessions (
    upload_id         TEXT     PRIMARY KEY,
    asset_id          TEXT     NOT NULL,
    owner_id          TEXT     NOT NULL,
    upload_user_id    TEXT     NOT NULL,
    album_id          TEXT,
    content_type      TEXT,
    expected_hash     TEXT     NOT NULL,
    crypto_suite_id   INTEGER  NOT NULL CHECK (crypto_suite_id BETWEEN 0 AND 65535),
    protocol_version  TEXT     NOT NULL,
    blob_role         TEXT     NOT NULL,
    intent_id         TEXT,
    manifest_envelope TEXT     NOT NULL,
    committed         TEXT     NOT NULL,
    in_flight_cap     SMALLINT NOT NULL CHECK (in_flight_cap BETWEEN 0 AND 255),
    total_size        BIGINT   NOT NULL CHECK (total_size >= 0),
    status            TEXT     NOT NULL CHECK (status IN (
                          'pending', 'uploading', 'waiting_for_processing',
                          'completed', 'failed_processing')),
    failure_code      TEXT,
    created_at        BIGINT   NOT NULL,
    last_progress_at  BIGINT   NOT NULL,
    expires_at        BIGINT   NOT NULL
) WITH (fillfactor = 70);
CREATE INDEX idx_upload_sessions_uploader ON upload_sessions (upload_user_id);
CREATE INDEX idx_upload_sessions_uploader_hash ON upload_sessions (upload_user_id, expected_hash);
CREATE INDEX idx_upload_sessions_hash ON upload_sessions (expected_hash);
CREATE INDEX idx_upload_sessions_album ON upload_sessions (album_id) WHERE album_id IS NOT NULL;
CREATE INDEX idx_upload_sessions_claimed ON upload_sessions (created_at)
    WHERE status = 'waiting_for_processing';
CREATE INDEX idx_upload_sessions_expires_at ON upload_sessions (expires_at);

CREATE TABLE rate_counters (
    kind       TEXT   NOT NULL,
    scope      TEXT   NOT NULL,
    opened_at  BIGINT NOT NULL,
    hits       BIGINT NOT NULL CHECK (hits >= 0),
    expires_at BIGINT NOT NULL,
    PRIMARY KEY (kind, scope)
);
CREATE INDEX idx_rate_counters_kind_expires_at ON rate_counters (kind, expires_at);
CREATE INDEX idx_rate_counters_expires_at ON rate_counters (expires_at);

CREATE TABLE federation_pull_schedules (
    peer_id     TEXT    NOT NULL,
    album_id    TEXT    NOT NULL,
    next_due    BIGINT,
    last_pull   BIGINT,
    failures    INTEGER NOT NULL DEFAULT 0 CHECK (failures >= 0),
    holder      TEXT,
    lease_until BIGINT,
    expires_at  BIGINT  NOT NULL,
    PRIMARY KEY (peer_id, album_id)
);
CREATE INDEX idx_federation_pull_schedules_expires_at ON federation_pull_schedules (expires_at);

CREATE TABLE federation_pull_circuits (
    peer_id    TEXT   PRIMARY KEY,
    open_until BIGINT NOT NULL
);
CREATE INDEX idx_federation_pull_circuits_open_until ON federation_pull_circuits (open_until);
";

const DOWN: &str = r"
DROP TABLE IF EXISTS federation_pull_circuits;
DROP TABLE IF EXISTS federation_pull_schedules;
DROP TABLE IF EXISTS rate_counters;
DROP TABLE IF EXISTS upload_sessions;
DROP TABLE IF EXISTS oidc_device_authorizations;
DROP TABLE IF EXISTS oidc_authorizations;
DROP TABLE IF EXISTS enrollment_mailbox;
DROP TABLE IF EXISTS enrollment_channels;
DROP TABLE IF EXISTS enrollment_codes;
DROP TABLE IF EXISTS revoke_all_challenges;
DROP TABLE IF EXISTS auth_sessions;
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
