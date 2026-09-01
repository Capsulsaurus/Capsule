use capsule_core::crypto::keys::DeviceDirectory;
use sea_orm::{ConnectionTrait, Statement};

use super::DirectoryError;
use crate::directory::query::Query;

pub struct Mutation;

impl Mutation {
    /// Publish a signed [`DeviceDirectory`] for `user_id`, enforcing the anti-rollback
    /// monotonicity guard (threat-model invariant 23): the accepted version must strictly
    /// exceed the version currently stored for the user.
    ///
    /// `document` is the client-signed canonical CBOR. It is stored **verbatim** — only
    /// `directory_version` is projected out (the server never re-serializes the signed
    /// bytes). Returns the accepted version on success.
    ///
    /// Atomicity: the guarded `INSERT … ON CONFLICT (user_id) DO UPDATE … WHERE
    /// excluded.directory_version > device_directory.directory_version` takes the row lock,
    /// so concurrent publishes are linearised and a non-advancing one updates nothing
    /// (`RETURNING` yields no row) — surfaced as [`DirectoryError::VersionConflict`].
    #[tracing::instrument(skip(db, document), fields(user_id = %user_id, bytes = document.len()))]
    pub async fn publish<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
        document: Vec<u8>,
    ) -> Result<i64, DirectoryError> {
        // Project directory_version out of the opaque signed bytes. Deserialize failures are
        // client errors (a malformed body), not server faults.
        let directory: DeviceDirectory = capsule_core::cbor::from_slice(&document)
            .map_err(|e| DirectoryError::Malformed(e.to_string()))?;
        let submitted = i64::try_from(directory.core.directory_version).map_err(|_| {
            DirectoryError::Malformed(format!(
                "directory_version {} exceeds the representable range",
                directory.core.directory_version
            ))
        })?;

        let stmt = Statement::from_sql_and_values(
            db.get_database_backend(),
            r"INSERT INTO device_directory (user_id, directory_version, document, updated_at)
              VALUES ($1, $2, $3, now())
              ON CONFLICT (user_id) DO UPDATE
                 SET directory_version = excluded.directory_version,
                     document          = excluded.document,
                     updated_at        = excluded.updated_at
                 WHERE excluded.directory_version > device_directory.directory_version
              RETURNING directory_version",
            [user_id.into(), submitted.into(), document.into()],
        );

        // A returned row means the guarded upsert applied (fresh insert or strict advance);
        // no row means the `WHERE excluded.directory_version > …` guard blocked a
        // non-advancing publish (invariant 23).
        if let Some(row) = db.query_one(stmt).await? {
            let accepted = row.try_get::<i64>("", "directory_version")?;
            tracing::info!(accepted_version = accepted, "device directory published");
            Ok(accepted)
        } else {
            // Read the current high-water mark for diagnostics (best-effort; the guard
            // above is the SSoT for the rejection).
            let stored = Query::stored_version(db, user_id)
                .await?
                .unwrap_or_default();
            tracing::warn!(
                stored_version = stored,
                submitted_version = submitted,
                "device directory publish rejected: non-advancing version (invariant 23)"
            );
            Err(DirectoryError::VersionConflict { stored, submitted })
        }
    }
}
