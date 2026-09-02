pub(crate) mod driver;
pub(crate) mod migrate;
pub(crate) mod rows;
pub(crate) mod schema;
pub(crate) mod vector;

pub use driver::DatabaseDriver;
pub use migrate::{Applied, MigrationError, Outcome as MigrationOutcome};
pub use rows::{
    AlbumRow, AssetRow, AssetStackRow, AssetTagRow, CachedRepresentationRow, StackMemberRow,
};
pub use vector::{
    EmbeddingInsert, EmbeddingProvenance, EmbeddingRecord, KnnHit, VectorIndexError,
    VectorTableSpec,
};
