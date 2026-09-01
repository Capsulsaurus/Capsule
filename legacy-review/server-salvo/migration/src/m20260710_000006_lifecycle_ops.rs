//! The lifecycle-write idempotency store (slice `S-C16`).
//!
//! `lifecycle_op_replay` is the durable content-hash replay store for the generic
//! `POST /albums/{album_id}/ops` surface. Keyed by the SHA-256 of the signed op bundle
//! (canonical-CBOR manifest ‖ metadata blob), each row remembers the **byte-identical**
//! response the first acceptance produced, inserted inside the same finalization transaction
//! that appended the provenance record and minted the `sync_seq`. A resubmission of the exact
//! bundle short-circuits to the stored response — the op is applied at most once even under a
//! lost-ACK retry (the lifecycle analogue of the upload chunk-replay tuple, invariant 12).

use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(LifecycleOpReplay::Table)
                    .if_not_exists()
                    // The op bundle's content address (lowercase hex SHA-256) — the row key.
                    .col(string_len(LifecycleOpReplay::OpHash, 64).primary_key())
                    .col(char_len(LifecycleOpReplay::AlbumId, 21))
                    // The signed manifest's `file_id` (UUID) — the asset the op chains onto.
                    .col(string_len(LifecycleOpReplay::AssetId, 36))
                    .col(small_integer(LifecycleOpReplay::Action))
                    .col(integer(LifecycleOpReplay::StatusCode))
                    .col(
                        ColumnDef::new(LifecycleOpReplay::ResponseBody)
                            .binary()
                            .not_null(),
                    )
                    .col(
                        timestamp_with_time_zone(LifecycleOpReplay::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(LifecycleOpReplay::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum LifecycleOpReplay {
    Table,
    OpHash,
    AlbumId,
    AssetId,
    Action,
    StatusCode,
    ResponseBody,
    CreatedAt,
}
