//! Second-factor enrolments: one TOTP secret per account, and its replay ledger.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One row per account, keyed on it: an account has at most one second factor, and the
        // primary key is what lets `begin` be one upsert that refuses to replace an active row.
        //
        // `state` is `pending` or `active`, constrained so a row no version of this server
        // wrote cannot be read as either. `last_step` is the replay ledger's whole state: the
        // highest RFC 6238 step a code was accepted at, compared-and-set in one `UPDATE`.
        //
        // `secret` is the base32 shared secret, verbatim. It is a credential — anyone holding it
        // mints codes — and it is server-generated, so there is no caller-supplied value to hash
        // and verify against: TOTP needs the secret itself back. Encrypting it at rest under a
        // server key is a deployment-wide key-management decision this table does not make.
        manager
            .create_table(
                Table::create()
                    .table(TotpEnrollments::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TotpEnrollments::UserId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(TotpEnrollments::Secret).text().not_null())
                    .col(
                        ColumnDef::new(TotpEnrollments::State)
                            .text()
                            .not_null()
                            .check(Expr::col(TotpEnrollments::State).is_in(["pending", "active"])),
                    )
                    .col(
                        ColumnDef::new(TotpEnrollments::LastStep)
                            .big_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(TotpEnrollments::EnrolledAt)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TotpEnrollments::ActivatedAt)
                            .big_integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(TotpEnrollments::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum TotpEnrollments {
    Table,
    UserId,
    Secret,
    State,
    LastStep,
    EnrolledAt,
    ActivatedAt,
}
