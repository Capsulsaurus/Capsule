//! The web-upload drop store schema (slice `S-C5`).
//!
//! Two tables back the guest-drop + adoption path (SSoT: the Web Upload design doc):
//!
//! - `upload_links` — one row per provisioned upload link. `opaque_id` is the random
//!   ≥128-bit URL-path token (unique), resolved on every drop-session creation; `owner_id`
//!   is the provisioning user whose quota a drop debits (invariant 29). The per-link caps
//!   (`expires_at`, `max_total_bytes`, `max_file_count`, `max_file_size`, `single_use`) plus
//!   the running `bytes_used`/`files_used` counters enforce the cumulative caps (invariant
//!   26); `passphrase_verifier` holds the optional Argon2id abuse-gate verifier;
//!   `revoked_at` is the fail-closed revocation flag.
//! - `drop_inbox` — one row per pending drop awaiting the owner's review. It references the
//!   content-addressed drop blob by `ciphertext_hash` and never an album; adoption
//!   (invariant 32) promotes it to an `assets` row and deletes the inbox row in one
//!   transaction.

use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The provisioned upload links.
        manager
            .create_table(
                Table::create()
                    .table(UploadLinks::Table)
                    .if_not_exists()
                    // Revocation handle (owner-held; may be a UUIDv7 since it is never URL-exposed).
                    .col(string(UploadLinks::LinkId).primary_key())
                    // The random ≥128-bit opaque URL-path token (hex of 16 bytes = 32 chars).
                    .col(string_len(UploadLinks::OpaqueId, 32))
                    .col(string(UploadLinks::OwnerId))
                    // Optional destination-album hint (advisory; adoption picks the album).
                    .col(string_null(UploadLinks::AlbumHint))
                    .col(string_len(UploadLinks::ProtocolVersion, 10))
                    .col(integer(UploadLinks::CryptoSuiteId))
                    // ── Per-link caps (all optional). ──
                    .col(timestamp_with_time_zone_null(UploadLinks::ExpiresAt))
                    .col(big_integer_null(UploadLinks::MaxTotalBytes))
                    .col(integer_null(UploadLinks::MaxFileCount))
                    .col(big_integer_null(UploadLinks::MaxFileSize))
                    .col(boolean(UploadLinks::SingleUse).default(false))
                    // Optional Argon2id abuse-gate verifier (JSON of the S-A6 `PassphraseVerifier`).
                    .col(json_binary_null(UploadLinks::PassphraseVerifier))
                    // Fail-closed revocation flag.
                    .col(timestamp_with_time_zone_null(UploadLinks::RevokedAt))
                    // Running cumulative-cap counters.
                    .col(big_integer(UploadLinks::BytesUsed).default(0))
                    .col(integer(UploadLinks::FilesUsed).default(0))
                    .col(
                        timestamp_with_time_zone(UploadLinks::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // The opaque-id is the URL lookup key; it must be unique and indistinguishable-404 fast.
        manager
            .create_index(
                Index::create()
                    .name("uq_upload_links_opaque")
                    .table(UploadLinks::Table)
                    .col(UploadLinks::OpaqueId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // The owner's pending-drop inbox.
        manager
            .create_table(
                Table::create()
                    .table(DropInbox::Table)
                    .if_not_exists()
                    .col(string(DropInbox::DropId).primary_key())
                    .col(string(DropInbox::OwnerId))
                    .col(string(DropInbox::LinkId))
                    // The content address of the staged drop blob (never an album asset yet).
                    .col(string_len(DropInbox::CiphertextHash, 64))
                    .col(big_integer(DropInbox::Size))
                    .col(string(DropInbox::ContentType))
                    .col(string_null(DropInbox::SuggestedFilename))
                    // The full unsigned `DropDescriptor` projection, carried opaquely.
                    .col(json_binary(DropInbox::Descriptor))
                    .col(
                        timestamp_with_time_zone(DropInbox::ReceivedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Inbox listing scan (owner's pending drops).
        manager
            .create_index(
                Index::create()
                    .name("idx_drop_inbox_owner")
                    .table(DropInbox::Table)
                    .col(DropInbox::OwnerId)
                    .to_owned(),
            )
            .await?;

        // Adoption lookup: the caller's own drop by content address (invariant 32).
        manager
            .create_index(
                Index::create()
                    .name("idx_drop_inbox_owner_hash")
                    .table(DropInbox::Table)
                    .col(DropInbox::OwnerId)
                    .col(DropInbox::CiphertextHash)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(DropInbox::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(UploadLinks::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum UploadLinks {
    Table,
    LinkId,
    OpaqueId,
    OwnerId,
    AlbumHint,
    ProtocolVersion,
    CryptoSuiteId,
    ExpiresAt,
    MaxTotalBytes,
    MaxFileCount,
    MaxFileSize,
    SingleUse,
    PassphraseVerifier,
    RevokedAt,
    BytesUsed,
    FilesUsed,
    CreatedAt,
}

#[derive(DeriveIden)]
enum DropInbox {
    Table,
    DropId,
    OwnerId,
    LinkId,
    CiphertextHash,
    Size,
    ContentType,
    SuggestedFilename,
    Descriptor,
    ReceivedAt,
}
