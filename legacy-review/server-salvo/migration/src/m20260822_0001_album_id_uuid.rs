//! Widen every album-id column so it can hold a **UUID** (slice `S-C25`).
//!
//! The album id is a plain UUID the server stores and serves ([Filesystem — Server], the
//! `default_album_id` paragraph), derived deterministically from the account master key so a
//! device can recompute it after recovery ([Organization — The Default Album]). Every
//! album-id column, however, was still `character(21)` — a nanoid, from the original
//! pre-key-free schema — so a client could physically never name its own album to the server:
//! Postgres answered `value too long for type character(21)` and `POST /upload` therefore
//! refused every real push at [invariant 6]. The columns were the bug, not the client.
//!
//! Six columns move together, because the album id flows through all of them:
//!
//! | Column | Origin |
//! | --- | --- |
//! | `albums.id` | `m20250210_000000_initial_schema` |
//! | `album_shares.album_id` | `m20250210_000000_initial_schema` |
//! | `assets.album_id` (nullable) | `m20250210_000000_initial_schema` |
//! | `sync_album_seq.album_id` | `m20260710_000000_sync_feed` |
//! | `sync_entries.album_id` | `m20260710_000000_sync_feed` |
//! | `lifecycle_op_replay.album_id` | `m20260710_000006_lifecycle_ops` |
//!
//! # Width: `varchar(64)`
//!
//! A canonical hyphenated UUID is **36** characters, so 64 holds one with 28 characters to
//! spare — room for a future prefixed or namespaced identifier without a second rewrite —
//! while staying the width already used for the schema's other opaque identifier columns
//! (`assets.file_hash`, the federation/moderation id columns: 64 is the most common
//! `string_len` in the migration set). It is deliberately **not** wider: the column is an
//! exact-match lookup key on the sync feed's hot path, and a bounded width keeps the intent
//! ("an identifier, not free text") legible in the schema.
//!
//! `varchar`, **not** a wider `char`: `character(n)` is blank-padded to `n`, so a 36-char
//! UUID stored in `char(64)` would read back with 28 trailing spaces and no longer compare
//! equal to what the client sent. `varchar(64)` stores exactly what is written. Postgres
//! strips the (non-existent) trailing blanks when casting the existing `bpchar` values, and
//! every stored nanoid is exactly 21 characters, so **the conversion is backfill-safe**: no
//! existing row changes value, and no existing row can overflow the new width.
//!
//! # Foreign keys and indexes
//!
//! Two foreign keys reference `albums.id` and so must move with the type — the referencing
//! and referenced columns have to stay type-compatible, and Postgres will not let one side
//! change out from under a live constraint across the `bpchar`→`varchar` boundary:
//!
//! - `fk_album_shares_album_id` — `album_shares.album_id` → `albums.id` (`ON DELETE CASCADE`)
//! - `fk_assets_album_id` — `assets.album_id` → `albums.id` (`ON DELETE SET NULL`)
//!
//! Both are dropped, all six columns are retyped, and both are recreated with **identical**
//! names and referential actions. The three feed/replay tables carry no foreign key on their
//! album id (they are deliberately decoupled append-only logs), so nothing else moves.
//!
//! Indexes need no explicit handling: `ALTER TABLE … ALTER COLUMN … TYPE` rebuilds every
//! dependent index in place, keeping its name, kind, and uniqueness. That covers the
//! `albums` and `sync_album_seq` primary keys, `idx_assets_album_id` (hash),
//! `uq_sync_entries_album_seq` (unique), and `idx_sync_entries_album_feed`.
//!
//! Forward-only: widening is not reverted (a `down` that narrowed the columns would truncate
//! every UUID it found), so `down` is intentionally a no-op rather than a faithful inverse.
//!
//! [Filesystem — Server]: ../../../capsule-docs/src/content/docs/design/filesystem/server.md
//! [Organization — The Default Album]: ../../../capsule-docs/src/content/docs/design/organization.md
//! [invariant 6]: ../../../capsule-docs/src/content/docs/design/threat-model/validation.md

use sea_orm_migration::prelude::*;

/// The width every album-id column is widened to. A canonical hyphenated UUID is 36
/// characters; see the module docs for why 64 and why `varchar`.
pub(crate) const ALBUM_ID_LEN: u32 = 64;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 1. Release the two foreign keys that pin `albums.id`'s type from the other side.
        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .table(AlbumShares::Table)
                    .name("fk_album_shares_album_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_foreign_key(
                ForeignKey::drop()
                    .table(Assets::Table)
                    .name("fk_assets_album_id")
                    .to_owned(),
            )
            .await?;

        // 2. Retype all six columns. Nullability is preserved exactly — only `assets.album_id`
        //    is nullable (an asset may be filed in no album).
        manager
            .alter_table(
                Table::alter()
                    .table(Albums::Table)
                    .modify_column(
                        ColumnDef::new(Albums::Id)
                            .string_len(ALBUM_ID_LEN)
                            .not_null()
                            .to_owned(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(AlbumShares::Table)
                    .modify_column(
                        ColumnDef::new(AlbumShares::AlbumId)
                            .string_len(ALBUM_ID_LEN)
                            .not_null()
                            .to_owned(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Assets::Table)
                    .modify_column(
                        ColumnDef::new(Assets::AlbumId)
                            .string_len(ALBUM_ID_LEN)
                            .null()
                            .to_owned(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(SyncAlbumSeq::Table)
                    .modify_column(
                        ColumnDef::new(SyncAlbumSeq::AlbumId)
                            .string_len(ALBUM_ID_LEN)
                            .not_null()
                            .to_owned(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(SyncEntries::Table)
                    .modify_column(
                        ColumnDef::new(SyncEntries::AlbumId)
                            .string_len(ALBUM_ID_LEN)
                            .not_null()
                            .to_owned(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(LifecycleOpReplay::Table)
                    .modify_column(
                        ColumnDef::new(LifecycleOpReplay::AlbumId)
                            .string_len(ALBUM_ID_LEN)
                            .not_null()
                            .to_owned(),
                    )
                    .to_owned(),
            )
            .await?;

        // 3. Restore both foreign keys, name-for-name and action-for-action.
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_album_shares_album_id")
                    .from(AlbumShares::Table, AlbumShares::AlbumId)
                    .to(Albums::Table, Albums::Id)
                    .on_delete(ForeignKeyAction::Cascade)
                    .to_owned(),
            )
            .await?;
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_assets_album_id")
                    .from(Assets::Table, Assets::AlbumId)
                    .to(Albums::Table, Albums::Id)
                    .on_delete(ForeignKeyAction::SetNull)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only: narrowing back to char(21) would truncate every UUID the widened
        // columns now hold, so there is deliberately no faithful inverse.
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Albums {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum AlbumShares {
    Table,
    AlbumId,
}

#[derive(DeriveIden)]
enum Assets {
    Table,
    AlbumId,
}

#[derive(DeriveIden)]
enum SyncAlbumSeq {
    Table,
    AlbumId,
}

#[derive(DeriveIden)]
enum SyncEntries {
    Table,
    AlbumId,
}

#[derive(DeriveIden)]
enum LifecycleOpReplay {
    Table,
    AlbumId,
}
