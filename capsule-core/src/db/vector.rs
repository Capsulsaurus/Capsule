//! The local vector index — SQLite + `sqlite-vec` (SSoT: [AI/ML — Database Indexing] +
//! [AI/ML — Embedding Provenance]).
//!
//! Each embedding-producing task gets a `vec0` virtual table sized from the
//! [`VectorTableSpec`] its writer declares, partitioned by `(platform, model_version)`. Querying the
//! **current** canonical `(platform, model_version)` partition means stale embeddings (an older
//! `model_version`, left over after a model swap, or admitted at a superseded version) are
//! excluded structurally — never compared across vector spaces — until the regeneration task
//! replaces them per-asset. The companion [`embeddings`](crate::db::schema) table carries the
//! provenance tuple and the `vec0` rowid so the index can find / replace / delete an asset's
//! embedding without scanning the vector store.
//!
//! The index is **derived state**: if lost it is rebuilt by re-running inference over the
//! originals (recovery-first), so it is never restored from a backup.
//!
//! This module knows **nothing about the ML layer**. The inventory that decides which models are
//! known, and at which version, reaches it through the [`EmbeddingProvenance`] seam — implemented
//! by the ML registry, and by a test double here. That inversion is what turned the former
//! `db -> ml -> db` cycle into the DAG `domain <- db <- ml`; the identity vocabulary both sides
//! speak lives in the [`domain::model_identity`](crate::domain::model_identity) leaf.
//!
//! [AI/ML — Database Indexing]: https://docs/design/ai/#database-indexing-and-view-generation
//! [AI/ML — Embedding Provenance]: https://docs/design/ai/#embedding-provenance

use std::os::raw::{c_char, c_int};
use std::sync::Once;

use rusqlite::{Connection, params};
use thiserror::Error;

use crate::db::driver::DatabaseDriver;
use crate::domain::model_identity::{
    DistanceMetric, EmbeddingDim, ModelId, ModelVersion, RegistryError, TaskKind,
};

/// A provenanced embedding to insert into the index: the subject asset, the producing model
/// `(task, model_id, model_version)`, the `platform` partition discriminator, and the vector.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddingInsert<'a> {
    /// The asset the embedding was produced from.
    pub asset_id: &'a str,
    /// The task that produced it.
    pub task: TaskKind,
    /// The producing model id.
    pub model_id: &'a ModelId,
    /// The producing model version.
    pub model_version: &'a ModelVersion,
    /// The platform partition (incomparable across platforms; see the E2EE fallback in `ai.md`).
    pub platform: &'a str,
    /// The embedding vector (length must equal the task's registry-declared dimension).
    pub vector: &'a [f32],
}

/// A single nearest-neighbour hit.
#[derive(Debug, Clone, PartialEq)]
pub struct KnnHit {
    /// The matched asset id.
    pub asset_id: String,
    /// Distance under the task's metric (smaller = nearer; cosine over normalized vectors).
    pub distance: f64,
}

/// A stored embedding's provenance (from the companion table; the vector itself stays in `vec0`).
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingRecord {
    /// Asset the embedding was produced from.
    pub asset_id: String,
    /// Task that produced it.
    pub task: TaskKind,
    /// Platform partition discriminator.
    pub platform: String,
    /// Producing model.
    pub model_id: ModelId,
    /// Producing model version.
    pub model_version: ModelVersion,
}

/// Failures from the vector index.
#[derive(Debug, Error)]
pub enum VectorIndexError {
    /// The embedding-provenance invariant rejected the insert (an unknown model or a
    /// non-embedding task).
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// The vector's length does not match the task's registry-declared dimension.
    #[error("embedding dimension mismatch for {task:?}: expected {expected}, got {got}")]
    DimMismatch {
        /// The task.
        task: TaskKind,
        /// Registry-declared dimension.
        expected: usize,
        /// The supplied vector length.
        got: usize,
    },
    /// A SQLite error.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// The shape of one task's `vec0` partition table: which task it serves, and the `(dim, metric)`
/// its vectors are sized and ranked by.
///
/// A vector table cannot be created before something declares that shape, and the shape is a
/// property of the *model* that writes into it — so the index takes it from the writer rather than
/// guessing at open time. That is why [`DatabaseDriver::open`] creates no vector tables: a caller
/// with an inventory in hand can materialize them all up front with
/// [`create_vector_tables`](DatabaseDriver::create_vector_tables), and otherwise each table is
/// created on first use by the insert/query that knows its shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VectorTableSpec {
    /// The task whose partition table this describes.
    pub task: TaskKind,
    /// Vector dimensionality (the `vec0` `float[N]` column width).
    pub dim: EmbeddingDim,
    /// The metric the partition ranks by.
    pub metric: DistanceMetric,
}

/// The embedding-provenance gate the vector index calls before it stores or ranks anything —
/// the seam through which the model inventory reaches `db` **without `db` depending on `ml`**.
///
/// Implemented by the ML layer's `Registry`, which owns the canonical rows and the version-bump
/// swap primitive. The index only ever asks the three questions below, so the rest of the
/// inventory (display names, fallbacks, swap history) stays out of this layer entirely.
pub trait EmbeddingProvenance {
    /// The gate on every insert: accept any version of a **known** model for an embedding task,
    /// returning the `(dim, metric)` its vectors must match. An **unknown** model or a
    /// non-embedding task is refused; a *superseded-but-known* version is **not** refused — it is
    /// admitted and lands as a stale-flagged row.
    fn check_insert(
        &self,
        task: TaskKind,
        model_id: &ModelId,
        version: &ModelVersion,
    ) -> Result<(EmbeddingDim, DistanceMetric), RegistryError>;

    /// The `(dim, metric)` of `task`'s vector space, or `None` if `task` stores no embeddings.
    fn embedding_spec(&self, task: TaskKind) -> Option<(EmbeddingDim, DistanceMetric)>;

    /// The **current** canonical `model_version` for `task` — the partition queries are answered
    /// from — or `None` if the task has no canonical model.
    fn canonical_version(&self, task: TaskKind) -> Option<ModelVersion>;

    /// The partition table shape for `task`, if it stores embeddings.
    fn vector_table(&self, task: TaskKind) -> Option<VectorTableSpec> {
        let (dim, metric) = self.embedding_spec(task)?;
        Some(VectorTableSpec { task, dim, metric })
    }
}

static VEC_EXTENSION: Once = Once::new();

/// Register `sqlite-vec`'s C entrypoint as a SQLite auto-extension so every connection opened
/// afterwards in this process gains the `vec0` virtual table. Idempotent (runs once).
pub(in crate::db) fn ensure_vec_extension() {
    VEC_EXTENSION.call_once(|| {
        // SAFETY: `sqlite3_vec_init` is sqlite-vec's documented extension entrypoint; registering
        // it as an auto-extension is the supported integration path with rusqlite's bundled
        // SQLite. The transmute adapts its signature to the `xEntryPoint` type sqlite expects.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut c_char,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> c_int,
            >(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
    });
}

/// Emit the `CREATE VIRTUAL TABLE IF NOT EXISTS` for one partition table (idempotent).
fn create_vector_table(conn: &Connection, spec: VectorTableSpec) -> rusqlite::Result<()> {
    let ddl = format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS {table} USING vec0(\
           embedding float[{dim}] distance_metric={metric}, \
           platform text partition key, \
           model_version text partition key, \
           +asset_id text, \
           +model_id text)",
        table = vec_table(spec.task),
        dim = spec.dim.get(),
        metric = metric_sql(spec.metric),
    );
    tracing::trace!(task = ?spec.task, dim = %spec.dim, "vector index: creating vec0 partition table");
    conn.execute_batch(&ddl)
}

fn metric_sql(metric: DistanceMetric) -> &'static str {
    match metric {
        DistanceMetric::Cosine => "cosine",
        DistanceMetric::L2 => "L2",
    }
}

/// The `vec0` table name for a task (stable, underscore-cased).
fn vec_table(task: TaskKind) -> String {
    let ident = match task {
        TaskKind::SemanticSearch => "semantic_search",
        TaskKind::ObjectDetection => "object_detection",
        TaskKind::FaceDetection => "face_detection",
        TaskKind::FaceRecognition => "face_recognition",
    };
    format!("embeddings_vec_{ident}")
}

/// Pack an `f32` slice as little-endian bytes — the `vec0` `float[N]` BLOB wire form.
fn f32_to_le_blob(v: &[f32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(v.len() * 4);
    for x in v {
        b.extend_from_slice(&x.to_le_bytes());
    }
    b
}

impl DatabaseDriver {
    /// Create the `vec0` partition tables for `specs` up front (idempotent).
    ///
    /// Optional: every insert / query creates the table it needs on first use. Call this when a
    /// caller already holds the whole inventory (`ml::Registry::vector_tables()`) and wants the
    /// schema materialized eagerly — e.g. so an external tool inspecting the file sees it.
    pub fn create_vector_tables(&self, specs: &[VectorTableSpec]) -> rusqlite::Result<()> {
        for spec in specs {
            self.ensure_vector_table(*spec)?;
        }
        Ok(())
    }

    /// Create `spec`'s partition table if this driver has not already done so.
    ///
    /// Memoized per task: the DDL is idempotent, but `insert_embedding` is a per-asset hot path
    /// and SQLite would re-parse the statement on every call.
    fn ensure_vector_table(&self, spec: VectorTableSpec) -> rusqlite::Result<()> {
        if self.vector_tables.borrow().contains(&spec.task) {
            return Ok(());
        }
        create_vector_table(&self.conn, spec)?;
        self.vector_tables.borrow_mut().insert(spec.task);
        Ok(())
    }

    /// Insert (or replace) the embedding described by `e`.
    ///
    /// Enforces the embedding-provenance invariant via [`EmbeddingProvenance::check_insert`]: an **unknown**
    /// model or a non-embedding task is refused, while any version of a **known** model is
    /// admitted — a *superseded-but-known* version lands as a stale-flagged row (excluded from
    /// queries until regenerated). Replacing an existing entry (e.g. regeneration at a new
    /// version) drops the prior vector first, so an asset never carries two embeddings for the
    /// same `(task, platform)`.
    pub fn insert_embedding(
        &self,
        registry: &impl EmbeddingProvenance,
        e: EmbeddingInsert<'_>,
    ) -> Result<(), VectorIndexError> {
        let (dim, metric) = registry.check_insert(e.task, e.model_id, e.model_version)?;
        if e.vector.len() != dim.get() {
            return Err(VectorIndexError::DimMismatch {
                task: e.task,
                expected: dim.get(),
                got: e.vector.len(),
            });
        }
        self.ensure_vector_table(VectorTableSpec {
            task: e.task,
            dim,
            metric,
        })?;
        let table = vec_table(e.task);

        // Replace semantics: drop any existing vector for this (asset, task, platform).
        if let Some(prior_rowid) = self.embedding_rowid(e.asset_id, e.task, e.platform)? {
            self.conn.execute(
                &format!("DELETE FROM {table} WHERE rowid = ?1"),
                params![prior_rowid],
            )?;
            self.conn.execute(
                "DELETE FROM embeddings WHERE asset_id = ?1 AND task = ?2 AND platform = ?3",
                params![e.asset_id, task_str(e.task), e.platform],
            )?;
        }

        let rowid: i64 = self.conn.query_row(
            &format!("SELECT COALESCE(MAX(rowid), 0) + 1 FROM {table}"),
            [],
            |r| r.get(0),
        )?;
        self.conn.execute(
            &format!(
                "INSERT INTO {table}(rowid, embedding, platform, model_version, asset_id, model_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
            ),
            params![
                rowid,
                f32_to_le_blob(e.vector),
                e.platform,
                e.model_version.as_str(),
                e.asset_id,
                e.model_id.as_str(),
            ],
        )?;
        self.conn.execute(
            "INSERT INTO embeddings(asset_id, task, platform, model_id, model_version, vec_rowid, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                e.asset_id,
                task_str(e.task),
                e.platform,
                e.model_id.as_str(),
                e.model_version.as_str(),
                rowid,
                now_secs(),
            ],
        )?;
        Ok(())
    }

    /// K nearest neighbours for `query` in `task`'s **current canonical** `(platform,
    /// model_version)` partition. Stale embeddings (older versions) are excluded structurally.
    /// Returns hits nearest-first (inner-product ranking over normalized vectors).
    pub fn knn(
        &self,
        registry: &impl EmbeddingProvenance,
        task: TaskKind,
        query: &[f32],
        k: usize,
        platform: &str,
    ) -> Result<Vec<KnnHit>, VectorIndexError> {
        let canonical_version =
            registry
                .canonical_version(task)
                .ok_or_else(|| RegistryError::NonCanonical {
                    task,
                    model_id: ModelId::from(""),
                })?;
        let (dim, metric) = registry
            .embedding_spec(task)
            .ok_or(RegistryError::NotAnEmbeddingTask { task })?;
        if query.len() != dim.get() {
            return Err(VectorIndexError::DimMismatch {
                task,
                expected: dim.get(),
                got: query.len(),
            });
        }
        self.ensure_vector_table(VectorTableSpec { task, dim, metric })?;
        let table = vec_table(task);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT asset_id, distance FROM {table} \
             WHERE embedding MATCH ?1 AND k = ?2 AND platform = ?3 AND model_version = ?4 \
             ORDER BY distance"
        ))?;
        let hits = stmt
            .query_map(
                params![
                    f32_to_le_blob(query),
                    k as i64,
                    platform,
                    canonical_version.as_str(),
                ],
                |r| {
                    Ok(KnnHit {
                        asset_id: r.get(0)?,
                        distance: r.get(1)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(hits)
    }

    /// The provenance records of every embedding stored for `asset_id` (any task / platform).
    pub fn embeddings_for(&self, asset_id: &str) -> Result<Vec<EmbeddingRecord>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT asset_id, task, platform, model_id, model_version \
             FROM embeddings WHERE asset_id = ?1 ORDER BY task, platform",
        )?;
        let rows = stmt
            .query_map(params![asset_id], |r| {
                let task: String = r.get(1)?;
                Ok(EmbeddingRecord {
                    asset_id: r.get(0)?,
                    task: task_from_str(&task).ok_or_else(|| {
                        rusqlite::Error::InvalidColumnType(1, task, rusqlite::types::Type::Text)
                    })?,
                    platform: r.get(2)?,
                    model_id: ModelId::from(r.get::<_, String>(3)?),
                    model_version: ModelVersion::from(r.get::<_, String>(4)?),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Delete every embedding stored for `asset_id` (its `vec0` rows and companion rows). Used
    /// when an asset is purged.
    pub fn delete_embeddings_for(&self, asset_id: &str) -> Result<(), rusqlite::Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT task, vec_rowid FROM embeddings WHERE asset_id = ?1")?;
        let rows: Vec<(String, i64)> = stmt
            .query_map(params![asset_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        for (task, rowid) in rows {
            if let Some(task) = task_from_str(&task) {
                self.conn.execute(
                    &format!("DELETE FROM {} WHERE rowid = ?1", vec_table(task)),
                    params![rowid],
                )?;
            }
        }
        self.conn.execute(
            "DELETE FROM embeddings WHERE asset_id = ?1",
            params![asset_id],
        )?;
        Ok(())
    }

    /// Assets in `task`'s `platform` partition whose stored embedding is **stale** — its
    /// `model_version` trails the registry's canonical version. These are the regeneration
    /// work-list after a model swap.
    pub fn stale_embedding_assets(
        &self,
        registry: &impl EmbeddingProvenance,
        task: TaskKind,
        platform: &str,
    ) -> Result<Vec<String>, rusqlite::Error> {
        let Some(canonical) = registry.canonical_version(task) else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(
            "SELECT asset_id FROM embeddings \
             WHERE task = ?1 AND platform = ?2 AND model_version != ?3 ORDER BY asset_id",
        )?;
        let rows = stmt
            .query_map(params![task_str(task), platform, canonical.as_str()], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Total embeddings stored for `task` (all platforms / versions).
    pub fn embedding_count(&self, task: TaskKind) -> Result<usize, rusqlite::Error> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM embeddings WHERE task = ?1",
            params![task_str(task)],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    fn embedding_rowid(
        &self,
        asset_id: &str,
        task: TaskKind,
        platform: &str,
    ) -> Result<Option<i64>, rusqlite::Error> {
        self.conn
            .query_row(
                "SELECT vec_rowid FROM embeddings WHERE asset_id = ?1 AND task = ?2 AND platform = ?3",
                params![asset_id, task_str(task), platform],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
    }
}

fn task_str(task: TaskKind) -> &'static str {
    match task {
        TaskKind::SemanticSearch => "semantic-search",
        TaskKind::ObjectDetection => "object-detection",
        TaskKind::FaceDetection => "face-detection",
        TaskKind::FaceRecognition => "face-recognition",
    }
}

fn task_from_str(s: &str) -> Option<TaskKind> {
    Some(match s {
        "semantic-search" => TaskKind::SemanticSearch,
        "object-detection" => TaskKind::ObjectDetection,
        "face-detection" => TaskKind::FaceDetection,
        "face-recognition" => TaskKind::FaceRecognition,
        _ => return None,
    })
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLATFORM: &str = "cpu-reference";

    /// A double for the model inventory.
    ///
    /// `db` must not depend on `ml`, so its tests reach the index through the
    /// [`EmbeddingProvenance`] seam with a local fake rather than the real `ml::Registry` — which
    /// is exercised against this same index end-to-end in `ml::regen` and `ml::orchestrator`. The
    /// fake keeps the real inventory's *shape* (two embedding slots, two detection slots) at a
    /// small dimension.
    #[derive(Debug, Clone)]
    struct FakeInventory {
        rows: Vec<FakeRow>,
    }

    /// One fake inventory row; a `None` spec is a detection task (stores no vectors).
    #[derive(Debug, Clone)]
    struct FakeRow {
        task: TaskKind,
        model_id: ModelId,
        canonical_version: ModelVersion,
        spec: Option<(EmbeddingDim, DistanceMetric)>,
    }

    const FAKE_DIM: EmbeddingDim = EmbeddingDim(8);

    impl FakeInventory {
        fn canonical() -> Self {
            let embedding = Some((FAKE_DIM, DistanceMetric::Cosine));
            let row = |task, model_id: &str, spec| FakeRow {
                task,
                model_id: ModelId::from(model_id),
                canonical_version: ModelVersion::from("1"),
                spec,
            };
            Self {
                rows: vec![
                    row(TaskKind::SemanticSearch, "mobileclip-b", embedding),
                    row(TaskKind::FaceRecognition, "adaface", embedding),
                    row(TaskKind::ObjectDetection, "yolov10", None),
                    row(TaskKind::FaceDetection, "scrfd", None),
                ],
            }
        }

        fn row(&self, task: TaskKind) -> Option<&FakeRow> {
            self.rows.iter().find(|r| r.task == task)
        }

        fn set_canonical_version(&mut self, task: TaskKind, version: ModelVersion) {
            if let Some(row) = self.rows.iter_mut().find(|r| r.task == task) {
                row.canonical_version = version;
            }
        }

        fn dim_for(&self, task: TaskKind) -> Option<EmbeddingDim> {
            self.embedding_spec(task).map(|(dim, _)| dim)
        }
    }

    impl EmbeddingProvenance for FakeInventory {
        fn check_insert(
            &self,
            task: TaskKind,
            model_id: &ModelId,
            _version: &ModelVersion,
        ) -> Result<(EmbeddingDim, DistanceMetric), RegistryError> {
            let unknown = || RegistryError::NonCanonical {
                task,
                model_id: model_id.clone(),
            };
            let row = self.row(task).ok_or_else(unknown)?;
            if &row.model_id != model_id {
                return Err(unknown());
            }
            row.spec.ok_or(RegistryError::NotAnEmbeddingTask { task })
        }

        fn embedding_spec(&self, task: TaskKind) -> Option<(EmbeddingDim, DistanceMetric)> {
            self.row(task)?.spec
        }

        fn canonical_version(&self, task: TaskKind) -> Option<ModelVersion> {
            self.row(task).map(|r| r.canonical_version.clone())
        }
    }

    fn unit(i: usize, dim: usize) -> Vec<f32> {
        // A one-hot unit vector (already L2-normalized) in `dim` dimensions.
        let mut v = vec![0.0f32; dim];
        v[i % dim] = 1.0;
        v
    }

    fn db() -> DatabaseDriver {
        DatabaseDriver::open_in_memory().unwrap()
    }

    fn sem() -> TaskKind {
        TaskKind::SemanticSearch
    }

    /// Whether `table` exists in the connected database (`vec0` tables register as `table`s).
    fn table_exists(db: &DatabaseDriver, table: &str) -> bool {
        db.conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                params![table],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0
    }

    /// Insert a semantic-search embedding for the common (canonical model, given version) case.
    fn ins(
        db: &DatabaseDriver,
        reg: &FakeInventory,
        asset: &str,
        version: &str,
        platform: &str,
        vector: &[f32],
    ) -> Result<(), VectorIndexError> {
        db.insert_embedding(
            reg,
            EmbeddingInsert {
                asset_id: asset,
                task: sem(),
                model_id: &ModelId::from("mobileclip-b"),
                model_version: &ModelVersion::from(version),
                platform,
                vector,
            },
        )
    }

    #[test]
    fn insert_then_knn_round_trips_the_nearest_asset() {
        let db = db();
        let reg = FakeInventory::canonical();
        let dim = reg.dim_for(sem()).unwrap().get();
        for (asset, i) in [("a", 0usize), ("b", 1), ("c", 2)] {
            ins(&db, &reg, asset, "1", PLATFORM, &unit(i, dim)).unwrap();
        }
        // Query the exact direction of "b" → "b" is the nearest hit.
        let hits = db.knn(&reg, sem(), &unit(1, dim), 2, PLATFORM).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].asset_id, "b");
        assert!(hits[0].distance <= hits[1].distance);
    }

    #[test]
    fn insert_refuses_unknown_model() {
        let db = db();
        let reg = FakeInventory::canonical();
        let dim = reg.dim_for(sem()).unwrap().get();
        // An unknown model id is rejected at the insert API (never silently mixed in).
        let err = db
            .insert_embedding(
                &reg,
                EmbeddingInsert {
                    asset_id: "a",
                    task: sem(),
                    model_id: &ModelId::from("siglip-tiny"),
                    model_version: &ModelVersion::from("1"),
                    platform: PLATFORM,
                    vector: &unit(0, dim),
                },
            )
            .unwrap_err();
        assert!(matches!(
            err,
            VectorIndexError::Registry(RegistryError::NonCanonical { .. })
        ));
        assert_eq!(db.embedding_count(sem()).unwrap(), 0);
    }

    #[test]
    fn superseded_but_known_version_is_admitted_as_stale() {
        let db = db();
        let reg = FakeInventory::canonical();
        let dim = reg.dim_for(sem()).unwrap().get();
        // Canonical is version "1"; insert a *superseded-but-known* version "0" directly. The
        // contract admits it (the refusal targets unknown models, not known-but-old ones).
        ins(&db, &reg, "a", "0", PLATFORM, &unit(0, dim)).unwrap();
        // It is stored, and its provenance version is preserved.
        assert_eq!(db.embedding_count(sem()).unwrap(), 1);
        assert_eq!(
            db.embeddings_for("a").unwrap()[0].model_version,
            ModelVersion::from("0")
        );
        // It appears on the regeneration work-list …
        assert_eq!(
            db.stale_embedding_assets(&reg, sem(), PLATFORM).unwrap(),
            vec!["a".to_string()]
        );
        // … and is excluded from queries at the canonical version (different partition).
        assert!(
            db.knn(&reg, sem(), &unit(0, dim), 5, PLATFORM)
                .unwrap()
                .is_empty(),
            "a superseded embedding must be excluded from queries until regenerated"
        );
    }

    #[test]
    fn insert_refuses_wrong_dimension() {
        let db = db();
        let reg = FakeInventory::canonical();
        let err = ins(&db, &reg, "a", "1", PLATFORM, &[1.0, 0.0, 0.0]).unwrap_err();
        assert!(matches!(err, VectorIndexError::DimMismatch { .. }));
    }

    #[test]
    fn provenance_tuple_is_preserved() {
        let db = db();
        let reg = FakeInventory::canonical();
        let dim = reg.dim_for(sem()).unwrap().get();
        ins(&db, &reg, "a", "1", PLATFORM, &unit(0, dim)).unwrap();
        let recs = db.embeddings_for("a").unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].task, sem());
        assert_eq!(recs[0].model_id, ModelId::from("mobileclip-b"));
        assert_eq!(recs[0].model_version, ModelVersion::from("1"));
        assert_eq!(recs[0].platform, PLATFORM);
    }

    #[test]
    fn version_bump_flags_stale_excludes_from_queries_and_regenerates_per_asset() {
        let db = db();
        let mut reg = FakeInventory::canonical();
        let dim = reg.dim_for(sem()).unwrap().get();

        ins(&db, &reg, "a", "1", PLATFORM, &unit(0, dim)).unwrap();
        assert_eq!(
            db.knn(&reg, sem(), &unit(0, dim), 5, PLATFORM)
                .unwrap()
                .len(),
            1
        );
        assert!(
            db.stale_embedding_assets(&reg, sem(), PLATFORM)
                .unwrap()
                .is_empty()
        );

        // Swap the model: bump the canonical version. The v1 embedding is now stale.
        reg.set_canonical_version(sem(), ModelVersion::from("2"));
        assert_eq!(
            db.stale_embedding_assets(&reg, sem(), PLATFORM).unwrap(),
            vec!["a".to_string()]
        );
        // A query at the new canonical version excludes the stale v1 embedding (different
        // partition) — even though it is geometrically identical to the query.
        assert!(
            db.knn(&reg, sem(), &unit(0, dim), 5, PLATFORM)
                .unwrap()
                .is_empty(),
            "stale embeddings must be excluded from queries until regenerated"
        );

        // Regenerate per-asset at the new version: the old vector is replaced, not truncated.
        ins(&db, &reg, "a", "2", PLATFORM, &unit(0, dim)).unwrap();
        assert!(
            db.stale_embedding_assets(&reg, sem(), PLATFORM)
                .unwrap()
                .is_empty()
        );
        let hits = db.knn(&reg, sem(), &unit(0, dim), 5, PLATFORM).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].asset_id, "a");
        // Still exactly one stored embedding for the asset (replace, not accumulate).
        assert_eq!(db.embedding_count(sem()).unwrap(), 1);
        assert_eq!(
            db.embeddings_for("a").unwrap()[0].model_version,
            ModelVersion::from("2")
        );
    }

    #[test]
    fn platform_partitions_are_not_merged() {
        let db = db();
        let reg = FakeInventory::canonical();
        let dim = reg.dim_for(sem()).unwrap().get();
        // Same asset+direction embedded on two platforms that did not reach bit-exact parity.
        ins(&db, &reg, "a", "1", "apple-coreml", &unit(0, dim)).unwrap();
        ins(&db, &reg, "b", "1", "android-nnapi", &unit(0, dim)).unwrap();
        // A query within one platform partition never returns the other platform's vectors.
        let hits = db
            .knn(&reg, sem(), &unit(0, dim), 5, "apple-coreml")
            .unwrap();
        assert_eq!(
            hits.iter().map(|h| h.asset_id.as_str()).collect::<Vec<_>>(),
            ["a"]
        );
        // Both platforms are retained for the asset set (regenerated locally, not merged).
        assert_eq!(db.embedding_count(sem()).unwrap(), 2);
    }

    #[test]
    fn delete_removes_vectors_and_provenance() {
        let db = db();
        let reg = FakeInventory::canonical();
        let dim = reg.dim_for(sem()).unwrap().get();
        ins(&db, &reg, "a", "1", PLATFORM, &unit(0, dim)).unwrap();
        db.delete_embeddings_for("a").unwrap();
        assert!(db.embeddings_for("a").unwrap().is_empty());
        assert!(
            db.knn(&reg, sem(), &unit(0, dim), 5, PLATFORM)
                .unwrap()
                .is_empty()
        );
        assert_eq!(db.embedding_count(sem()).unwrap(), 0);
    }

    #[test]
    fn derived_state_rebuilds_from_empty() {
        // A fresh DB starts empty and is repopulated by re-running inference (here, re-inserting),
        // never restored from a backup.
        let db = db();
        let reg = FakeInventory::canonical();
        assert_eq!(db.embedding_count(sem()).unwrap(), 0);
        let dim = reg.dim_for(sem()).unwrap().get();
        ins(&db, &reg, "a", "1", PLATFORM, &unit(0, dim)).unwrap();
        assert_eq!(db.embedding_count(sem()).unwrap(), 1);
    }

    // ── Partition tables come from their writer, not from schema init ────────────────────────

    #[test]
    fn a_partition_table_is_created_by_the_writer_that_declares_its_shape() {
        // Opening the DB cannot know a vec0 column width — that is the model's property, and this
        // layer has no model inventory. So `open` creates no partition table…
        let db = db();
        let table = vec_table(sem());
        assert!(
            !table_exists(&db, &table),
            "open must not guess a vector-table shape"
        );

        // …and the first insert creates it from the spec the provenance gate returned.
        let reg = FakeInventory::canonical();
        let dim = reg.dim_for(sem()).unwrap().get();
        ins(&db, &reg, "a", "1", PLATFORM, &unit(0, dim)).unwrap();
        assert!(table_exists(&db, &table));
        assert_eq!(dim, FAKE_DIM.get());

        // A second insert reuses it (the memo makes the DDL a no-op, and nothing is lost).
        ins(&db, &reg, "b", "1", PLATFORM, &unit(1, dim)).unwrap();
        assert_eq!(db.embedding_count(sem()).unwrap(), 2);
    }

    #[test]
    fn a_query_against_a_never_written_task_creates_its_table_and_returns_nothing() {
        // knn must not explode on a task nothing has written yet: it knows the shape too.
        let db = db();
        let reg = FakeInventory::canonical();
        let dim = reg.dim_for(TaskKind::FaceRecognition).unwrap().get();
        let hits = db
            .knn(&reg, TaskKind::FaceRecognition, &unit(0, dim), 5, PLATFORM)
            .unwrap();
        assert!(hits.is_empty());
        assert!(table_exists(&db, &vec_table(TaskKind::FaceRecognition)));
    }

    #[test]
    fn create_vector_tables_materializes_the_whole_schema_up_front() {
        // The eager path a caller holding a full inventory uses: every declared partition exists
        // before a single embedding is produced, and detection tasks get no table at all.
        let db = db();
        let reg = FakeInventory::canonical();
        let specs: Vec<VectorTableSpec> = [TaskKind::SemanticSearch, TaskKind::FaceRecognition]
            .into_iter()
            .filter_map(|t| reg.vector_table(t))
            .collect();
        assert_eq!(specs.len(), 2);
        db.create_vector_tables(&specs).unwrap();
        assert!(table_exists(&db, &vec_table(TaskKind::SemanticSearch)));
        assert!(table_exists(&db, &vec_table(TaskKind::FaceRecognition)));
        assert!(reg.vector_table(TaskKind::ObjectDetection).is_none());
        assert!(!table_exists(&db, &vec_table(TaskKind::ObjectDetection)));
        // Idempotent: calling again neither fails nor drops the stored vectors.
        let dim = reg.dim_for(sem()).unwrap().get();
        ins(&db, &reg, "a", "1", PLATFORM, &unit(0, dim)).unwrap();
        db.create_vector_tables(&specs).unwrap();
        assert_eq!(db.embedding_count(sem()).unwrap(), 1);
    }
}
