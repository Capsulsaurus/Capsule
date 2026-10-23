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
//! SSoT: design/versioning § Client Catalog Migration.
//!
//! # Why not drop and rebuild
//!
//! `rebuild_index` is the **repair** path ([Maintenance § Repair]), reached when the index is
//! already known-inconsistent. It is not a release ritual: spending it on every shipped column
//! would re-derive the whole library on each upgrade and re-lose whatever the sidecars do not
//! yet carry (importer-formed stack placement). The migrator is the durability
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
//! Two consequences the original plan did not anticipate, both handled below:
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

/// v2 → v3. Adds the `embeddings` provenance companion table.
///
/// The `vec0` partition tables are *not* created here: their vector dimension is declared by
/// the model registry, so they are created at runtime by their writer (see
/// [`crate::db::VectorTableSpec`]).
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

/// v3 → v4. Adds `assets.is_hidden`, the index projection of the sidecar `hidden` LWW register.
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

/// Why a catalog could not be brought to `SCHEMA_VERSION`.
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

/// One applied step, as reported by `migrate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    pub from: u32,
    pub to: u32,
    pub name: &'static str,
}

/// What `migrate` did. Recorded so the CLI, the FFI host, and a support bundle can all say
/// exactly what ran against a user's library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The version the catalog reported on open (`0` for a catalog this call created).
    pub from: u32,
    /// The version it is stamped at now — always `SCHEMA_VERSION` on success.
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
mod tests;
