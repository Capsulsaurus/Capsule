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
//! This module is the SQLite adapter of the ML layer's [`VectorIndex`] port: the port, its value
//! types and the [`EmbeddingProvenance`] gate are declared in `ml::index`, which names no storage
//! engine. The inventory that decides which models are known, and at which version, reaches the
//! index only through that gate — implemented by the ML registry, and by a test double here. The
//! identity vocabulary both sides speak lives in the
//! [`domain::model_identity`](capsule_core::domain::model_identity) leaf.
//!
//! [AI/ML — Database Indexing]: https://docs/design/ai/#database-indexing-and-view-generation
//! [AI/ML — Embedding Provenance]: https://docs/design/ai/#embedding-provenance

use std::os::raw::{c_char, c_int};
use std::sync::Once;

use capsule_core::domain::model_identity::{
    DistanceMetric, EmbeddingDim, ModelId, ModelVersion, RegistryError, TaskKind,
};
pub use capsule_core::ml::{
    EmbeddingInsert, EmbeddingProvenance, EmbeddingRecord, KnnHit, VectorIndex, VectorIndexError,
    VectorTableSpec,
};
use rusqlite::{Connection, params};

use crate::db::driver::DatabaseDriver;

/// Type-erase a SQLite failure into the index port's [`VectorIndexError::Store`].
fn store(e: rusqlite::Error) -> VectorIndexError {
    VectorIndexError::Store(Box::new(e))
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
        self.insert_checked(e, dim, metric).map_err(store)
    }

    /// The SQL half of [`insert_embedding`](Self::insert_embedding), once the provenance gate and
    /// the dimension check have admitted `e` at `(dim, metric)`.
    fn insert_checked(
        &self,
        e: EmbeddingInsert<'_>,
        dim: EmbeddingDim,
        metric: DistanceMetric,
    ) -> rusqlite::Result<()> {
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
        self.knn_checked(task, query, k, platform, &canonical_version, dim, metric)
            .map_err(store)
    }

    /// The SQL half of [`knn`](Self::knn), once the registry has named the canonical version and
    /// the query's dimension has been checked.
    #[allow(clippy::too_many_arguments)]
    fn knn_checked(
        &self,
        task: TaskKind,
        query: &[f32],
        k: usize,
        platform: &str,
        canonical_version: &ModelVersion,
        dim: EmbeddingDim,
        metric: DistanceMetric,
    ) -> rusqlite::Result<Vec<KnnHit>> {
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

/// The catalog is the production [`VectorIndex`]: each port method is the inherent method of the
/// same name, with a SQLite failure type-erased into [`VectorIndexError::Store`].
impl VectorIndex for DatabaseDriver {
    fn insert_embedding<P: EmbeddingProvenance>(
        &self,
        registry: &P,
        e: EmbeddingInsert<'_>,
    ) -> Result<(), VectorIndexError> {
        DatabaseDriver::insert_embedding(self, registry, e)
    }

    fn knn<P: EmbeddingProvenance>(
        &self,
        registry: &P,
        task: TaskKind,
        query: &[f32],
        k: usize,
        platform: &str,
    ) -> Result<Vec<KnnHit>, VectorIndexError> {
        DatabaseDriver::knn(self, registry, task, query, k, platform)
    }

    fn stale_embedding_assets<P: EmbeddingProvenance>(
        &self,
        registry: &P,
        task: TaskKind,
        platform: &str,
    ) -> Result<Vec<String>, VectorIndexError> {
        DatabaseDriver::stale_embedding_assets(self, registry, task, platform).map_err(store)
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
mod tests;
