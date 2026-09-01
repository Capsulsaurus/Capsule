//! The master-key escrow store (slice `S-C12`).
//!
//! One row per user: the passphrase-wrapped account master key as an opaque blob
//! (`blob`), stored and served verbatim. The server never interprets the bytes — the
//! wrap format lives entirely in `capsule_core::backup` and the blob is offline-decryptable
//! only with the ≥128-bit recovery secret (that entropy floor is enforced client-side).
//!
//! **Single active escrow.** The primary key is `user_id`, so a store-or-replace is a
//! single guarded upsert that overwrites the row in place: the prior ciphertext is gone
//! the instant the new one lands, in the same statement (the guided re-wrap contract in
//! the backup doc — the lost secret must unwrap nothing).
//!
//! `user_id` is the account id (a 21-char nanoid, matching `users.id`) the escrow owner
//! authenticated as.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(BackupEscrow::Table)
                    .if_not_exists()
                    // Account id (nanoid) the escrow belongs to — one active escrow per user.
                    .col(
                        ColumnDef::new(BackupEscrow::UserId)
                            .char_len(21)
                            .not_null()
                            .primary_key(),
                    )
                    // The passphrase-wrapped master key as an opaque blob, stored verbatim.
                    .col(ColumnDef::new(BackupEscrow::Blob).binary().not_null())
                    .col(
                        ColumnDef::new(BackupEscrow::UpdatedAt)
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
            .drop_table(Table::drop().table(BackupEscrow::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum BackupEscrow {
    Table,
    UserId,
    Blob,
    UpdatedAt,
}
