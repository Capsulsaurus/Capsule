//! The album key escrows beside the master-key escrow (#741): one sealed blob per account and
//! album, and nothing older.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The escrow table's shape keyed by `(user_id, album_id)` rather than `user_id` alone:
        // albums are created and rotated independently, so each album's keys are replaced by an
        // upsert of their own row and never by rewriting another album's. No history table, and
        // no foreign key to `albums`: the blob is the client's, sealed under its master key, and
        // an album the server has not been told about yet must still be escrowable first.
        manager
            .create_table(
                Table::create()
                    .table(AlbumKeyEscrows::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(AlbumKeyEscrows::UserId).text().not_null())
                    .col(ColumnDef::new(AlbumKeyEscrows::AlbumId).text().not_null())
                    .col(ColumnDef::new(AlbumKeyEscrows::Blob).binary().not_null())
                    .col(
                        ColumnDef::new(AlbumKeyEscrows::StoredAt)
                            .big_integer()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(AlbumKeyEscrows::UserId)
                            .col(AlbumKeyEscrows::AlbumId),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AlbumKeyEscrows::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum AlbumKeyEscrows {
    Table,
    UserId,
    AlbumId,
    Blob,
    StoredAt,
}
