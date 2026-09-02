//! The account cluster (`S-C53`, `S-C54`): one table behind four ports.
//!
//! `AccountRegistry`, `AccountDirectory`, `AccountProfiles` and `PasswordChange` are four ports
//! because they answer four questions with four disclosure contracts — not because they are
//! four stores. One row holds every fact all four read.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Accounts::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Accounts::UserId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    // Compared **verbatim**, which is why there is no folded column beside it.
                    // Case folding is a normalization policy no port describes, and inventing
                    // one here would make the durable adapter disagree with the in-memory one
                    // about who `Foo@example.test` is — a divergence the shared conformance
                    // suite exists to make impossible. Saying otherwise is a decision about
                    // identity, and it belongs to a slice that argues for it.
                    .col(ColumnDef::new(Accounts::Email).text().not_null())
                    .col(ColumnDef::new(Accounts::DisplayName).text().null())
                    // The Argon2id PHC string `auth::credential` produces. Never a password,
                    // and never read above the adapter.
                    .col(ColumnDef::new(Accounts::Credential).text().not_null())
                    // The lockout the directory port makes the adapter's own state: a column on
                    // the account row, not a windowed counter. Rate limiting is `S-C32` and has
                    // no port in this crate.
                    .col(
                        ColumnDef::new(Accounts::Failures)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(Accounts::CreatedAt).big_integer().not_null())
                    .col(ColumnDef::new(Accounts::UpdatedAt).big_integer().not_null())
                    .to_owned(),
            )
            .await?;

        // What decides `Registration::AlreadyExists`. A unique index rather than a read
        // followed by a write: two registrations racing on one address must not both believe
        // they own it, and the port is explicit that the check and the write are one operation.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_accounts_email")
                    .table(Accounts::Table)
                    .col(Accounts::Email)
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Accounts::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Accounts {
    Table,
    UserId,
    Email,
    DisplayName,
    Credential,
    Failures,
    CreatedAt,
    UpdatedAt,
}
