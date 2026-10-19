//! The server's PostgreSQL schema, one migration per ordinal.
//!
//! # What the ordinals cover
//!
//! One ordinal per durable port, landed with the adapter that reads it, so a migration never
//! describes a table nothing reads:
//!
//! - `000001`–`000004` (#402): the asset index, the account cluster, the device-cohort map and
//!   the quota ledger;
//! - `000005`: album membership (`S-C51`, #405);
//! - `000006`: federation's capabilities, revocation list and peers (`S-E2`, #406);
//! - `000007`–`000015` (#446): albums, device directories, moderation, share links, drops,
//!   escrow, the collector's marks, custody receipts and TOTP enrolments;
//! - `000016` (#460): federated accounts on the account table — a nullable credential and
//!   address, and the `(issuer, subject)` link;
//! - `000017` (#524): the checksum ledger's backfill (below);
//! - `000018` (#544): the sealed user identity key beside the escrow;
//! - `000019` (#659): an index on `share_serves (address)`, for the collector's live-link check.
//!
//! # Every instant is a `BIGINT` of microseconds since the Unix epoch
//!
//! Not `TIMESTAMPTZ`. Binding one needs sea-orm's `with-chrono` or `with-time`, and both are
//! refused: `chrono` is banned outside `capsule-cli/entity` by a gate
//! (design/dependencies.md), and `time` would be a third datetime crate with no row in that
//! table. `capsule_server::postgres::time` converts at the adapter boundary, exactly as the
//! CLI converts at its entity boundary. What that costs is SQL date arithmetic; every expiry
//! and retention comparison in these tables is an ordering on one column, and integers order
//! identically.

//!
//! # Every applied migration records a checksum of its source (#524)
//!
//! `seaql_migrations` holds a name and a timestamp, so a migration that was renamed or edited
//! after it was applied is indistinguishable from the one that ran. [`CHECKSUM_LEDGER`] is the
//! companion ledger that closes that: one row per applied migration, holding the SHA-256 of the
//! migration's source file ([`checksum`]). Every migration [`Migrator`] lists is wrapped so that
//! applying it writes its row and rolling it back deletes it, inside the transaction
//! `sea-orm-migration` opens around the whole run — the two ledgers cannot disagree about which
//! migrations are applied.
//!
//! `capsule_server::postgres::assert_schema_current` reads the ledger and refuses to boot when a
//! recorded checksum differs from the one the server was built against. The consequence is a
//! rule: **an applied migration is never edited, not even its comments**. A change to the
//! schema is a new ordinal.
//!
//! `000017` is the ledger's backfill for a database migrated before the ledger existed: it
//! records the checksums of the migrations already applied, as this build knows them.

pub use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::Statement;
use sha2::{Digest as _, Sha256};

/// The companion ledger: `version` (the migration's name) and `checksum` (its [`checksum`]).
pub const CHECKSUM_LEDGER: &str = "capsule_migration_checksums";

/// Declare every migration once: its module, and its source for the checksum.
///
/// One list rather than a `mod` list and a `vec!` beside it, so a migration cannot be added to
/// the migrator without its source being the one hashed — the file is named by the module.
macro_rules! migrations {
    ($($module:ident),+ $(,)?) => {
        $(mod $module;)+

        /// Every migration with the text of the file that defines it, oldest first.
        fn sources() -> Vec<(Box<dyn MigrationTrait>, &'static str)> {
            vec![$((
                Box::new($module::Migration) as Box<dyn MigrationTrait>,
                include_str!(concat!(stringify!($module), ".rs")),
            )),+]
        }
    };
}

migrations![
    m20260902_000001_asset_index,
    m20260902_000002_accounts,
    m20260902_000003_cohorts,
    m20260902_000004_quota,
    m20260902_000005_album_membership,
    m20260902_000006_federation,
    m20260902_000007_albums,
    m20260902_000008_device_directories,
    m20260902_000009_moderation,
    m20260902_000010_shares,
    m20260902_000011_drops,
    m20260902_000012_escrow,
    m20260902_000013_collection_marks,
    m20260902_000014_receipts,
    m20260902_000015_totp,
    m20260902_000016_federated_accounts,
    m20260902_000017_migration_checksums,
    m20260902_000018_sealed_identities,
    m20260902_000019_share_serves_address_index,
];

/// The content checksum of one migration's source: lowercase hex SHA-256.
///
/// Line endings are normalised to `\n` first, so a checkout that converts them (a Windows
/// `core.autocrlf`) builds the same checksum as the one that applied the migration.
#[must_use]
pub fn checksum(source: &str) -> String {
    let normalised = source.replace("\r\n", "\n");
    hex::encode(Sha256::digest(normalised.as_bytes()))
}

/// Every migration's name and [`checksum`], oldest first.
///
/// What `capsule-server`'s compiled-in `EXPECTED_MIGRATIONS` is a copy of; its test compares the
/// two.
#[must_use]
pub fn checksums() -> Vec<(String, String)> {
    sources()
        .into_iter()
        .map(|(migration, source)| (migration.name().to_owned(), checksum(source)))
        .collect()
}

/// Create [`CHECKSUM_LEDGER`] if it is not there yet.
///
/// Idempotent, and called before every ledger write rather than by one migration, because the
/// first migrations on a fresh database run before `000017` does.
async fn ensure_ledger(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .get_connection()
        .execute_unprepared(&format!(
            "CREATE TABLE IF NOT EXISTS {CHECKSUM_LEDGER} \
               (version TEXT PRIMARY KEY, checksum TEXT NOT NULL)"
        ))
        .await?;
    Ok(())
}

/// Record `checksum` as the applied checksum of `version`, replacing any earlier row.
async fn record(manager: &SchemaManager<'_>, version: &str, checksum: &str) -> Result<(), DbErr> {
    ensure_ledger(manager).await?;
    let connection = manager.get_connection();
    connection
        .execute(Statement::from_sql_and_values(
            connection.get_database_backend(),
            format!(
                "INSERT INTO {CHECKSUM_LEDGER} (version, checksum) VALUES ($1, $2) \
                 ON CONFLICT (version) DO UPDATE SET checksum = EXCLUDED.checksum"
            ),
            [version.into(), checksum.into()],
        ))
        .await?;
    Ok(())
}

/// A migration that keeps [`CHECKSUM_LEDGER`] in step with `seaql_migrations`.
struct Checksummed {
    inner: Box<dyn MigrationTrait>,
    checksum: String,
}

impl MigrationName for Checksummed {
    fn name(&self) -> &str {
        self.inner.name()
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Checksummed {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        self.inner.up(manager).await?;
        record(manager, self.inner.name(), &self.checksum).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        self.inner.down(manager).await?;
        ensure_ledger(manager).await?;
        let connection = manager.get_connection();
        connection
            .execute(Statement::from_sql_and_values(
                connection.get_database_backend(),
                format!("DELETE FROM {CHECKSUM_LEDGER} WHERE version = $1"),
                [self.inner.name().into()],
            ))
            .await?;
        Ok(())
    }
}

/// The server's migrator.
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        sources()
            .into_iter()
            .map(|(inner, source)| {
                Box::new(Checksummed {
                    inner,
                    checksum: checksum(source),
                }) as Box<dyn MigrationTrait>
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{checksum, checksums, sources};

    #[test]
    fn a_checksum_is_the_hex_sha256_of_the_source() {
        assert_eq!(
            checksum("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_checksum_does_not_depend_on_line_endings() {
        assert_eq!(checksum("a\r\nb\r\n"), checksum("a\nb\n"));
    }

    #[test]
    fn an_edit_changes_the_checksum() {
        assert_ne!(checksum("CREATE TABLE a"), checksum("CREATE TABLE a "));
    }

    /// Each migration hashes its own file, and no two files are the same text.
    #[test]
    fn every_migration_has_a_distinct_checksum() {
        let all = checksums();
        assert_eq!(all.len(), sources().len());
        for (index, (name, sum)) in all.iter().enumerate() {
            assert!(
                all.iter().skip(index + 1).all(|(_, other)| other != sum),
                "{name} shares its checksum with a later migration"
            );
        }
    }
}
