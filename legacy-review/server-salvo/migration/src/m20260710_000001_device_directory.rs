//! The signed device-directory store (slice `S-C9`).
//!
//! One row per user: the latest master-signed [`DeviceDirectory`] document, stored as
//! opaque canonical CBOR (`document`) with its `directory_version` projected out for the
//! anti-rollback monotonicity check (threat-model invariant 23). The server never
//! re-models the signed bytes — it stores and serves them verbatim; only
//! `directory_version` is read out, to refuse a non-advancing or regressing publish.
//!
//! `user_id` is the account id (a 21-char nanoid, matching `users.id`) the publisher
//! authenticated as, not the crypto `DirectoryCore.user_id`; the two identifier spaces are
//! kept separate and the account id is the row key.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(DeviceDirectory::Table)
                    .if_not_exists()
                    // Account id (nanoid) the directory belongs to — one directory per user.
                    .col(
                        ColumnDef::new(DeviceDirectory::UserId)
                            .char_len(21)
                            .not_null()
                            .primary_key(),
                    )
                    // The strictly-monotonic version projected from the signed document
                    // (invariant 23's high-water mark).
                    .col(
                        ColumnDef::new(DeviceDirectory::DirectoryVersion)
                            .big_integer()
                            .not_null(),
                    )
                    // The signed DeviceDirectory as opaque canonical CBOR, stored verbatim.
                    .col(
                        ColumnDef::new(DeviceDirectory::Document)
                            .binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DeviceDirectory::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(DeviceDirectory::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum DeviceDirectory {
    Table,
    UserId,
    DirectoryVersion,
    Document,
    UpdatedAt,
}
