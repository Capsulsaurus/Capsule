use sea_orm::{ConnectionTrait, Statement};

use super::EscrowError;

pub struct Mutation;

impl Mutation {
    /// Store (or replace) the passphrase-wrapped master-key escrow for `user_id`.
    ///
    /// `blob` is the opaque `capsule_core::backup` wrap — stored **verbatim**; the server
    /// never interprets it. The only guard is a coarse size sanity bound (non-empty); the
    /// ≥128-bit recovery-secret rule is enforced client-side in core and is deliberately not
    /// re-validated here.
    ///
    /// **Single active escrow.** The guarded `INSERT … ON CONFLICT (user_id) DO UPDATE`
    /// takes the row lock and overwrites `blob` in place, so a replace deletes the prior
    /// ciphertext in the **same statement**: after it returns, the old blob is gone and can
    /// never be fetched or unwrapped again (the guided re-wrap contract — the lost secret must
    /// reach nothing). There is no version/monotonicity here: rotation is a plain replace.
    #[tracing::instrument(skip(db, blob), fields(user_id = %user_id, bytes = blob.len()))]
    pub async fn store<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
        blob: Vec<u8>,
    ) -> Result<(), EscrowError> {
        if blob.is_empty() {
            return Err(EscrowError::Malformed("escrow blob is empty".into()));
        }

        let stmt = Statement::from_sql_and_values(
            db.get_database_backend(),
            r"INSERT INTO backup_escrow (user_id, blob, updated_at)
              VALUES ($1, $2, now())
              ON CONFLICT (user_id) DO UPDATE
                 SET blob       = excluded.blob,
                     updated_at = excluded.updated_at",
            [user_id.into(), blob.into()],
        );
        db.execute(stmt).await?;
        tracing::info!("master-key escrow stored (single active escrow: prior blob replaced)");
        Ok(())
    }
}
