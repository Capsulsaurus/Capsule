//! Album provisioning — binding a client-derived album id to an owner (slice `S-C25`).
//!
//! A container album's id is **derived deterministically from the account master key**
//! ([Organization — The Default Album]), so every one of a user's devices — and the same user
//! after a recovery on a fresh device — recomputes the *same* id with no synced pointer. The
//! server's job is only to learn that the UUID exists and whose it is: it is
//! "a plain UUID the server stores and serves" ([Filesystem — Server]), never a key holder and
//! never a namer.
//!
//! That shape dictates the two properties this module exists to guarantee:
//!
//! - **Idempotent.** Re-provisioning an id the caller already owns is a success that writes
//!   nothing, not a conflict. A second device, a re-install, and a passphrase recovery all
//!   submit the same id and must all succeed — otherwise the client would need a
//!   "have I registered yet?" flag, which is exactly the synced pointer the derivation exists
//!   to avoid.
//! - **Owner-bound, and quiet about it.** An id already bound to a *different* account is
//!   refused with a single stable code and a single message that says nothing about whether
//!   that id exists. Album ids are unguessable before creation, and probing must not turn the
//!   endpoint into an existence oracle over other accounts' derived ids.
//!
//! # No name, by construction
//!
//! Provisioning accepts **only** an album id. `albums.name` and `albums.description` are
//! plaintext columns predating the key-free model — the server is not entitled to album
//! titles, which live in the encrypted sidecar clients already read them from. Both are
//! `NOT NULL`, so they are written as the **empty string**: no client-supplied text can reach
//! either column, because no client-supplied text is accepted. (Retiring the two columns
//! outright is slice `S-C26`.)
//!
//! [Organization — The Default Album]: ../../../../capsule-docs/src/content/docs/design/organization.md
//! [Filesystem — Server]: ../../../../capsule-docs/src/content/docs/design/filesystem/server.md

use ::entity::{album, owner, owner_member, time};
use capsule_core::models::album::AlbumAccess;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait,
    QueryFilter, QuerySelect, Set, Statement, TransactionTrait,
};

use super::Query;

/// The values written into the plaintext `name` / `description` columns. Empty, always: the
/// server is not entitled to album titles (see the module docs and slice `S-C26`).
const NO_PLAINTEXT: &str = "";

/// What a provisioning call did. Both variants are successes — the distinction exists so the
/// transport can answer `201` vs `200` and so a caller can log the difference, never so a
/// client has to branch on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisionOutcome {
    /// The album row did not exist and was created, bound to the caller's owner group.
    Created,
    /// The album already existed and the caller can already write to it. Nothing was
    /// written (beyond clearing a soft-delete marker on the caller's own album — see
    /// [`Mutation::provision_album`]).
    AlreadyProvisioned,
}

/// Why a provisioning call was refused.
#[derive(Debug, thiserror::Error)]
pub enum ProvisionError {
    /// The submitted id is not a canonical lowercase hyphenated UUID.
    #[error("album_id must be a canonical lowercase hyphenated UUID")]
    InvalidAlbumId,
    /// The id cannot be bound to this account. Deliberately **one** variant with **one**
    /// message covering every refusal reason, so the response is identical whether the id is
    /// bound to another account or is otherwise unavailable — the endpoint is not an
    /// existence oracle over other accounts' derived album ids.
    #[error("this album id is not available to this account")]
    NotAvailable,
    /// The database failed.
    #[error(transparent)]
    Db(#[from] DbErr),
}

pub struct Mutation;

impl Mutation {
    /// Bind `album_id` to `user_id`'s owner group, creating the album row if it does not
    /// exist yet.
    ///
    /// The whole call runs in one transaction:
    ///
    /// 1. `album_id` is validated as a canonical hyphenated UUID ([`validate_album_id`]) —
    ///    the server stores exactly the id the client derived, in exactly one spelling, so
    ///    two capitalizations can never become two rows.
    /// 2. The caller's solo owner group is ensured (`owners.id == user_id`, with the user as
    ///    its member), under that owner row's lock so concurrent provisions serialize.
    /// 3. The album row is inserted `ON CONFLICT DO NOTHING`. Inserting it is the race
    ///    resolver: exactly one concurrent caller sees a row affected and reports
    ///    [`ProvisionOutcome::Created`].
    /// 4. If the insert was a no-op the row already existed, so the caller's **real** write
    ///    capability on it is checked (owner-group membership or an `edit` share — the same
    ///    [`Query::get_album_access`] that backs [invariant 6]). Writable →
    ///    [`ProvisionOutcome::AlreadyProvisioned`]; anything else → [`ProvisionError::NotAvailable`].
    ///    Nothing here weakens invariant 6: the album genuinely exists and the caller
    ///    genuinely holds write capability on it before this returns success.
    ///
    /// A soft-deleted album the caller **already owns** has its `deleted_at` cleared, because
    /// the client's contract is that it re-derives and re-registers the same de facto album
    /// rather than inventing a new id; leaving it tombstoned would hide it from the sync feed
    /// while still admitting uploads.
    ///
    /// [invariant 6]: ../../../../capsule-docs/src/content/docs/design/threat-model/validation.md
    #[tracing::instrument(skip(db), fields(user_id = %user_id, album_id = %album_id))]
    pub async fn provision_album(
        db: &DatabaseConnection,
        user_id: &str,
        album_id: &str,
    ) -> Result<ProvisionOutcome, ProvisionError> {
        let album_id = validate_album_id(album_id)?;

        let txn = db.begin().await?;
        let owner_id = ensure_owner_group(&txn, user_id).await?;

        let now = time::now_entity();
        let inserted = album::Entity::insert(album::ActiveModel {
            id: Set(album_id.clone()),
            owner_id: Set(owner_id.clone()),
            // Never client-supplied. See NO_PLAINTEXT and the module docs.
            name: Set(NO_PLAINTEXT.to_string()),
            description: Set(NO_PLAINTEXT.to_string()),
            created_at: Set(now),
            modified_at: Set(now),
            deleted_at: Set(None),
        })
        .on_conflict(
            OnConflict::column(album::Column::Id)
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(&txn)
        .await?;

        if inserted == 1 {
            txn.commit().await?;
            tracing::info!(%owner_id, "album provisioned: row created and bound to owner");
            return Ok(ProvisionOutcome::Created);
        }

        // The row already existed — re-provisioning. Success requires *real* write capability,
        // exactly as invariant 6 demands of an upload.
        let access = Query::get_album_access(&txn, user_id, &album_id).await?;
        if !access.as_ref().is_some_and(AlbumAccess::is_write) {
            txn.rollback().await?;
            tracing::info!("album provisioning refused: id is not available to this account");
            return Err(ProvisionError::NotAvailable);
        }

        // The caller's own album, back from a soft delete: re-registering revives it.
        let existing = album::Entity::find_by_id(album_id.clone())
            .one(&txn)
            .await?
            .ok_or(ProvisionError::NotAvailable)?;
        if existing.deleted_at.is_some() {
            let mut revived: album::ActiveModel = existing.into();
            revived.deleted_at = Set(None);
            revived.modified_at = Set(now);
            revived.update(&txn).await?;
            tracing::info!("album provisioning revived a soft-deleted album for its owner");
        }

        txn.commit().await?;
        tracing::info!("album provisioning is a no-op: already bound to this account");
        Ok(ProvisionOutcome::AlreadyProvisioned)
    }
}

/// Accept only the canonical lowercase hyphenated UUID spelling of `album_id`, returning it
/// owned.
///
/// The album id is a UUID by contract, and the server stores it verbatim as an exact-match
/// key across six tables and the sync feed. Round-tripping through [`uuid::Uuid`] and
/// requiring byte equality rejects every other spelling the parser tolerates (braced, URN,
/// simple, upper-case) *before* it can become a second row for the same album.
fn validate_album_id(album_id: &str) -> Result<String, ProvisionError> {
    let parsed = uuid::Uuid::parse_str(album_id).map_err(|_| ProvisionError::InvalidAlbumId)?;
    if parsed.hyphenated().to_string() != album_id {
        return Err(ProvisionError::InvalidAlbumId);
    }
    Ok(album_id.to_string())
}

/// Ensure `user_id` has a solo owner group and return its id.
///
/// The owner group is keyed **on the user id** (`owners.id == user_id`, one `owner_members`
/// row), the same shape the upload tests and the drop-adoption path assume for a solo
/// account: it keeps `get_album_access(U, A)` and the `assets.owner_id` foreign key
/// consistent without a second identifier to reconcile. Multi-user owner groups are created
/// elsewhere and are untouched by this.
///
/// Idempotent and concurrency-safe: the `ON CONFLICT DO NOTHING` insert plus the subsequent
/// `SELECT … FOR UPDATE` serialize concurrent provisions for the same account on the owner
/// row, so the membership row is inserted at most once.
async fn ensure_owner_group<C: ConnectionTrait>(
    txn: &C,
    user_id: &str,
) -> Result<String, ProvisionError> {
    txn.execute(Statement::from_sql_and_values(
        txn.get_database_backend(),
        r"INSERT INTO owners (id, created_at) VALUES ($1, now())
          ON CONFLICT (id) DO NOTHING",
        [user_id.into()],
    ))
    .await?;

    // Take the owner row's lock so two concurrent provisions for this account cannot both
    // decide the membership row is missing.
    owner::Entity::find_by_id(user_id)
        .lock_exclusive()
        .one(txn)
        .await?
        .ok_or_else(|| {
            ProvisionError::Db(DbErr::Custom(format!(
                "owner group {user_id} vanished immediately after being ensured"
            )))
        })?;

    let member_exists = owner_member::Entity::find()
        .filter(owner_member::Column::OwnerId.eq(user_id))
        .filter(owner_member::Column::UserId.eq(user_id))
        .one(txn)
        .await?
        .is_some();
    if !member_exists {
        owner_member::ActiveModel {
            owner_id: Set(user_id.to_string()),
            user_id: Set(user_id.to_string()),
            created_at: Set(time::now_entity()),
            ..Default::default()
        }
        .insert(txn)
        .await?;
        tracing::info!(user_id, "solo owner group created for account");
    }

    Ok(user_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_uuid_is_accepted_verbatim() {
        let id = "0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e35";
        assert_eq!(validate_album_id(id).expect("canonical uuid"), id);
    }

    #[test]
    fn a_nanoid_is_not_an_album_id() {
        assert!(matches!(
            validate_album_id("V1StGXR8_Z5jdHi6B-myT"),
            Err(ProvisionError::InvalidAlbumId)
        ));
    }

    /// Every non-canonical spelling the UUID parser tolerates is refused, so one album can
    /// never become two rows.
    #[test]
    fn non_canonical_spellings_are_refused() {
        for id in [
            "0198F3C2-9C4A-7B3D-8F21-4D7C9A1B2E35",          // upper-case
            "0198f3c29c4a7b3d8f214d7c9a1b2e35",              // simple (no hyphens)
            "{0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e35}",        // braced
            "urn:uuid:0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e35", // URN
        ] {
            assert!(
                matches!(validate_album_id(id), Err(ProvisionError::InvalidAlbumId)),
                "{id} must be refused"
            );
        }
    }

    #[test]
    fn empty_and_garbage_ids_are_refused() {
        for id in ["", "   ", "not-a-uuid", "../../etc/passwd"] {
            assert!(matches!(
                validate_album_id(id),
                Err(ProvisionError::InvalidAlbumId)
            ));
        }
    }

    /// The canonical form fits the widened `varchar(64)` album-id columns with room to spare.
    #[test]
    fn a_canonical_uuid_fits_the_widened_columns() {
        let id = uuid::Uuid::now_v7().hyphenated().to_string();
        assert_eq!(id.len(), 36);
        assert!(id.len() <= 64);
    }

    /// The plaintext columns are written empty, never from client input — the privacy
    /// constraint stated as a value, not a comment.
    #[test]
    fn plaintext_columns_are_written_empty() {
        assert_eq!(NO_PLAINTEXT, "");
    }
}
