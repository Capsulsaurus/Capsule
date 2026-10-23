//! Guest drops: the upload links, the declarations held while bytes land, and the
//! owner's inbox.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One row per provisioned link. The caps and the running totals live on the same row,
        // because `charge` decides and reserves under one row lock — a counter kept anywhere
        // else is a counter two guests can both read as having room. A revoked link keeps its
        // row with `revoked_at` set.
        manager
            .create_table(
                Table::create()
                    .table(DropLinks::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DropLinks::OpaqueId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(DropLinks::OwnerId).text().not_null())
                    .col(ColumnDef::new(DropLinks::DropPubkey).binary().not_null())
                    .col(
                        ColumnDef::new(DropLinks::CryptoSuiteId)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DropLinks::ProtocolVersion).text().not_null())
                    .col(ColumnDef::new(DropLinks::ExpiresAt).big_integer().null())
                    .col(
                        ColumnDef::new(DropLinks::MaxTotalBytes)
                            .big_integer()
                            .null(),
                    )
                    .col(ColumnDef::new(DropLinks::MaxFileCount).big_integer().null())
                    .col(ColumnDef::new(DropLinks::MaxFileSize).big_integer().null())
                    .col(ColumnDef::new(DropLinks::SingleUse).boolean().not_null())
                    .col(
                        ColumnDef::new(DropLinks::PassphraseVerifier)
                            .binary()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(DropLinks::UsedBytes)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DropLinks::UsedFiles)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DropLinks::RevokedAt).big_integer().null())
                    .to_owned(),
            )
            .await?;

        // A guest's declaration, keyed by the upload session it belongs to, held between
        // session creation and the bytes landing. The upload-session record has nowhere to
        // carry `kem_ct` or a filename, nor should it.
        manager
            .create_table(
                Table::create()
                    .table(DropPending::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DropPending::UploadId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(DropPending::DropId).text().not_null())
                    .col(ColumnDef::new(DropPending::OpaqueId).text().not_null())
                    .col(ColumnDef::new(DropPending::OwnerId).text().not_null())
                    .col(ColumnDef::new(DropPending::KemCt).binary().not_null())
                    .col(ColumnDef::new(DropPending::ContentType).text().not_null())
                    .col(ColumnDef::new(DropPending::SuggestedFilename).text().null())
                    .col(ColumnDef::new(DropPending::Size).big_integer().not_null())
                    .to_owned(),
            )
            .await?;

        // A finished drop waiting for its owner. `adopting` is the first phase of the two-phase
        // claim: a crash between claim and settle leaves the row visibly held, neither lost nor
        // duplicated.
        manager
            .create_table(
                Table::create()
                    .table(DropInbox::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DropInbox::DropId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(DropInbox::OwnerId).text().not_null())
                    .col(ColumnDef::new(DropInbox::OpaqueId).text().not_null())
                    .col(ColumnDef::new(DropInbox::Address).text().not_null())
                    .col(ColumnDef::new(DropInbox::Size).big_integer().not_null())
                    .col(ColumnDef::new(DropInbox::ContentType).text().not_null())
                    .col(ColumnDef::new(DropInbox::KemCt).binary().not_null())
                    .col(ColumnDef::new(DropInbox::SuggestedFilename).text().null())
                    .col(
                        ColumnDef::new(DropInbox::ReceivedAt)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DropInbox::Adopting).boolean().not_null())
                    .to_owned(),
            )
            .await?;
        // "Everything waiting for this owner, oldest first" — the inbox listing's question.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_drop_inbox_owner_received")
                    .table(DropInbox::Table)
                    .col(DropInbox::OwnerId)
                    .col(DropInbox::ReceivedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(DropInbox::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(DropPending::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(DropLinks::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum DropLinks {
    Table,
    OpaqueId,
    OwnerId,
    DropPubkey,
    CryptoSuiteId,
    ProtocolVersion,
    ExpiresAt,
    MaxTotalBytes,
    MaxFileCount,
    MaxFileSize,
    SingleUse,
    PassphraseVerifier,
    UsedBytes,
    UsedFiles,
    RevokedAt,
}

#[derive(DeriveIden)]
enum DropPending {
    Table,
    UploadId,
    DropId,
    OpaqueId,
    OwnerId,
    KemCt,
    ContentType,
    SuggestedFilename,
    Size,
}

#[derive(DeriveIden)]
enum DropInbox {
    Table,
    DropId,
    OwnerId,
    OpaqueId,
    Address,
    Size,
    ContentType,
    KemCt,
    SuggestedFilename,
    ReceivedAt,
    Adopting,
}
