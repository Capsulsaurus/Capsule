use entity::quota_ledger;
use jiff::Timestamp;
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbErr, EntityTrait,
    QuerySelect, Set, Statement, TransactionTrait,
};

use super::query::Query;
use super::{BlobKind, QuotaError, QuotaLimits, QuotaState, WriteClass};

/// The result of charging a blob to the quota ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChargeOutcome {
    /// A new content address — `byte_size` bytes were newly debited to the user.
    Charged {
        /// The bytes debited.
        byte_size: u64,
    },
    /// The content address already existed — a content-addressed dedup **merge**: the
    /// refcount was incremented, no new bytes were charged (counts against the first
    /// attributee only).
    Merged {
        /// The refcount after the merge.
        refcount: i32,
    },
}

/// The result of releasing one reference to a ledger blob (an asset hard-purge).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseOutcome {
    /// Refcount decremented but still positive — the blob (and its bytes) is retained.
    Retained {
        /// The refcount after the decrement.
        refcount: i32,
    },
    /// The last reference dropped — the row was garbage-collected and the bytes credited
    /// back to `attributed_user_id`.
    GarbageCollected {
        /// The bytes credited back.
        freed_bytes: u64,
        /// The user the bytes were credited to.
        attributed_user_id: String,
    },
    /// No such content address in the ledger (already GC'd, or never charged here).
    Absent,
}

pub struct Mutation;

impl Mutation {
    /// Enforce quota for a write of `additional_bytes` in class `class` by `user_id`.
    ///
    /// Refreshes the grace-window marker from the freshly computed usage, then applies the
    /// per-class rule:
    /// - [`WriteClass::UploadSession`] — the single **hard** gate: refused if
    ///   `used + additional_bytes` crosses the hard limit ([`QuotaError::Exceeded`]).
    /// - [`WriteClass::MetadataGrowth`] — refused **only** when the account is Grace-expired
    ///   ([`QuotaError::GraceLocked`]); permitted in every other state (metadata edits keep
    ///   working while merely Hard-exceeded).
    /// - [`WriteClass::Lifecycle`] — always admitted (a user must be able to delete their way
    ///   back under quota).
    ///
    /// `Suspended` is reported by [`Query::current_status`] but not enforced here — suspension
    /// enforcement at session creation is the moderation slice's responsibility.
    #[tracing::instrument(skip(db, limits), fields(user_id = %user_id, ?class, additional_bytes))]
    pub async fn check<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
        additional_bytes: u64,
        class: WriteClass,
        limits: &QuotaLimits,
    ) -> Result<(), QuotaError> {
        let used = Query::used(db, user_id).await?;
        Self::refresh_marker(db, user_id, used, limits).await?;
        let (since, suspended) = Query::lifecycle_state(db, user_id).await?;
        let state = QuotaState::classify(used, limits, since, suspended, Timestamp::now());

        match class {
            WriteClass::Lifecycle => Ok(()),
            WriteClass::MetadataGrowth => {
                if state == QuotaState::GraceExpired {
                    tracing::info!(
                        user_id,
                        used,
                        "metadata-growth write refused: account grace-expired"
                    );
                    return Err(QuotaError::GraceLocked {
                        used,
                        hard_limit: limits.hard_limit,
                    });
                }
                Ok(())
            }
            WriteClass::UploadSession => {
                if !limits.is_unlimited()
                    && used.saturating_add(additional_bytes) > limits.hard_limit
                {
                    tracing::info!(
                        user_id,
                        used,
                        additional_bytes,
                        hard_limit = limits.hard_limit,
                        "upload session refused: would cross hard quota limit"
                    );
                    return Err(QuotaError::Exceeded {
                        used,
                        additional: additional_bytes,
                        hard_limit: limits.hard_limit,
                    });
                }
                Ok(())
            }
        }
    }

    /// Maintain the grace-window clock (`user_quota.hard_exceeded_since`): set it the first
    /// time usage is observed at/above the hard limit (preserving an existing mark), clear it
    /// when usage falls back below. A no-op under an unlimited hard limit.
    async fn refresh_marker<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
        used: u64,
        limits: &QuotaLimits,
    ) -> Result<(), DbErr> {
        if limits.is_unlimited() {
            return Ok(());
        }
        let backend = db.get_database_backend();
        let stmt = if used >= limits.hard_limit {
            Statement::from_sql_and_values(
                backend,
                r"INSERT INTO user_quota (user_id, hard_exceeded_since, updated_at)
                  VALUES ($1, now(), now())
                  ON CONFLICT (user_id) DO UPDATE
                     SET hard_exceeded_since =
                             COALESCE(user_quota.hard_exceeded_since, EXCLUDED.hard_exceeded_since),
                         updated_at = now()",
                [user_id.into()],
            )
        } else {
            Statement::from_sql_and_values(
                backend,
                r"UPDATE user_quota SET hard_exceeded_since = NULL, updated_at = now()
                  WHERE user_id = $1 AND hard_exceeded_since IS NOT NULL",
                [user_id.into()],
            )
        };
        db.execute(stmt).await?;
        Ok(())
    }

    /// Charge a locally produced auxiliary blob (metadata / derivative / provenance) to
    /// `user_id`, content-addressed deduped. A hash already present is a [merge](ChargeOutcome::Merged).
    #[tracing::instrument(skip(db), fields(user_id = %user_id, content_hash = %content_hash, byte_size, kind = kind.as_str()))]
    pub async fn charge_aux(
        db: &DatabaseConnection,
        user_id: &str,
        content_hash: &str,
        byte_size: u64,
        kind: BlobKind,
    ) -> Result<ChargeOutcome, QuotaError> {
        let txn = db.begin().await?;
        let outcome = if let Some(existing) = Self::locked_row(&txn, content_hash).await? {
            Self::merge(&txn, existing).await?
        } else {
            Self::insert_row(&txn, user_id, content_hash, byte_size, kind, None).await?;
            ChargeOutcome::Charged { byte_size }
        };
        txn.commit().await?;
        Ok(outcome)
    }

    /// Charge a blob cached from a federated `source_peer` to the **receiving** `user_id`.
    ///
    /// Content-addressed dedup: a blob the server already holds is a [merge](ChargeOutcome::Merged)
    /// (never double-counted, no budget consumed). A genuinely new cache is gated by the
    /// per-`(user_id, source_peer)` caching budget ([`QuotaLimits::per_peer_budget`]) so one
    /// user cannot push the home server's storage past their own quota by pulling from many
    /// peers ([`QuotaError::PeerBudgetExceeded`]).
    #[tracing::instrument(skip(db, limits), fields(user_id = %user_id, content_hash = %content_hash, byte_size, peer = %source_peer))]
    pub async fn charge_federated(
        db: &DatabaseConnection,
        user_id: &str,
        content_hash: &str,
        byte_size: u64,
        kind: BlobKind,
        source_peer: &str,
        limits: &QuotaLimits,
    ) -> Result<ChargeOutcome, QuotaError> {
        let txn = db.begin().await?;
        // Deduped: a blob the server already holds is a merge — no budget consumed.
        let outcome = if let Some(existing) = Self::locked_row(&txn, content_hash).await? {
            Self::merge(&txn, existing).await?
        } else {
            let budget = limits.per_peer_budget();
            let used = Query::used_from_peer(&txn, user_id, source_peer).await?;
            if used.saturating_add(byte_size) > budget {
                txn.rollback().await?;
                tracing::info!(
                    user_id, peer = %source_peer, used, byte_size, budget,
                    "federated cache refused: per-peer caching budget exhausted"
                );
                return Err(QuotaError::PeerBudgetExceeded {
                    peer: source_peer.to_string(),
                    used,
                    additional: byte_size,
                    budget,
                });
            }
            Self::insert_row(
                &txn,
                user_id,
                content_hash,
                byte_size,
                kind,
                Some(source_peer),
            )
            .await?;
            ChargeOutcome::Charged { byte_size }
        };
        txn.commit().await?;
        Ok(outcome)
    }

    /// Release one reference to a ledger blob (an asset hard-purge dropping its derivative /
    /// metadata / provenance reference). Garbage-collects the row and credits the bytes back
    /// when the last reference drops.
    #[tracing::instrument(skip(db), fields(content_hash = %content_hash))]
    pub async fn release_hash(
        db: &DatabaseConnection,
        content_hash: &str,
    ) -> Result<ReleaseOutcome, DbErr> {
        let txn = db.begin().await?;
        let outcome = match Self::locked_row(&txn, content_hash).await? {
            None => ReleaseOutcome::Absent,
            Some(existing) if existing.refcount > 1 => {
                let refcount = existing.refcount - 1;
                let mut am: quota_ledger::ActiveModel = existing.into();
                am.refcount = Set(refcount);
                am.update(&txn).await?;
                ReleaseOutcome::Retained { refcount }
            }
            Some(existing) => {
                let freed_bytes = u64::try_from(existing.byte_size).unwrap_or(0);
                let attributed_user_id = existing.attributed_user_id.clone();
                quota_ledger::Entity::delete_by_id(existing.content_hash.clone())
                    .exec(&txn)
                    .await?;
                tracing::debug!(freed_bytes, %attributed_user_id, "quota ledger blob GC'd");
                ReleaseOutcome::GarbageCollected {
                    freed_bytes,
                    attributed_user_id,
                }
            }
        };
        txn.commit().await?;
        Ok(outcome)
    }

    /// Set (or clear) the moderation suspension flag for `user_id`. Enforcement of suspension
    /// lives in the moderation slice; this is the setter the quota-state report reads.
    #[tracing::instrument(skip(db), fields(user_id = %user_id, suspended))]
    pub async fn set_suspended<C: ConnectionTrait>(
        db: &C,
        user_id: &str,
        suspended: bool,
    ) -> Result<(), DbErr> {
        let stmt = Statement::from_sql_and_values(
            db.get_database_backend(),
            r"INSERT INTO user_quota (user_id, suspended, updated_at)
              VALUES ($1, $2, now())
              ON CONFLICT (user_id) DO UPDATE SET suspended = EXCLUDED.suspended, updated_at = now()",
            [user_id.into(), suspended.into()],
        );
        db.execute(stmt).await?;
        Ok(())
    }

    /// Fetch a ledger row under an exclusive row lock (`SELECT … FOR UPDATE`), serialising
    /// concurrent charges/releases against the same content address.
    async fn locked_row(
        txn: &DatabaseTransaction,
        content_hash: &str,
    ) -> Result<Option<quota_ledger::Model>, DbErr> {
        quota_ledger::Entity::find_by_id(content_hash)
            .lock_exclusive()
            .one(txn)
            .await
    }

    /// Increment an existing row's refcount (a dedup merge).
    async fn merge(
        txn: &DatabaseTransaction,
        existing: quota_ledger::Model,
    ) -> Result<ChargeOutcome, DbErr> {
        let refcount = existing.refcount + 1;
        let mut am: quota_ledger::ActiveModel = existing.into();
        am.refcount = Set(refcount);
        am.update(txn).await?;
        Ok(ChargeOutcome::Merged { refcount })
    }

    /// Insert a fresh ledger row at refcount 1.
    async fn insert_row(
        txn: &DatabaseTransaction,
        user_id: &str,
        content_hash: &str,
        byte_size: u64,
        kind: BlobKind,
        source_peer: Option<&str>,
    ) -> Result<(), DbErr> {
        quota_ledger::ActiveModel {
            content_hash: Set(content_hash.to_string()),
            attributed_user_id: Set(user_id.to_string()),
            byte_size: Set(i64::try_from(byte_size).unwrap_or(i64::MAX)),
            blob_kind: Set(kind.as_str().to_string()),
            source_peer: Set(source_peer.map(str::to_string)),
            refcount: Set(1),
            created_at: Set(entity::time::now_entity()),
        }
        .insert(txn)
        .await?;
        Ok(())
    }
}
