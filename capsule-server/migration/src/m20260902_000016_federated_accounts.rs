//! Federated accounts (#460): the OIDC relying party's accounts, on the one account table.
//!
//! An account created by an OpenID Connect sign-in is a row of `accounts` like any other — the
//! profile, the registration uniqueness check and the lockout all read the same row — with three
//! differences, each made a column fact here:
//!
//! - **`credential` is nullable.** A federated account has no password, and that is structural:
//!   the account adapters refuse a credential presentation against a row whose credential is
//!   null, so no password an attacker could guess exists for it.
//! - **`email` is nullable.** A federated row records the address only when the provider
//!   asserted it **verified**; an unverified or absent address reserves nothing (the OIDC module's
//!   docs, decision 11 of #459). The unique index on `email` keeps holding, because Postgres never
//!   counts two nulls as equal.
//! - **`(oidc_issuer, oidc_subject)` is the link**, unique, and the key `resolve_or_create`
//!   looks a returning identity up by.
//!
//! The `CHECK`s are what keep a row from being neither kind of account: a link is a pair or
//! nothing, and every row has a way in — a credential or a link — and a password account has an
//! address to sign in with.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

/// The constraints, by name so `down` can drop them.
const CONSTRAINTS: &str = "
ALTER TABLE accounts
  ADD CONSTRAINT accounts_oidc_link_is_a_pair
    CHECK ((oidc_issuer IS NULL) = (oidc_subject IS NULL)),
  ADD CONSTRAINT accounts_have_a_way_in
    CHECK (credential IS NOT NULL OR oidc_issuer IS NOT NULL),
  ADD CONSTRAINT accounts_with_a_password_have_an_address
    CHECK (credential IS NULL OR email IS NOT NULL)
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Accounts::Table)
                    .modify_column(ColumnDef::new(Accounts::Credential).text().null())
                    .modify_column(ColumnDef::new(Accounts::Email).text().null())
                    .add_column(ColumnDef::new(Accounts::OidcIssuer).text().null())
                    .add_column(ColumnDef::new(Accounts::OidcSubject).text().null())
                    .to_owned(),
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(CONSTRAINTS)
            .await?;
        // What decides `FederatedLink::Linked`, and what two callbacks for one new identity race
        // on: the second insert conflicts here rather than creating a second account.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_accounts_oidc_link")
                    .table(Accounts::Table)
                    .col(Accounts::OidcIssuer)
                    .col(Accounts::OidcSubject)
                    .unique()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    /// Back to a table of password accounts only — which **deletes every federated account**,
    /// because a row with no credential cannot satisfy the restored `NOT NULL`. There is no
    /// lossless way down: the schema being returned to has no place for those rows.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_accounts_oidc_link")
                    .table(Accounts::Table)
                    .to_owned(),
            )
            .await?;
        let connection = manager.get_connection();
        connection
            .execute_unprepared(
                "ALTER TABLE accounts \
                   DROP CONSTRAINT accounts_oidc_link_is_a_pair, \
                   DROP CONSTRAINT accounts_have_a_way_in, \
                   DROP CONSTRAINT accounts_with_a_password_have_an_address",
            )
            .await?;
        connection
            .execute_unprepared("DELETE FROM accounts WHERE credential IS NULL OR email IS NULL")
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Accounts::Table)
                    .drop_column(Accounts::OidcSubject)
                    .drop_column(Accounts::OidcIssuer)
                    .modify_column(ColumnDef::new(Accounts::Email).text().not_null())
                    .modify_column(ColumnDef::new(Accounts::Credential).text().not_null())
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Accounts {
    Table,
    Email,
    Credential,
    OidcIssuer,
    OidcSubject,
}
