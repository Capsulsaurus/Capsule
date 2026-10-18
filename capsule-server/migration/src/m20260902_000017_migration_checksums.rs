//! The checksum ledger's backfill (#524).
//!
//! Every migration records its own checksum as it is applied (the crate docs), so on a fresh
//! database this ordinal finds the ledger already full and changes nothing. What it is for is a
//! database migrated **before** the ledger existed: there, `000001`–`000016` are in
//! `seaql_migrations` with no checksum anywhere, and `serve` would refuse them as unrecorded.
//!
//! The backfill records, for each migration `seaql_migrations` holds, the checksum this build
//! computes from its source. That trusts the source as it stands at this ordinal to be the text
//! that ran — the one point at which the ledger has to take the code's word for it; from here on
//! a later edit is a refusal.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::Statement;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        crate::ensure_ledger(manager).await?;
        let connection = manager.get_connection();
        let ledger = crate::CHECKSUM_LEDGER;
        for (version, checksum) in crate::checksums() {
            // Only an applied migration gets a row, and an existing row is the one written when
            // it was applied — never overwritten here.
            connection
                .execute(Statement::from_sql_and_values(
                    connection.get_database_backend(),
                    format!(
                        "INSERT INTO {ledger} (version, checksum) \
                         SELECT $1, $2 WHERE EXISTS \
                           (SELECT 1 FROM seaql_migrations WHERE version = $1) \
                         ON CONFLICT (version) DO NOTHING"
                    ),
                    [version.into(), checksum.into()],
                ))
                .await?;
        }
        Ok(())
    }

    /// Nothing to undo: the rows it backfilled belong to the earlier migrations, and each of
    /// those deletes its own row as it is rolled back.
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
