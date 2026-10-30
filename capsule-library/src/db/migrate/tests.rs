//! Test contract for the client catalog migrator.
//!
//! Shape of the chain
//!   - `steps_form_a_contiguous_chain` — one step per version from `BASELINE_VERSION` to
//!     `SCHEMA_VERSION`, each `to == from + 1`, names unique, no downgrade step.
//!   - `shipped_steps_are_immutable` — every shipped step still hashes to its checked-in
//!     fingerprint.
//!
//! Fresh and current
//!   - `fresh_catalog_is_created_at_the_current_version`
//!   - `an_already_current_catalog_applies_no_steps`
//!   - `migration_is_idempotent`
//!
//! Historical fixtures (each reconstructed from git, not inferred)
//!   - `migrating_from_each_historical_version_reaches_the_current_version`
//!   - `a_migrated_catalog_is_structurally_identical_to_a_fresh_one` — the core invariant:
//!     every historical dialect converges on the shape `DDL` creates.
//!   - `v1_blake3_catalog_is_renamed_and_keeps_its_rows`
//!   - `v1_already_renamed_catalog_migrates_without_a_rename` — the unstamped-rename hazard.
//!   - `v2_lifecycle_dialect_gains_albums` / `v2_ios_dialect_gains_cached_representations`
//!     — the two colliding v2 stamps.
//!   - `v3_rows_default_to_not_hidden`
//!   - `v4_rows_gain_no_caption_and_no_location`
//!   - `v5_catalog_gains_the_album_recency_index_and_keeps_its_rows`
//!   - `an_unstamped_catalog_is_adopted_at_the_baseline_version`
//!
//! Acceptance (the "done when")
//!   - `each_historical_library_opens_with_correct_default_and_gated_projections`
//!
//! Refusal and atomicity
//!   - `a_catalog_newer_than_this_build_is_refused`
//!   - `refusal_leaves_the_catalog_untouched`
//!   - `a_failing_step_rolls_back_and_leaves_the_previous_version`

use std::collections::BTreeMap;

use rusqlite::Connection;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::*;
use crate::db::DatabaseDriver;
use crate::db::rows::AssetRow;

// ── Historical schemas, reconstructed from `git log -p` ──────────────────
//
// Fragments rather than whole files so the differences between versions are the only thing
// written down. Provenance is in the module docs' history table.

/// The v1 `assets` table, with the original BLAKE3 hash column (`6f691c7`).
const ASSETS_V1_BLAKE3: &str = "
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
";

/// The same table after the rename that shipped inside v1 (`1776c04`).
const ASSETS_V1_SHA256: &str = "
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
    deleted_at        INTEGER
);
";

/// Tables unchanged from v1 through v4.
const CORE_TABLES_V1: &str = "
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
";

/// v1 indexes over the BLAKE3 hash column, with the narrow timeline index.
const INDEXES_V1_BLAKE3: &str = "
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
";

const INDEXES_V1_SHA256: &str = "
CREATE INDEX IF NOT EXISTS idx_assets_hash       ON assets(hash_sha256);
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
";

/// The `cached_representations` block that the lifecycle-index branch stamped v2 for.
const CACHE_BLOCK: &str = "
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
";

/// The `albums` block that the iOS branch independently stamped v2 for.
const ALBUMS_BLOCK: &str = "
CREATE TABLE IF NOT EXISTS albums (
    id              TEXT    PRIMARY KEY,
    name            TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    modified_at     INTEGER NOT NULL,
    cover_asset_id  TEXT
);

CREATE INDEX IF NOT EXISTS idx_assets_type       ON assets(asset_type);
CREATE INDEX IF NOT EXISTS idx_albums_created    ON albums(created_at);
";

/// The `embeddings` block added at v3.
const EMBEDDINGS_BLOCK: &str = "
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

/// The v4 additions over v3: `assets.is_hidden`, its index, and the widened timeline index
/// (`5d3ef26`). Applied on top of the v3 shape, as the v4 step itself does.
const HIDDEN_BLOCK_V4: &str = "
ALTER TABLE assets ADD COLUMN is_hidden INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS idx_assets_hidden     ON assets(is_hidden);
DROP INDEX IF EXISTS idx_assets_timeline;
CREATE INDEX IF NOT EXISTS idx_assets_timeline   ON assets(is_deleted, is_stack_hidden, is_hidden, capture_utc, capture_timestamp);
";

/// The v5 additions over v4: the caption and effective-location projections.
const CAPTION_BLOCK_V5: &str = "
ALTER TABLE assets ADD COLUMN caption TEXT;
ALTER TABLE assets ADD COLUMN latitude REAL;
ALTER TABLE assets ADD COLUMN longitude REAL;
";

/// A historical catalog shape: the DDL a build of that era created, and the stamp it wrote.
#[derive(Clone, Copy)]
struct Fixture {
    /// Label used in assertion messages.
    label: &'static str,
    stamp: u32,
    /// Name of the hash column at that shape, for row-level fixtures.
    hash_column: &'static str,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        label: "v1 (blake3, pre-rename)",
        stamp: 1,
        hash_column: "hash_blake3",
    },
    Fixture {
        label: "v1 (sha256, post unstamped rename)",
        stamp: 1,
        hash_column: "hash_sha256",
    },
    Fixture {
        label: "v2 (lifecycle-index dialect: cache, no albums)",
        stamp: 2,
        hash_column: "hash_sha256",
    },
    Fixture {
        label: "v2 (ios dialect: albums, no cache)",
        stamp: 2,
        hash_column: "hash_sha256",
    },
    Fixture {
        label: "v3",
        stamp: 3,
        hash_column: "hash_sha256",
    },
    Fixture {
        label: "v4",
        stamp: 4,
        hash_column: "hash_sha256",
    },
    Fixture {
        label: "v5",
        stamp: 5,
        hash_column: "hash_sha256",
    },
];

fn fixture_ddl(f: &Fixture) -> String {
    match (f.label, f.stamp) {
        ("v1 (blake3, pre-rename)", _) => {
            format!("{ASSETS_V1_BLAKE3}{CORE_TABLES_V1}{INDEXES_V1_BLAKE3}")
        }
        ("v1 (sha256, post unstamped rename)", _) => {
            format!("{ASSETS_V1_SHA256}{CORE_TABLES_V1}{INDEXES_V1_SHA256}")
        }
        ("v2 (lifecycle-index dialect: cache, no albums)", _) => {
            format!("{ASSETS_V1_SHA256}{CORE_TABLES_V1}{INDEXES_V1_SHA256}{CACHE_BLOCK}")
        }
        ("v2 (ios dialect: albums, no cache)", _) => {
            format!("{ASSETS_V1_SHA256}{CORE_TABLES_V1}{INDEXES_V1_SHA256}{ALBUMS_BLOCK}")
        }
        ("v4", _) => format!(
            "{ASSETS_V1_SHA256}{CORE_TABLES_V1}{INDEXES_V1_SHA256}{CACHE_BLOCK}{ALBUMS_BLOCK}{EMBEDDINGS_BLOCK}{HIDDEN_BLOCK_V4}"
        ),
        ("v5", _) => format!(
            "{ASSETS_V1_SHA256}{CORE_TABLES_V1}{INDEXES_V1_SHA256}{CACHE_BLOCK}{ALBUMS_BLOCK}{EMBEDDINGS_BLOCK}{HIDDEN_BLOCK_V4}{CAPTION_BLOCK_V5}"
        ),
        _ => format!(
            "{ASSETS_V1_SHA256}{CORE_TABLES_V1}{INDEXES_V1_SHA256}{CACHE_BLOCK}{ALBUMS_BLOCK}{EMBEDDINGS_BLOCK}"
        ),
    }
}

/// Build a historical catalog in memory, stamped as that era's build would have stamped it.
fn historical_conn(f: &Fixture) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&fixture_ddl(f)).unwrap();
    conn.execute_batch(&format!("PRAGMA user_version = {};", f.stamp))
        .unwrap();
    conn
}

/// Structural fingerprint of a catalog: every table's columns (name, type, NOT NULL,
/// default, PK position) and every index (uniqueness, origin, column list).
///
/// Compared instead of raw `sqlite_master` text because `ALTER TABLE` rewrites that text
/// and because the migrator's statements are not formatted identically to the DDL's.
/// Whitespace is not the contract; shape is.
fn schema_shape(conn: &Connection) -> BTreeMap<String, Vec<String>> {
    let mut tables = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master
                     WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                     ORDER BY name",
            )
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            tables.push(row.get::<_, String>(0).unwrap());
        }
    }

    let mut shape = BTreeMap::new();
    for table in tables {
        let mut facts = Vec::new();

        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            let cid: i64 = row.get(0).unwrap();
            let name: String = row.get(1).unwrap();
            let ty: String = row.get(2).unwrap();
            let notnull: i64 = row.get(3).unwrap();
            let dflt: Option<String> = row.get(4).unwrap();
            let pk: i64 = row.get(5).unwrap();
            facts.push(format!(
                "col {cid} {name} {ty} notnull={notnull} default={dflt:?} pk={pk}"
            ));
        }

        let mut index_names = Vec::new();
        {
            let mut stmt = conn
                .prepare(&format!("PRAGMA index_list({table})"))
                .unwrap();
            let mut rows = stmt.query([]).unwrap();
            while let Some(row) = rows.next().unwrap() {
                let name: String = row.get(1).unwrap();
                let unique: i64 = row.get(2).unwrap();
                let origin: String = row.get(3).unwrap();
                index_names.push((name, unique, origin));
            }
        }
        index_names.sort();
        for (name, unique, origin) in index_names {
            let mut cols = Vec::new();
            let mut stmt = conn.prepare(&format!("PRAGMA index_info({name})")).unwrap();
            let mut rows = stmt.query([]).unwrap();
            while let Some(row) = rows.next().unwrap() {
                let col: Option<String> = row.get(2).unwrap();
                cols.push(col.unwrap_or_else(|| "<expr>".to_string()));
            }
            facts.push(format!(
                "index {name} unique={unique} origin={origin} cols={cols:?}"
            ));
        }

        shape.insert(table, facts);
    }
    shape
}

fn current_shape() -> BTreeMap<String, Vec<String>> {
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn).unwrap();
    schema_shape(&conn)
}

fn insert_historical_asset(
    conn: &Connection,
    f: &Fixture,
    uuid: &str,
    hash: &str,
    album: Option<&str>,
) {
    let col = f.hash_column;
    conn.execute(
        &format!(
            "INSERT INTO assets (uuid, asset_type, capture_timestamp, capture_utc,
                 import_timestamp, {col}, album_id, rating, is_deleted, is_stack_hidden)
                 VALUES (?1, 'photo', 1720000000, 1719997200, 1720000000, ?2, ?3, 0, 0, 0)"
        ),
        rusqlite::params![uuid, hash, album],
    )
    .unwrap();
}

// ── Shape of the chain ──────────────────────────────────────────────────

#[test]
fn steps_form_a_contiguous_chain() {
    assert!(!STEPS.is_empty(), "at least one step must be registered");
    assert_eq!(
        STEPS[0].from, BASELINE_VERSION,
        "the chain must start at the baseline version"
    );
    assert_eq!(
        STEPS[STEPS.len() - 1].to,
        SCHEMA_VERSION,
        "the chain must end at SCHEMA_VERSION; bumping the version needs a new step"
    );

    let mut names = std::collections::BTreeSet::new();
    for (i, step) in STEPS.iter().enumerate() {
        assert_eq!(
            step.to,
            step.from + 1,
            "step {} is not a single-version step",
            step.name
        );
        assert!(step.from < step.to, "there is no downgrade step");
        assert!(
            names.insert(step.name),
            "duplicate step name: {}",
            step.name
        );
        if i > 0 {
            assert_eq!(
                STEPS[i - 1].to,
                step.from,
                "gap in the migration chain before {}",
                step.name
            );
        }
    }
}

/// A step that has shipped is immutable — see the module docs. This is the mechanism, not
/// the prose: editing a shipped step's DDL, its guard, or its name changes its fingerprint.
#[test]
fn shipped_steps_are_immutable() {
    assert_eq!(
        STEPS.len(),
        STEP_FINGERPRINTS.len(),
        "every step needs a fingerprint; a new step appends one, it never edits an existing one"
    );
    for (step, expected) in STEPS.iter().zip(STEP_FINGERPRINTS) {
        let actual = hex::encode(Sha256::digest(step.fingerprint_input().as_bytes()));
        assert_eq!(
            &actual, expected,
            "migration step `{}` has been modified after shipping. A shipped step is \
                 immutable: some catalog has already been walked through the old text, so \
                 changing it now makes one user_version describe two different schemas. \
                 Add the NEXT step instead and bump SCHEMA_VERSION. (If this step has genuinely \
                 never shipped, update its fingerprint to {actual}.)",
            step.name
        );
    }
}

// ── Fresh and current ───────────────────────────────────────────────────

#[test]
fn fresh_catalog_is_created_at_the_current_version() {
    let conn = Connection::open_in_memory().unwrap();
    let outcome = migrate(&conn).unwrap();
    assert!(outcome.created, "an empty catalog is created, not migrated");
    assert_eq!(outcome.from, 0);
    assert_eq!(outcome.to, SCHEMA_VERSION);
    assert!(outcome.applied.is_empty(), "no steps run on a fresh create");
    assert_eq!(read_user_version(&conn).unwrap(), SCHEMA_VERSION);
}

#[test]
fn an_already_current_catalog_applies_no_steps() {
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn).unwrap();
    let second = migrate(&conn).unwrap();
    assert!(!second.created);
    assert_eq!(second.from, SCHEMA_VERSION);
    assert!(second.applied.is_empty());
}

#[test]
fn migration_is_idempotent() {
    for f in FIXTURES {
        let conn = historical_conn(f);
        let first = migrate(&conn).unwrap();
        assert!(!first.applied.is_empty(), "{}: expected steps", f.label);
        let shape_once = schema_shape(&conn);

        let second = migrate(&conn).unwrap();
        assert!(
            second.applied.is_empty(),
            "{}: re-running must be a no-op",
            f.label
        );
        assert_eq!(shape_once, schema_shape(&conn), "{}", f.label);
    }
}

// ── Historical fixtures ─────────────────────────────────────────────────

#[test]
fn migrating_from_each_historical_version_reaches_the_current_version() {
    for f in FIXTURES {
        let conn = historical_conn(f);
        let outcome = migrate(&conn).unwrap();
        assert_eq!(outcome.from, f.stamp, "{}", f.label);
        assert_eq!(outcome.to, SCHEMA_VERSION, "{}", f.label);
        assert_eq!(
            outcome.applied.len() as u32,
            SCHEMA_VERSION - f.stamp,
            "{}: one step per version",
            f.label
        );
        assert_eq!(
            read_user_version(&conn).unwrap(),
            SCHEMA_VERSION,
            "{}",
            f.label
        );
    }
}

/// The invariant that makes the migrator trustworthy: every historical dialect, including
/// both colliding v2 stamps, converges on exactly the shape `schema::DDL` creates.
#[test]
fn a_migrated_catalog_is_structurally_identical_to_a_fresh_one() {
    let expected = current_shape();
    for f in FIXTURES {
        let conn = historical_conn(f);
        migrate(&conn).unwrap();
        let actual = schema_shape(&conn);
        assert_eq!(
            actual, expected,
            "{}: migrated catalog does not match a freshly created one",
            f.label
        );
    }
}

#[test]
fn v1_blake3_catalog_is_renamed_and_keeps_its_rows() {
    let f = FIXTURES[0];
    let conn = historical_conn(&f);
    insert_historical_asset(&conn, &f, "uuid-1", &"a".repeat(64), None);
    migrate(&conn).unwrap();

    assert!(has_column(&conn, "assets", "hash_sha256").unwrap());
    assert!(!has_column(&conn, "assets", "hash_blake3").unwrap());
    let hash: String = conn
        .query_row(
            "SELECT hash_sha256 FROM assets WHERE uuid = 'uuid-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hash, "a".repeat(64), "the rename must preserve the value");
}

/// The rename shipped inside v1 without a bump, so a v1-stamped catalog may already have
/// `hash_sha256`. The step must not blow up trying to rename a column that is not there.
#[test]
fn v1_already_renamed_catalog_migrates_without_a_rename() {
    let f = FIXTURES[1];
    let conn = historical_conn(&f);
    insert_historical_asset(&conn, &f, "uuid-1", &"b".repeat(64), None);
    migrate(&conn).unwrap();
    let hash: String = conn
        .query_row(
            "SELECT hash_sha256 FROM assets WHERE uuid = 'uuid-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hash, "b".repeat(64));
}

/// Two branches stamped 2 for different additions. A v2 catalog from the lifecycle-index
/// branch has `cached_representations` but no `albums`; the 2 -> 3 step must supply it.
#[test]
fn v2_lifecycle_dialect_gains_albums() {
    let f = FIXTURES[2];
    let conn = historical_conn(&f);
    assert!(!table_exists(&conn, "albums"));
    migrate(&conn).unwrap();
    assert!(table_exists(&conn, "albums"));
    assert!(table_exists(&conn, "cached_representations"));
}

/// The mirror image: a v2 catalog from the iOS branch has `albums` but no
/// `cached_representations`.
#[test]
fn v2_ios_dialect_gains_cached_representations() {
    let f = FIXTURES[3];
    let conn = historical_conn(&f);
    assert!(!table_exists(&conn, "cached_representations"));
    migrate(&conn).unwrap();
    assert!(table_exists(&conn, "cached_representations"));
    assert!(table_exists(&conn, "albums"));
}

/// A catalog whose sidecars predate the `hidden` register reads as "nothing is hidden" —
/// not as a random subset of the library vanishing from the timeline.
#[test]
fn v3_rows_default_to_not_hidden() {
    let f = FIXTURES[4];
    let conn = historical_conn(&f);
    insert_historical_asset(&conn, &f, "uuid-1", &"c".repeat(64), None);
    migrate(&conn).unwrap();
    let hidden: i64 = conn
        .query_row(
            "SELECT is_hidden FROM assets WHERE uuid = 'uuid-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hidden, 0);
}

/// A catalog whose rows predate the caption and location projections reads as "no caption,
/// no location" — `NULL`, which the row mapper reads back as `None` — and keeps its `hidden`
/// projection across the step.
#[test]
fn v4_rows_gain_no_caption_and_no_location() {
    let f = FIXTURES[5];
    let conn = historical_conn(&f);
    insert_historical_asset(&conn, &f, "uuid-1", &"e".repeat(64), None);
    conn.execute_batch("UPDATE assets SET is_hidden = 1 WHERE uuid = 'uuid-1';")
        .unwrap();
    let outcome = migrate(&conn).unwrap();
    assert_eq!(
        outcome.applied.first(),
        Some(&Applied {
            from: 4,
            to: 5,
            name: "v4_to_v5_caption_and_location"
        })
    );
    let (caption, lat, lon, hidden): (Option<String>, Option<f64>, Option<f64>, i64) = conn
        .query_row(
            "SELECT caption, latitude, longitude, is_hidden FROM assets WHERE uuid = 'uuid-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!((caption, lat, lon), (None, None, None));
    assert_eq!(hidden, 1, "the step must not touch an existing projection");
}

/// The v6 step adds the album-recency index and touches no row: a v5 catalog's assets keep
/// every projection, and the album order reads them through the new index.
#[test]
fn v5_catalog_gains_the_album_recency_index_and_keeps_its_rows() {
    let f = FIXTURES[6];
    let conn = historical_conn(&f);
    insert_historical_asset(&conn, &f, "uuid-1", &"f".repeat(64), Some("album-1"));
    conn.execute_batch(
        "UPDATE assets SET is_hidden = 1, caption = 'kept', latitude = 1.5, longitude = 2.5
         WHERE uuid = 'uuid-1';",
    )
    .unwrap();
    let outcome = migrate(&conn).unwrap();
    assert_eq!(
        outcome.applied,
        vec![Applied {
            from: 5,
            to: 6,
            name: "v5_to_v6_album_recency_index"
        }]
    );
    let index: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_assets_album_recency'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(index, 1);
    let row: (
        Option<String>,
        i64,
        Option<String>,
        Option<f64>,
        Option<f64>,
    ) = conn
        .query_row(
            "SELECT album_id, is_hidden, caption, latitude, longitude FROM assets
             WHERE uuid = 'uuid-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (
            Some("album-1".to_string()),
            1,
            Some("kept".to_string()),
            Some(1.5),
            Some(2.5)
        ),
        "the step must not touch an existing row"
    );
}

#[test]
fn an_unstamped_catalog_is_adopted_at_the_baseline_version() {
    let f = FIXTURES[0];
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&fixture_ddl(&f)).unwrap();
    // Deliberately not stamped: user_version stays 0 while tables exist.
    assert_eq!(read_user_version(&conn).unwrap(), 0);

    let outcome = migrate(&conn).unwrap();
    assert!(!outcome.created, "a populated catalog is never re-created");
    assert_eq!(outcome.from, BASELINE_VERSION);
    assert_eq!(outcome.to, SCHEMA_VERSION);
    assert_eq!(schema_shape(&conn), current_shape());
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        rusqlite::params![name],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

// ── Acceptance ────────────────────────────────────────────

/// The "done when": a library created at each historical `user_version` opens at
/// the current version with every column present, and both the default projections
/// (timeline, filtered timeline, album) and the gated Hidden projection are correct.
///
/// Deliberately goes through [`DatabaseDriver::open`] on a real file rather than calling
/// the migrator directly: that is the path an upgraded app takes into a user's library.
#[test]
fn each_historical_library_opens_with_correct_default_and_gated_projections() {
    let tmp = TempDir::new().unwrap();
    for (i, f) in FIXTURES.iter().enumerate() {
        let path = tmp.path().join(format!("lib-{i}.sqlite"));
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&fixture_ddl(f)).unwrap();
            conn.execute_batch(&format!("PRAGMA user_version = {};", f.stamp))
                .unwrap();
            insert_historical_asset(&conn, f, "uuid-visible", &"a".repeat(64), Some("album-1"));
            insert_historical_asset(&conn, f, "uuid-hidden", &"b".repeat(64), Some("album-1"));
        }

        // The upgrade: an app built at SCHEMA_VERSION opens a library built earlier.
        let db = DatabaseDriver::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION, "{}", f.label);

        // Every column the current row mapper reads is present and readable.
        let visible: AssetRow = db.find_by_uuid("uuid-visible").unwrap().unwrap();
        assert_eq!(visible.hash_sha256, "a".repeat(64), "{}", f.label);
        assert!(!visible.is_hidden, "{}", f.label);
        assert_eq!(visible.album_id.as_deref(), Some("album-1"), "{}", f.label);

        // Pre-migration rows are visible in every default projection.
        assert_eq!(db.query_timeline(0, 100).unwrap().len(), 2, "{}", f.label);
        assert!(db.query_hidden(0, 100).unwrap().is_empty(), "{}", f.label);

        // Pre-migration rows carry no caption and no location.
        assert_eq!(visible.caption, None, "{}", f.label);
        assert_eq!(
            (visible.latitude, visible.longitude),
            (None, None),
            "{}",
            f.label
        );

        // The write paths added at v4 and v5 work against the migrated columns.
        db.update_asset_hidden("uuid-hidden", true).unwrap();
        assert!(
            db.update_asset_caption("uuid-visible", Some("kept"))
                .unwrap(),
            "{}",
            f.label
        );
        let captioned = db.find_by_uuid("uuid-visible").unwrap().unwrap();
        assert_eq!(captioned.caption.as_deref(), Some("kept"), "{}", f.label);

        let timeline = db.query_timeline(0, 100).unwrap();
        assert_eq!(timeline.len(), 1, "{}: default timeline", f.label);
        assert_eq!(timeline[0].uuid, "uuid-visible", "{}", f.label);

        let filtered = db
            .query_timeline_filtered(Some("photo"), None, None, 0, 100)
            .unwrap();
        assert_eq!(filtered.len(), 1, "{}: filtered timeline", f.label);

        let album = db.query_album_assets("album-1", 0, 100).unwrap();
        assert_eq!(album.len(), 1, "{}: album projection", f.label);

        let gated = db.query_hidden(0, 100).unwrap();
        assert_eq!(gated.len(), 1, "{}: gated Hidden view", f.label);
        assert_eq!(gated[0].uuid, "uuid-hidden", "{}", f.label);
        assert!(gated[0].is_hidden, "{}", f.label);

        // Hiding is view-layer only: the row stays reachable by uuid and by hash.
        assert!(
            db.find_by_uuid("uuid-hidden").unwrap().is_some(),
            "{}",
            f.label
        );
        assert!(
            db.find_by_hash(&"b".repeat(64)).unwrap().is_some(),
            "{}",
            f.label
        );

        // Tables one or other historical dialect lacked are usable after the walk.
        assert!(table_exists(db.connection(), "albums"), "{}", f.label);
        assert!(
            table_exists(db.connection(), "cached_representations"),
            "{}",
            f.label
        );
        assert!(table_exists(db.connection(), "embeddings"), "{}", f.label);
    }
}

// ── Refusal and atomicity ───────────────────────────────────────────────

/// A catalog stamped newer than this build is refused, not downgraded and not opened.
/// Forward-only settles the direction of migration; this settles the other direction, and
/// it is the first thing a user hits when they install an older app build.
#[test]
fn a_catalog_newer_than_this_build_is_refused() {
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn).unwrap();
    conn.execute_batch(&format!("PRAGMA user_version = {};", SCHEMA_VERSION + 1))
        .unwrap();

    let err = migrate(&conn).unwrap_err();
    match err {
        MigrationError::CatalogTooNew { found, supported } => {
            assert_eq!(found, SCHEMA_VERSION + 1);
            assert_eq!(supported, SCHEMA_VERSION);
        }
        other => panic!("expected CatalogTooNew, got {other:?}"),
    }
}

#[test]
fn refusal_leaves_the_catalog_untouched() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("future.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        migrate(&conn).unwrap();
        // A future build's column this one knows nothing about.
        conn.execute_batch("ALTER TABLE assets ADD COLUMN future_flag INTEGER NOT NULL DEFAULT 7;")
            .unwrap();
        conn.execute_batch(&format!("PRAGMA user_version = {};", SCHEMA_VERSION + 2))
            .unwrap();
    }

    let conn = Connection::open(&path).unwrap();
    assert!(migrate(&conn).is_err());
    assert_eq!(
        read_user_version(&conn).unwrap(),
        SCHEMA_VERSION + 2,
        "the stamp must not be rewritten"
    );
    assert!(
        has_column(&conn, "assets", "future_flag").unwrap(),
        "the newer schema must be left intact"
    );

    // And the same refusal reaches the driver, which is how an app actually sees it.
    assert!(DatabaseDriver::open(&path).is_err());
}

/// Each step commits atomically. A step that fails part-way leaves the catalog at its
/// previous version, so the next open retries the same step rather than resuming from a
/// half-applied one.
#[test]
fn a_failing_step_rolls_back_and_leaves_the_previous_version() {
    let f = FIXTURES[4]; // v3
    let conn = historical_conn(&f);
    insert_historical_asset(&conn, &f, "uuid-1", &"d".repeat(64), None);
    // Poison the 3 -> 4 step: an object already occupies the name its index needs, which
    // `CREATE INDEX IF NOT EXISTS` cannot work around.
    conn.execute_batch("CREATE TABLE idx_assets_hidden (x INTEGER);")
        .unwrap();

    let err = migrate(&conn).unwrap_err();
    match err {
        MigrationError::Step { from, to, name, .. } => {
            assert_eq!((from, to), (3, 4));
            assert_eq!(name, "v3_to_v4_hidden_assets");
        }
        other => panic!("expected Step failure, got {other:?}"),
    }

    assert_eq!(
        read_user_version(&conn).unwrap(),
        3,
        "a failed step must not advance the stamp"
    );
    assert!(
        !has_column(&conn, "assets", "is_hidden").unwrap(),
        "a failed step must roll back its own DDL"
    );
}
