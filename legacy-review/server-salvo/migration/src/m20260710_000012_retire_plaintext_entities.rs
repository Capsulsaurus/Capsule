//! Plaintext entity retirement (slice `S-G3`).
//!
//! Retires the server-side plaintext-era surface that predates end-to-end encryption. The
//! server holds no decryption key, so it must not carry plaintext user metadata: capture
//! date, dimensions, EXIF geo-location, visual placeholders, tags, filenames, faces, people,
//! or auto-generated memories. Those features re-land **client-side**, where the key lives
//! (the [AI/ML] design + client-side views); the server keeps only the key-free index the
//! [Filesystem — Server] doc specifies.
//!
//! This migration is **forward-only** and safe on populated tables — dropping is the design
//! intent (the plaintext era does not come back), so `down` is intentionally a no-op rather
//! than a faithful inverse.
//!
//! Dropped:
//!
//! - **Tables** — `faces`, `people`, `smart_tags`, `memories`, `asset_smart_tags` (the
//!   AI-tags/faces/memories server halves). Dropped child-before-parent so no in-table
//!   foreign key dangles (`asset_smart_tags`→`smart_tags`, `faces`→`people`).
//! - **`assets` columns** — `width`, `height`, `original_filename`, `latitude`, `longitude`,
//!   `lqip_hash`, `dominant_color`, `is_favorite`, `captured_at`. Postgres drops each
//!   column's dependent index automatically. What remains is the key-free row set: identity
//!   / ownership / album ref, blob content hash + coarse media kind + declared size +
//!   content type, the server-visible lifecycle flags (`uploaded`, `deleted_at`, `served`),
//!   and the server's own clocks.
//!
//! [AI/ML]: ../../../capsule-docs/src/content/docs/design/ai.md
//! [Filesystem — Server]: ../../../capsule-docs/src/content/docs/design/filesystem/server.md

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop the plaintext-era tables. Child-before-parent: `asset_smart_tags` references
        // `smart_tags`, and `faces` references `people`.
        manager
            .drop_table(Table::drop().table(AssetSmartTags::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(SmartTags::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Faces::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(People::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Memories::Table).to_owned())
            .await?;

        // Drop the plaintext columns on `assets`, down to the key-free row set. A single
        // `ALTER TABLE` drops them all; Postgres removes each column's dependent index with it.
        manager
            .alter_table(
                Table::alter()
                    .table(Assets::Table)
                    .drop_column(Assets::Width)
                    .drop_column(Assets::Height)
                    .drop_column(Assets::OriginalFilename)
                    .drop_column(Assets::Latitude)
                    .drop_column(Assets::Longitude)
                    .drop_column(Assets::LqipHash)
                    .drop_column(Assets::DominantColor)
                    .drop_column(Assets::IsFavorite)
                    .drop_column(Assets::CapturedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only: the plaintext era is retired permanently (the features re-land
        // client-side, not on the server). There is deliberately no faithful inverse.
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Assets {
    Table,
    Width,
    Height,
    OriginalFilename,
    Latitude,
    Longitude,
    LqipHash,
    DominantColor,
    IsFavorite,
    CapturedAt,
}

#[derive(DeriveIden)]
enum Faces {
    Table,
}

#[derive(DeriveIden)]
enum People {
    Table,
}

#[derive(DeriveIden)]
enum SmartTags {
    Table,
}

#[derive(DeriveIden)]
enum Memories {
    Table,
}

#[derive(DeriveIden)]
enum AssetSmartTags {
    Table,
}
