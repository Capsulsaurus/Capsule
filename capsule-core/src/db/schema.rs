pub const SCHEMA_VERSION: u32 = 1;

pub const DDL: &str = r#"
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS assets (
    uuid              TEXT    PRIMARY KEY,
    asset_type        TEXT    NOT NULL,
    capture_timestamp INTEGER NOT NULL DEFAULT 0,
    capture_utc       INTEGER,
    capture_tz_source TEXT,
    import_timestamp  INTEGER NOT NULL,
    hash_blake3       TEXT    NOT NULL,
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
    deleted_at        INTEGER
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

CREATE INDEX IF NOT EXISTS idx_assets_hash       ON assets(hash_blake3);
CREATE INDEX IF NOT EXISTS idx_assets_utc        ON assets(capture_utc, capture_timestamp);
CREATE INDEX IF NOT EXISTS idx_assets_deleted    ON assets(is_deleted);
CREATE INDEX IF NOT EXISTS idx_assets_album      ON assets(album_id);
CREATE INDEX IF NOT EXISTS idx_assets_stack      ON assets(stack_id);
CREATE INDEX IF NOT EXISTS idx_assets_timeline   ON assets(is_deleted, is_stack_hidden, capture_utc, capture_timestamp);
CREATE INDEX IF NOT EXISTS idx_stacks_type       ON asset_stacks(stack_type);
CREATE INDEX IF NOT EXISTS idx_stacks_primary    ON asset_stacks(primary_asset_id);
CREATE INDEX IF NOT EXISTS idx_stack_members_stack  ON stack_members(stack_id);
CREATE INDEX IF NOT EXISTS idx_stack_members_asset  ON stack_members(asset_id);
CREATE INDEX IF NOT EXISTS idx_tags_tag          ON asset_tags(tag);
"#;
