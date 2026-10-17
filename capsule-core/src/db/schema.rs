/// SQLite catalog schema version, stamped into the database's `PRAGMA user_version`.
///
/// This DDL is the shape of a **fresh** catalog only. An existing catalog is brought here by
/// the forward-only stepwise migrator in [`crate::db::migrate`], which also carries the
/// authoritative version-by-version history (reconstructed from git; the list below is a
/// summary and the migrator's table is the SSoT where they disagree).
///
/// v2: `assets.hash_blake3` renamed to `hash_sha256` — the project moved from
///     BLAKE3 to SHA-256 (hardware-accelerated on Apple and modern CPUs).
///     Added the client-side `albums` table for user-defined album metadata.
///     (The rename actually shipped *inside* v1 and `albums` arrived from a branch that had
///     independently stamped 2 — see [`crate::db::migrate`]; both steps are conditional
///     because of it.)
/// v3: added the `embeddings` provenance companion table (S-H1) — the per-task
///     `sqlite-vec` `vec0` tables are created at runtime from the model registry
///     (their vector dimension is registry-declared), so they are not in this DDL.
/// v4: added `assets.is_hidden` (S-D19) — the index projection of the sidecar `hidden`
///     LWW register. Hidden assets are excluded from every default view and are reachable
///     only through the gated Hidden view (SSoT: design/organization § Hidden Assets).
///     Distinct from `is_stack_hidden`, which suppresses non-primary stack members.
///
/// **Bumping this constant requires appending a step to `db::migrate::STEPS`** —
/// `steps_form_a_contiguous_chain` fails otherwise. Never edit a step that has shipped.
pub(crate) const SCHEMA_VERSION: u32 = 4;

pub(crate) const DDL: &str = r"
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS assets (
    uuid              TEXT    PRIMARY KEY,
    asset_type        TEXT    NOT NULL,
    capture_timestamp INTEGER NOT NULL DEFAULT 0,
    capture_utc       INTEGER,
    capture_tz_source TEXT,
    import_timestamp  INTEGER NOT NULL,
    hash_sha256       TEXT    NOT NULL,
    width             INTEGER,
    height            INTEGER,
    duration_ms       INTEGER,
    stack_id          TEXT,
    is_stack_hidden   INTEGER NOT NULL DEFAULT 0,
    chromahash        TEXT,
    dominant_color    TEXT,
    album_id          TEXT,
    rating            INTEGER NOT NULL DEFAULT 0,
    is_deleted        INTEGER NOT NULL DEFAULT 0,
    deleted_at        INTEGER,
    -- Projection of the sidecar `hidden` LWW register: excluded from default views,
    -- served only by the gated Hidden view. Not `is_stack_hidden` (stack suppression).
    is_hidden         INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS asset_stacks (
    id                TEXT    PRIMARY KEY,
    stack_type        TEXT    NOT NULL,
    primary_asset_id  TEXT    NOT NULL,
    cover_asset_id    TEXT,
    is_collapsed      INTEGER NOT NULL DEFAULT 1,
    is_auto_generated INTEGER NOT NULL DEFAULT 1,
    created_at        INTEGER NOT NULL,
    modified_at       INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS stack_members (
    id                TEXT    PRIMARY KEY,
    stack_id          TEXT    NOT NULL,
    asset_id          TEXT    NOT NULL,
    sequence_order    INTEGER NOT NULL,
    member_role       TEXT    NOT NULL,
    created_at        INTEGER NOT NULL,
    UNIQUE (stack_id, asset_id)
);

CREATE TABLE IF NOT EXISTS asset_tags (
    uuid  TEXT NOT NULL,
    tag   TEXT NOT NULL,
    PRIMARY KEY (uuid, tag)
);

CREATE TABLE IF NOT EXISTS albums (
    id              TEXT    PRIMARY KEY,
    name            TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    modified_at     INTEGER NOT NULL,
    cover_asset_id  TEXT
);

CREATE INDEX IF NOT EXISTS idx_assets_hash       ON assets(hash_sha256);
CREATE INDEX IF NOT EXISTS idx_assets_utc        ON assets(capture_utc, capture_timestamp);
CREATE INDEX IF NOT EXISTS idx_assets_deleted    ON assets(is_deleted);
CREATE INDEX IF NOT EXISTS idx_assets_album      ON assets(album_id);
CREATE INDEX IF NOT EXISTS idx_assets_stack      ON assets(stack_id);
CREATE INDEX IF NOT EXISTS idx_assets_type       ON assets(asset_type);
CREATE INDEX IF NOT EXISTS idx_assets_hidden     ON assets(is_hidden);
CREATE INDEX IF NOT EXISTS idx_assets_timeline   ON assets(is_deleted, is_stack_hidden, is_hidden, capture_utc, capture_timestamp);
CREATE INDEX IF NOT EXISTS idx_stacks_type       ON asset_stacks(stack_type);
CREATE INDEX IF NOT EXISTS idx_stacks_primary    ON asset_stacks(primary_asset_id);
CREATE INDEX IF NOT EXISTS idx_stack_members_stack  ON stack_members(stack_id);
CREATE INDEX IF NOT EXISTS idx_stack_members_asset  ON stack_members(asset_id);
CREATE INDEX IF NOT EXISTS idx_tags_tag          ON asset_tags(tag);
CREATE INDEX IF NOT EXISTS idx_albums_created    ON albums(created_at);

-- Reclaimable cached representations of an asset (the cache/ tier — thumbnails, previews,
-- transcodes — plus fetched-but-unpinned originals). Eviction is LRU by last_accessed_at,
-- tier original → preview → thumbnail at equal recency; pinned and device-owned originals are
-- exempt. Authoritative local state (re-scannable from disk), untouched by index rebuild.
-- SSoT: design/filesystem/client § Space Recovery.
CREATE TABLE IF NOT EXISTS cached_representations (
    uuid              TEXT    NOT NULL,
    tier              TEXT    NOT NULL,
    format            TEXT,
    bytes             INTEGER NOT NULL,
    path              TEXT    NOT NULL,
    last_accessed_at  INTEGER NOT NULL,
    pinned            INTEGER NOT NULL DEFAULT 0,
    is_owned_original INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (uuid, tier)
);

CREATE INDEX IF NOT EXISTS idx_cache_evict
    ON cached_representations(pinned, is_owned_original, last_accessed_at);

-- Provenance companion to the per-task `sqlite-vec` vec0 tables (created at runtime from the
-- model registry, since their vector dimension is registry-declared). One row per
-- (asset, task, platform): the embedding-provenance tuple (model_id, model_version), the
-- platform partition discriminator, and the vec0 rowid the actual vector lives at. Lets the
-- index find/replace/delete an asset's embedding and surface which entries are stale (their
-- model_version trails the canonical row) without scanning the vector store. Derived state:
-- rebuilt by re-running inference over the originals, never restored from backup.
-- SSoT: design/ai § Embedding Provenance.
CREATE TABLE IF NOT EXISTS embeddings (
    asset_id      TEXT    NOT NULL,
    task          TEXT    NOT NULL,
    platform      TEXT    NOT NULL,
    model_id      TEXT    NOT NULL,
    model_version TEXT    NOT NULL,
    vec_rowid     INTEGER NOT NULL,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (asset_id, task, platform)
);

CREATE INDEX IF NOT EXISTS idx_embeddings_asset ON embeddings(asset_id);
CREATE INDEX IF NOT EXISTS idx_embeddings_task  ON embeddings(task, platform, model_version);
";
