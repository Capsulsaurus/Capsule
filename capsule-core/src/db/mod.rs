pub mod driver;
pub mod migrate;
pub mod rows;
pub mod schema;
pub mod vector;

pub use driver::DatabaseDriver;
pub use migrate::{Applied, MigrationError, Outcome as MigrationOutcome};
pub use rows::{
    AlbumRow, AssetRow, AssetStackRow, AssetTagRow, CachedRepresentationRow, StackMemberRow,
};
pub use vector::{
    EmbeddingInsert, EmbeddingProvenance, EmbeddingRecord, KnnHit, VectorIndexError,
    VectorTableSpec,
};
