//! Forward-only stepwise migrator for the client SQLite catalog.
//!
//! The catalog (`index/library.sqlite` in the [client library layout]) **is** the user's
//! library as they experience it, so it carries the same "backward-compatible reads forever"
//! obligation as the server schema and is given the same mechanism: a forward-only stepwise
//! migrator keyed on `PRAGMA user_version`, the client-side analogue of the server's
//! forward-only sea-orm migrations. Opening a catalog stamped below [`SCHEMA_VERSION`] walks
//! it up, one step per version, to the current one. There is no downgrade step, for the same
//! reason the server has none — "rollback then continue" is what corrupts data, on a laptop
//! as much as on a server.
//!
//! SSoT: design/versioning § Client Catalog Migration (slice `S-D23`).
//!
//! # Why not drop and rebuild
//!
//! `rebuild_index` is the **repair** path ([Maintenance § Repair]), reached when the index is
//! already known-inconsistent. It is not a release ritual: spending it on every shipped column
//! would re-derive the whole library on each upgrade and re-lose whatever the sidecars do not
//! yet carry (importer-formed stack placement, slice `S-B15`). The migrator is the durability
//! mechanism; rebuild stays the recovery path it was.
//!
//! # A shipped step is immutable
//!
//! **Once a step appears in [`STEPS`], it is never edited — only followed by another step.**
//! Some user's catalog has already been walked through the old text; changing it now means the
//! same `user_version` describes two different schemas, and every later step is then reasoning
//! about a shape that may or may not exist. If a shipped step was wrong, add the *next* step
//! that repairs it and bump [`SCHEMA_VERSION`].
//!
//! This is enforced, not merely asserted: `shipped_steps_are_immutable` in this module's tests
//! locks each step's canonical rendering behind a SHA-256 fingerprint checked into
//! `STEP_FINGERPRINTS`. Editing a shipped step fails that test with an explanation. Adding a
//! new step appends a fingerprint and leaves the existing ones untouched.
//!
//! # What the shipped history actually was
//!
//! Reconstructed from `git log --follow -p capsule-core/src/db/schema.rs` rather than from the
//! version doc comment, which does not match what shipped on the mainline:
//!
//! | stamp | mainline shape |
//! |-------|----------------|
//! | v1    | base tables, `assets.hash_blake3` |
//! | v1    | *still* v1 after `assets.hash_blake3` was renamed to `hash_sha256` — the rename shipped **without a version bump** (`1776c04`) |
//! | v2    | + `cached_representations` + `idx_cache_evict` (`6c74679`, merged `8b3e06b`) |
//! | v2    | *still* v2 after `albums`, `idx_albums_created`, `idx_assets_type` arrived from a branch that had independently bumped 1 → 2 for its own change (`3e19de0`, merged `3ebb16d`) |
//! | v3    | + `embeddings` + its two indexes (`09ae1d1`) |
//! | v4    | + `assets.is_hidden`, `idx_assets_hidden`, widened `idx_assets_timeline` (`5d3ef26`) |
//!
//! Two consequences the slice text did not anticipate, both handled below:
//!
//! 1. **v1 is ambiguous**: a v1-stamped catalog may carry either `hash_blake3` or `hash_sha256`,
//!    so the 1 → 2 step renames *conditionally* rather than unconditionally.
//! 2. **v2 is ambiguous**: two branches stamped 2 for different additions, so a v2-stamped
//!    catalog may be missing `albums` **or** `cached_representations` depending on which build
//!    created it. The 2 → 3 step therefore re-asserts the whole v2 shape before adding its own
//!    tables. Every statement involved is `IF NOT EXISTS`, so this costs nothing on a catalog
//!    that already has them.
//!
//! [client library layout]: https://docs/design/filesystem/client/#desktop-library-layout
//! [Maintenance § Repair]: https://docs/design/filesystem/maintenance/#repair

use rusqlite::Connection;

use crate::db::schema::{DDL, SCHEMA_VERSION};

/// The oldest schema version this migrator can walk from.
///
/// Also the version an *unstamped* catalog (one that already has tables but reports
/// `user_version = 0`) is adopted at — v1 predates nothing, so there is no older shape to
/// mistake it for.
pub(crate) const BASELINE_VERSION: u32 = 1;

/// A single DDL statement (or batch) in a migration step, plus the condition under which it
/// runs.
///
/// Conditions are declarative rather than closures so that a step's entire behaviour is
/// visible in its data, and therefore fingerprintable — see the immutability rule in the
/// module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ddl {
    /// Run unconditionally. Must be idempotent (`IF NOT EXISTS` / `DROP … IF EXISTS`).
    Always(&'static str),
    /// Run only when `table` **has** `column` — used for renames of a column that an
    /// ambiguous historical stamp may or may not still carry.
    IfColumnPresent {
        table: &'static str,
        column: &'static str,
        sql: &'static str,
    },
    /// Run only when `table` **lacks** `column` — used for `ALTER TABLE … ADD COLUMN`, which
    /// is not idempotent on its own.
    IfColumnMissing {
        table: &'static str,
        column: &'static str,
        sql: &'static str,
    },
}

impl Ddl {
    /// Canonical single-line rendering, hashed into the step fingerprint. Whitespace inside
    /// `sql` is preserved deliberately: reformatting a shipped statement is an edit like any
    /// other, and the point of the lock is that shipped text does not move.
    #[cfg(test)]
    fn fingerprint_line(&self) -> String {
        match self {
            Self::Always(sql) => format!("A|{sql}"),
            Self::IfColumnPresent { table, column, sql } => format!("P|{table}.{column}|{sql}"),
            Self::IfColumnMissing { table, column, sql } => format!("M|{table}.{column}|{sql}"),
        }
    }
}

/// One version-to-version migration step. `to` is always `from + 1`: the walk is stepwise, so
/// a catalog four versions behind is brought forward by four separate, separately committed
/// steps rather than one jump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Step {
    pub from: u32,
    pub to: u32,
    /// Stable identifier used in logs and in the immutability fingerprint.
    pub name: &'static str,
    pub ddl: &'static [Ddl],
}

impl Step {
    #[cfg(test)]
    fn fingerprint_input(&self) -> String {
        let mut out = format!("{}->{} {}\n", self.from, self.to, self.name);
        for ddl in self.ddl {
            out.push_str(&ddl.fingerprint_line());
            out.push('\n');
        }
        out
    }
}

// ── The shipped steps ───────────────────────────────────────────────────────
//
// APPEND ONLY. See the module docs: a step that has shipped is never edited.

/// v1 → v2. Canonicalises the hash column and creates the two tables that the two colliding
/// v2 branches introduced between them (`albums` from the iOS branch, `cached_representations`
/// from the lifecycle-index branch).
///
/// The rename is conditional because it shipped inside v1 without a bump: a v1-stamped catalog
/// created before `1776c04` has `hash_blake3`, one created after has `hash_sha256`.
const STEP_1_TO_2: Step = Step {
    from: 1,
    to: 2,
    name: "v1_to_v2_sha256_albums_and_cache",
    ddl: &[
        Ddl::IfColumnPresent {
            table: "assets",
            column: "hash_blake3",
            sql: "ALTER TABLE assets RENAME COLUMN hash_blake3 TO hash_sha256;",
        },
        // SQLite rewrites dependent index definitions on RENAME COLUMN, but the index is
        // recreated explicitly so the resulting shape does not depend on that behaviour.
        Ddl::Always(
            "DROP INDEX IF EXISTS idx_assets_hash;
             CREATE INDEX IF NOT EXISTS idx_assets_hash ON assets(hash_sha256);",
        ),
        Ddl::Always(
            "CREATE TABLE IF NOT EXISTS albums (
                 id              TEXT    PRIMARY KEY,
                 name            TEXT    NOT NULL,
                 created_at      INTEGER NOT NULL,
                 modified_at     INTEGER NOT NULL,
                 cover_asset_id  TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_albums_created ON albums(created_at);
             CREATE INDEX IF NOT EXISTS idx_assets_type ON assets(asset_type);",
        ),
        Ddl::Always(
            "CREATE TABLE IF NOT EXISTS cached_representations (
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
                 ON cached_representations(pinned, is_owned_original, last_accessed_at);",
        ),
    ],
};

/// v2 → v3. Adds the `embeddings` provenance companion table (slice `S-H1`).
///
/// The `vec0` partition tables are *not* created here: their vector dimension is declared by
/// the model registry, so they are created at runtime by their writer (see
/// [`crate::db::vector::VectorTableSpec`]).
///
/// The first two statements re-assert the v2 shape. That is not redundancy: two branches
/// stamped 2 for different additions, so a v2-stamped catalog is missing one of the two tables
/// depending on which build created it, and the stamp cannot tell us which.
const STEP_2_TO_3: Step = Step {
    from: 2,
    to: 3,
    name: "v2_to_v3_embeddings",
    ddl: &[
        Ddl::Always(
            "CREATE TABLE IF NOT EXISTS albums (
                 id              TEXT    PRIMARY KEY,
                 name            TEXT    NOT NULL,
                 created_at      INTEGER NOT NULL,
                 modified_at     INTEGER NOT NULL,
                 cover_asset_id  TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_albums_created ON albums(created_at);
             CREATE INDEX IF NOT EXISTS idx_assets_type ON assets(asset_type);",
        ),
        Ddl::Always(
            "CREATE TABLE IF NOT EXISTS cached_representations (
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
                 ON cached_representations(pinned, is_owned_original, last_accessed_at);",
        ),
        Ddl::Always(
            "CREATE TABLE IF NOT EXISTS embeddings (
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
             CREATE INDEX IF NOT EXISTS idx_embeddings_task  ON embeddings(task, platform, model_version);",
        ),
    ],
};

/// v3 → v4. Adds `assets.is_hidden`, the index projection of the sidecar `hidden` LWW register
/// (slice `S-D19`).
///
/// Existing rows default to `0` — not hidden — which is the correct reading of a catalog whose
/// sidecars predate the register. The gated Hidden view is empty until the sidecars are
/// re-projected, rather than a random subset of the library disappearing from the timeline.
///
/// `idx_assets_timeline` is dropped and recreated rather than left alone: v4 widened it to
/// include `is_hidden`, and `CREATE INDEX IF NOT EXISTS` would silently keep the narrow one.
const STEP_3_TO_4: Step = Step {
    from: 3,
    to: 4,
    name: "v3_to_v4_hidden_assets",
    ddl: &[
        Ddl::IfColumnMissing {
            table: "assets",
            column: "is_hidden",
            sql: "ALTER TABLE assets ADD COLUMN is_hidden INTEGER NOT NULL DEFAULT 0;",
        },
        Ddl::Always(
            "CREATE INDEX IF NOT EXISTS idx_assets_hidden ON assets(is_hidden);
             DROP INDEX IF EXISTS idx_assets_timeline;
             CREATE INDEX IF NOT EXISTS idx_assets_timeline
                 ON assets(is_deleted, is_stack_hidden, is_hidden, capture_utc, capture_timestamp);",
        ),
    ],
};

/// Every shipped step, in ascending order. Append only.
pub(crate) const STEPS: &[Step] = &[STEP_1_TO_2, STEP_2_TO_3, STEP_3_TO_4];

/// SHA-256 (hex) of each shipped step's canonical rendering, parallel to [`STEPS`].
///
/// This is the immutability lock described in the module docs. Appending a step appends a
/// fingerprint; editing a shipped step changes an existing one, which fails
/// `shipped_steps_are_immutable`.
#[cfg(test)]
const STEP_FINGERPRINTS: &[&str] = &[
    // v1 -> v2  v1_to_v2_sha256_albums_and_cache
    "320b317864ff7cdd1a46cc65ec1b864a7b8ae675411521dcb449593bfabd93a4",
    // v2 -> v3  v2_to_v3_embeddings
    "15ff0fc10221e1d01deebaafd9b8855b8a9f59491285379a11bf90ba7756f2f6",
    // v3 -> v4  v3_to_v4_hidden_assets
    "420ac7077cf5dd3d5888ce07594263e8ace4558adc79f8972096d80ef9a6fe14",
];

// ── Errors ──────────────────────────────────────────────────────────────────

/// Why a catalog could not be brought to [`SCHEMA_VERSION`].
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    /// The catalog was written by a newer build than this one.
    ///
    /// **This is a refusal, not a downgrade.** Forward-only settles the direction of
    /// migration; this settles what happens in the other direction. The catalog is left
    /// byte-for-byte untouched — no stamp rewrite, no DDL, no drop — because an older binary
    /// cannot know what invariants the newer schema added, and writing to it would corrupt a
    /// library the user's *current* build can still open perfectly. The recovery is to update
    /// the app, which is always available; there is no recovery from silent divergent writes.
    #[error(
        "catalog schema v{found} is newer than this build supports (v{supported}); \
         update Capsule to open this library"
    )]
    CatalogTooNew { found: u32, supported: u32 },

    /// No step is registered out of `from`. Structurally impossible while
    /// `steps_form_a_contiguous_chain` passes; kept so a gap fails loudly rather than looping.
    #[error("no migration step is registered from catalog schema v{from} (target v{to})")]
    MissingStep { from: u32, to: u32 },

    /// A step failed. The catalog is still stamped `from`: each step commits atomically, so a
    /// failure rolls the whole step back and the next open retries it.
    #[error("catalog migration step v{from} -> v{to} ({name}) failed: {source}")]
    Step {
        from: u32,
        to: u32,
        name: &'static str,
        #[source]
        source: rusqlite::Error,
    },

    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

impl From<MigrationError> for rusqlite::Error {
    /// Flatten into the error type the catalog's callers already handle.
    ///
    /// [`crate::db::DatabaseDriver::open`] is consumed by `capsule-core-ffi` and
    /// `library::open` through `rusqlite::Error`, so the migrator's typed error is flattened
    /// at that boundary rather than rippling a new error type through them. Callers that want
    /// the typed error call [`DatabaseDriver::migrate`](crate::db::DatabaseDriver::migrate).
    fn from(err: MigrationError) -> Self {
        match err {
            MigrationError::Sqlite(inner) => inner,
            // `SqliteFailure` with a message is the only rusqlite variant that renders an
            // arbitrary string verbatim through `Display`, so the migrator's explanation
            // survives the flattening intact.
            other => Self::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some(other.to_string()),
            ),
        }
    }
}

// ── Outcome ─────────────────────────────────────────────────────────────────

/// One applied step, as reported by [`migrate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    pub from: u32,
    pub to: u32,
    pub name: &'static str,
}

/// What [`migrate`] did. Recorded so the CLI, the FFI host, and a support bundle can all say
/// exactly what ran against a user's library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The version the catalog reported on open (`0` for a catalog this call created).
    pub from: u32,
    /// The version it is stamped at now — always [`SCHEMA_VERSION`] on success.
    pub to: u32,
    /// True when the catalog was empty and the current schema was created outright.
    pub created: bool,
    /// The steps applied, in order. Empty for a fresh create and for an already-current open.
    pub applied: Vec<Applied>,
}

// ── The migrator ────────────────────────────────────────────────────────────

/// Bring `conn` to [`SCHEMA_VERSION`], creating the schema outright if the catalog is empty.
///
/// Fresh catalog → the current [`DDL`] is applied in one shot and stamped. Existing catalog →
/// the registered [`STEPS`] are walked upward from its `user_version`, each in its own
/// transaction. Catalog already current → nothing runs. Catalog newer than this build →
/// [`MigrationError::CatalogTooNew`], with nothing written.
#[tracing::instrument(level = "debug", skip_all, fields(target_version = SCHEMA_VERSION))]
pub(crate) fn migrate(conn: &Connection) -> Result<Outcome, MigrationError> {
    let stamped = read_user_version(conn)?;

    if stamped > SCHEMA_VERSION {
        tracing::error!(
            found = stamped,
            supported = SCHEMA_VERSION,
            "catalog migration: refusing to open a catalog newer than this build; \
             nothing was written"
        );
        return Err(MigrationError::CatalogTooNew {
            found: stamped,
            supported: SCHEMA_VERSION,
        });
    }

    let from = if stamped == 0 {
        if is_empty(conn)? {
            tracing::info!(
                version = SCHEMA_VERSION,
                "catalog migration: empty catalog, creating the current schema"
            );
            create_fresh(conn)?;
            return Ok(Outcome {
                from: 0,
                to: SCHEMA_VERSION,
                created: true,
                applied: Vec::new(),
            });
        }
        // Tables but no stamp: a catalog written before the stamp existed, or one whose header
        // was lost. v1 is the oldest shape that ever shipped, so adopt it and walk up; the
        // 1 -> 2 step's conditions tolerate either v1 dialect.
        tracing::warn!(
            baseline = BASELINE_VERSION,
            "catalog migration: catalog has tables but no user_version stamp; \
             adopting the baseline version and migrating forward"
        );
        BASELINE_VERSION
    } else {
        stamped
    };

    if from == SCHEMA_VERSION {
        tracing::debug!(
            version = SCHEMA_VERSION,
            "catalog migration: already at the current schema version, no steps to apply"
        );
        return Ok(Outcome {
            from,
            to: SCHEMA_VERSION,
            created: false,
            applied: Vec::new(),
        });
    }

    tracing::info!(
        from,
        to = SCHEMA_VERSION,
        "catalog migration: upgrading the catalog schema"
    );

    let mut cursor = from;
    let mut applied = Vec::new();
    while cursor < SCHEMA_VERSION {
        let step = STEPS
            .iter()
            .find(|s| s.from == cursor)
            .ok_or(MigrationError::MissingStep {
                from: cursor,
                to: SCHEMA_VERSION,
            })?;
        apply_step(conn, step)?;
        applied.push(Applied {
            from: step.from,
            to: step.to,
            name: step.name,
        });
        cursor = step.to;
    }

    tracing::info!(
        from,
        to = cursor,
        steps = applied.len(),
        "catalog migration: complete"
    );

    Ok(Outcome {
        from,
        to: cursor,
        created: false,
        applied,
    })
}

/// Apply one step atomically: every statement plus the stamp bump commit together, so a
/// failure leaves the catalog at `step.from` and the next open retries the same step.
fn apply_step(conn: &Connection, step: &Step) -> Result<(), MigrationError> {
    tracing::info!(
        from = step.from,
        to = step.to,
        name = step.name,
        statements = step.ddl.len(),
        "catalog migration: applying step"
    );

    let run = |tx: &rusqlite::Transaction<'_>| -> Result<(), rusqlite::Error> {
        for (idx, ddl) in step.ddl.iter().enumerate() {
            match ddl {
                Ddl::Always(sql) => {
                    tracing::trace!(step = step.name, idx, "catalog migration: exec");
                    tx.execute_batch(sql)?;
                }
                Ddl::IfColumnPresent { table, column, sql } => {
                    let present = has_column(tx, table, column)?;
                    tracing::trace!(
                        step = step.name,
                        idx,
                        table,
                        column,
                        present,
                        "catalog migration: guarded exec (if column present)"
                    );
                    if present {
                        tx.execute_batch(sql)?;
                    }
                }
                Ddl::IfColumnMissing { table, column, sql } => {
                    let present = has_column(tx, table, column)?;
                    tracing::trace!(
                        step = step.name,
                        idx,
                        table,
                        column,
                        present,
                        "catalog migration: guarded exec (if column missing)"
                    );
                    if !present {
                        tx.execute_batch(sql)?;
                    }
                }
            }
        }
        tx.execute_batch(&format!("PRAGMA user_version = {};", step.to))
    };

    let tx = conn.unchecked_transaction()?;
    match run(&tx) {
        Ok(()) => {
            tx.commit()?;
            tracing::info!(
                from = step.from,
                to = step.to,
                name = step.name,
                "catalog migration: step committed"
            );
            Ok(())
        }
        Err(source) => {
            // Explicit rollback so the failure is logged next to the reason; the guard would
            // roll back on drop regardless.
            let rollback = tx.rollback();
            tracing::error!(
                from = step.from,
                to = step.to,
                name = step.name,
                error = %source,
                rollback_ok = rollback.is_ok(),
                "catalog migration: step failed and was rolled back; \
                 the catalog is unchanged at its previous version"
            );
            Err(MigrationError::Step {
                from: step.from,
                to: step.to,
                name: step.name,
                source,
            })
        }
    }
}

/// Create the current schema on an empty catalog and stamp it.
///
/// The DDL runs outside a transaction because it opens with `PRAGMA journal_mode = WAL`, which
/// SQLite refuses to change inside one. That is safe here and only here: the catalog is empty,
/// so a partial failure leaves nothing worth preserving and the next open starts over.
fn create_fresh(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(DDL)?;
    conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
}

fn read_user_version(conn: &Connection) -> Result<u32, rusqlite::Error> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
}

/// True when the catalog holds no user tables — the fresh-install signal, distinct from a
/// populated catalog whose stamp was lost.
fn is_empty(conn: &Connection) -> Result<bool, rusqlite::Error> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    Ok(count == 0)
}

/// Whether `table` has `column`. `table` is always a step-local `&'static str`, never caller
/// input, so the interpolation cannot carry anything but our own identifiers.
fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, rusqlite::Error> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
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
    //!   - `an_unstamped_catalog_is_adopted_at_the_baseline_version`
    //!
    //! Acceptance (slice `S-D23` "done when")
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

    // ── Acceptance (slice S-D23) ────────────────────────────────────────────

    /// Slice `S-D23` "done when": a library created at each historical `user_version` opens at
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

            // The write path added at v4 works against the migrated column.
            db.update_asset_hidden("uuid-hidden", true).unwrap();

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
            conn.execute_batch(
                "ALTER TABLE assets ADD COLUMN future_flag INTEGER NOT NULL DEFAULT 7;",
            )
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
}
