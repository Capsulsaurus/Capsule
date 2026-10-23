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
