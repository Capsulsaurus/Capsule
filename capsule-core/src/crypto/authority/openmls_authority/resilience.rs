//! **MLS resilience** (slice S-X3): reconciliation over the server-authoritative commit chain, the
//! lost-commit retry primitive, and the group re-keying ceremony.
//!
//! SSoT: [MLS Resilience](https://docs/design/mls-resilience/).
//!
//! OpenMLS resolves ordinary concurrency (one commit wins, the other re-proposes — the S-X2
//! `stage`/`merge`/`discard` primitives). This module owns the recovery contracts the base protocol
//! does *not* resolve on its own: a **lost commit**, **state divergence**, and a **whole-group
//! re-key** after a suspected compromise.
//!
//! **Recovery posture** (from the doc):
//! - *Server chain is authoritative.* Any local inconsistency is reconciled by replaying the
//!   server's chain; the server can order commits but holds no group secrets.
//! - *Re-bootstrap is always available.* A device whose MLS state is unrecoverable is removed and
//!   re-added — losing local MLS state never loses access to the data.
//! - *Quarantine, not silent acceptance.* Divergence is surfaced (an explicit
//!   [`ReconcileOutcome`]), never silently merged.
//!
//! **In-process transport.** S-X3 makes no server changes: the server's authoritative chain is
//! supplied to [`reconcile_with_server`](OpenMlsAuthority::reconcile_with_server) as a
//! [`ServerChainView`] — the classification (behind / ahead / forked) that the real delivery
//! service would compute by comparing commit hashes against its persisted chain.

use openmls::group::CommitMessageBundle;
use openmls::prelude::tls_codec::Deserialize as _;
use openmls::prelude::{LeafNodeParameters, MlsMessageIn, MlsMessageOut};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::super::AlbumAuthority;
use super::{OpenMlsAuthority, OpenMlsAuthorityError, Result, WriteTierIngest};
use crate::crypto::hash::{self, Hash32};
use crate::crypto::keys::HybridSigningKey;

/// The content hash of an MLS commit message — its identity on the server-authoritative chain.
/// Suite-fixed SHA-256 over the commit's wire bytes; two applications of the same commit share a
/// hash (the idempotency identity).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitHash(pub Hash32);

/// The single "bring-me-current" reconciliation outcome (mls-resilience.md § Contract Skeleton).
/// Reconciliation is one entry point, not a per-failure-mode call; this enum reports which path was
/// taken, including the two that escalate to user action or re-bootstrap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// Local state already matches the server head.
    UpToDate,
    /// Local was behind; the missing commits were replayed from the server's chain, in order.
    Reconciled {
        /// The commits applied, oldest first.
        applied_commits: Vec<CommitHash>,
    },
    /// Local diverged from the server in a way that is not a simple replay — surfaced to the user
    /// (quarantine), never silently merged.
    Diverged {
        /// This member's local MLS epoch.
        local_epoch: u64,
        /// The server's head MLS epoch.
        server_epoch: u64,
    },
    /// Local state is unrecoverable (a provable fork, or a commit that will not apply) — the member
    /// must discard its group state and re-bootstrap (be re-added by another device).
    Unrecoverable,
}

/// The server's authoritative commit chain **as seen relative to this member** — the classification
/// the real delivery service computes by comparing the member's head against its persisted chain.
/// This is the mock boundary: S-X3 makes no server changes, so the caller supplies the comparison
/// result rather than this module querying a live server.
pub enum ServerChainView {
    /// The member's head equals the server head.
    UpToDate,
    /// The server is ahead: `missed_commits` are the wire bytes of the commits the member is
    /// missing, oldest first, for replay.
    Behind {
        /// The server's head MLS epoch.
        server_epoch: u64,
        /// The missing commits' wire bytes, oldest first.
        missed_commits: Vec<Vec<u8>>,
    },
    /// The member holds a commit whose hash is **absent** from the server's chain (it is ahead).
    /// `provable_fork` is `true` when the server has advanced past the local commit's parent with a
    /// *different* commit (re-submission can never land) — the discriminator between an honestly
    /// lost commit (retry) and a fork (re-bootstrap).
    LocalAhead {
        /// The server's head MLS epoch.
        server_epoch: u64,
        /// Whether the absence is a provable fork (vs. an honestly-lost commit still retryable).
        provable_fork: bool,
    },
}

/// Why a group re-key was triggered (mls-resilience.md § Group re-keying ceremony).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RekeyReason {
    /// An album admin manually initiated a full re-key.
    AdminInitiated,
    /// Automatic response to a suspected compromise (every leaf rotates; fresh AMK + write-tier).
    SuspectedCompromise,
    /// Optional scheduled rotation for a long-lived album (deployment policy).
    ScheduledRotation,
}

/// The resume state of an in-flight re-keying ceremony (two phases: the re-key commit, then the
/// fresh-epoch broadcast). Persisted, so a crash between the phases resumes on restart — the
/// `intent_id` is the idempotency key it shares with the [upgrade](super::upgrade) ceremony.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RekeyState {
    /// The ceremony's idempotency key.
    pub intent_id: Uuid,
    /// Why the re-key was triggered.
    pub reason: RekeyReason,
}

/// The artifacts a completed re-key produces, for delivery over the (out-of-scope) transport.
pub struct RekeyOutcome {
    /// The ceremony's idempotency key.
    pub intent_id: Uuid,
    /// Why the re-key was triggered.
    pub reason: RekeyReason,
    /// The re-key commit (phase 1), for delivery to the other members.
    pub commit: MlsMessageOut,
    /// The fresh epoch's [`AlbumKeyDistribution`](super::AlbumKeyDistribution) (all members) and
    /// [`WriteTierDistribution`](super::WriteTierDistribution) (writers) — phase 2.
    pub key_delivery: Vec<MlsMessageOut>,
}

/// The lost-commit detection + backoff schedule (mls-resilience.md § Lost commit): a device that
/// doesn't see its committed epoch on the chain within the detection timeout re-submits, backing
/// off on each attempt, and after the budget is exhausted surfaces the change to the user (never
/// silently abandoned). Re-submission is idempotent — OpenMLS rejects a duplicate, so a retry that
/// *did* land is harmless.
///
/// Pure timing bookkeeping (no real sleeping) so it is deterministic under test.
#[derive(Clone, Debug)]
pub struct LostCommitTracker {
    attempts: u32,
    schedule: Vec<jiff::SignedDuration>,
}

impl Default for LostCommitTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl LostCommitTracker {
    /// A tracker with the doc defaults: detection timeout 30 s, backoff 30 s → 2 min → 10 min,
    /// 3 attempts.
    pub fn new() -> Self {
        Self {
            attempts: 0,
            schedule: vec![
                jiff::SignedDuration::from_secs(30),
                jiff::SignedDuration::from_secs(2 * 60),
                jiff::SignedDuration::from_secs(10 * 60),
            ],
        }
    }

    /// The detection timeout: a committed epoch not reflected on the chain within this is treated
    /// as lost.
    pub fn detection_timeout(&self) -> jiff::SignedDuration {
        jiff::SignedDuration::from_secs(30)
    }

    /// The maximum number of re-submission attempts before the change is surfaced to the user.
    pub fn max_attempts(&self) -> u32 {
        u32::try_from(self.schedule.len()).unwrap_or(u32::MAX)
    }

    /// Record a re-submission attempt; returns the backoff to wait before the next attempt, or
    /// `None` if the budget is now exhausted (the change must be surfaced to the user).
    pub fn record_attempt(&mut self) -> Option<jiff::SignedDuration> {
        let backoff = self.schedule.get(self.attempts as usize).copied();
        self.attempts = self.attempts.saturating_add(1);
        backoff
    }

    /// Whether the retry budget is exhausted.
    pub fn is_exhausted(&self) -> bool {
        self.attempts >= self.max_attempts()
    }

    /// How many attempts have been recorded.
    pub fn attempts(&self) -> u32 {
        self.attempts
    }
}

impl OpenMlsAuthority {
    // ── Reconciliation ───────────────────────────────────────────────────────

    /// Bring this member current against the server-authoritative chain, reporting what happened as
    /// a [`ReconcileOutcome`]. The single reconciliation entry point (not a per-failure-mode call):
    ///
    /// - [`UpToDate`](ReconcileOutcome::UpToDate) — nothing to do;
    /// - [`Reconciled`](ReconcileOutcome::Reconciled) — the member was behind and replayed the
    ///   missed commits (the server-authoritative state);
    /// - [`Diverged`](ReconcileOutcome::Diverged) — the member is ahead with an honestly-lost
    ///   commit (surfaced to the user; the [`LostCommitTracker`] drives the actual re-submit),
    ///   never silently merged;
    /// - [`Unrecoverable`](ReconcileOutcome::Unrecoverable) — a provable fork, or a missed commit
    ///   that will not apply (e.g. this member was removed) — re-bootstrap.
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id, local_epoch = self.mls_epoch()))]
    pub fn reconcile_with_server(&mut self, view: ServerChainView) -> Result<ReconcileOutcome> {
        let local_epoch = self.mls_epoch();
        match view {
            ServerChainView::UpToDate => {
                tracing::debug!(album_id = %self.album_id, "reconcile: already up to date");
                Ok(ReconcileOutcome::UpToDate)
            }
            ServerChainView::Behind {
                server_epoch,
                missed_commits,
            } => {
                let mut applied = Vec::with_capacity(missed_commits.len());
                for bytes in missed_commits {
                    let commit_hash = CommitHash(hash::hash_bytes(&bytes));
                    let message =
                        MlsMessageIn::tls_deserialize_exact(bytes.as_slice()).map_err(|e| {
                            OpenMlsAuthorityError::Resilience(format!(
                                "missed commit failed to deserialize: {e:?}"
                            ))
                        })?;
                    match self.process_commit(message) {
                        Ok(_) => applied.push(commit_hash),
                        Err(e) => {
                            // A missed commit that will not apply (evicted, corrupt) is unrecoverable
                            // by replay — the member re-bootstraps.
                            tracing::warn!(album_id = %self.album_id, error = %e, "reconcile: missed commit unapplicable; unrecoverable");
                            return Ok(ReconcileOutcome::Unrecoverable);
                        }
                    }
                }
                tracing::info!(album_id = %self.album_id, server_epoch, applied = applied.len(), "reconcile: replayed missed commits");
                Ok(ReconcileOutcome::Reconciled {
                    applied_commits: applied,
                })
            }
            ServerChainView::LocalAhead {
                server_epoch,
                provable_fork,
            } => {
                if provable_fork {
                    tracing::warn!(album_id = %self.album_id, local_epoch, server_epoch, "reconcile: provable fork; unrecoverable");
                    Ok(ReconcileOutcome::Unrecoverable)
                } else {
                    // Honestly-lost local commit: surface the divergence (the LostCommitTracker
                    // drives the re-submit within its budget). Never silently merged.
                    tracing::warn!(album_id = %self.album_id, local_epoch, server_epoch, "reconcile: local ahead (lost commit); diverged");
                    Ok(ReconcileOutcome::Diverged {
                        local_epoch,
                        server_epoch,
                    })
                }
            }
        }
    }

    // ── Group re-keying ceremony ─────────────────────────────────────────────

    /// The full group re-keying ceremony (mls-resilience.md § Group re-keying ceremony): mint a
    /// fresh epoch (new AMK + write-tier key) for the whole group as one `intent_id`-keyed
    /// operation, and prepare the fresh epoch's broadcast. S-X2's rotation is the primitive; this is
    /// the doc-specified orchestration. Assets are **not** re-encrypted (content-addressed; prior
    /// AMKs are retained for reading history) — the ceremony re-attests the fresh epoch's write-tier
    /// and re-broadcasts the fresh AMK, which is what makes pre-compromise keys useless post-rekey.
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id, ?reason))]
    pub fn rekey_group(&mut self, reason: RekeyReason) -> Result<RekeyOutcome> {
        let (intent_id, commit) = self.begin_rekey(reason)?;
        let key_delivery = self.finish_rekey()?;
        tracing::info!(album_id = %self.album_id, %intent_id, epoch = self.ceiling, "group re-keyed");
        Ok(RekeyOutcome {
            intent_id,
            reason,
            commit,
            key_delivery,
        })
    }

    /// Phase 1 of the re-key: mint the fresh epoch via a self-update commit (fresh epoch secrets ⇒
    /// fresh AMK, fresh minted write-tier), record the resume state, and return the commit for the
    /// other members. The single commit is the atomic cutover — until a member applies it, the album
    /// stays on the prior epoch, so a partial run never leaves two live write-tier keys.
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id, ?reason))]
    pub fn begin_rekey(&mut self, reason: RekeyReason) -> Result<(Uuid, MlsMessageOut)> {
        self.ensure_not_tombstoned()?;
        let intent_id = Uuid::now_v7();
        let minted = HybridSigningKey::generate();
        self.set_write_tier_aad(&minted)?;
        let commit = self
            .group
            .self_update(
                &self.identity.provider,
                &self.identity.mls_signer,
                LeafNodeParameters::default(),
            )
            .map(CommitMessageBundle::into_commit)
            .map_err(|e| OpenMlsAuthorityError::Resilience(format!("re-key commit: {e:?}")))?;
        self.group
            .merge_pending_commit(&self.identity.provider)
            .map_err(|e| OpenMlsAuthorityError::Resilience(format!("re-key merge: {e:?}")))?;
        self.ingest_current_epoch(WriteTierIngest::Minted(minted), true)?;
        self.rekey_pending = Some(RekeyState { intent_id, reason });
        tracing::info!(album_id = %self.album_id, %intent_id, epoch = self.ceiling, "re-key phase 1 committed (fresh AMK + write-tier)");
        Ok((intent_id, commit))
    }

    /// Phase 2 of the re-key: broadcast the fresh epoch's AMK + write-tier private half and clear the
    /// resume state. Idempotent — resuming after the intent already completed is a no-op returning no
    /// messages. The commit is **not** re-produced here (it merged in phase 1), so a resume after a
    /// crash never advances the epoch a second time.
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id))]
    pub fn finish_rekey(&mut self) -> Result<Vec<MlsMessageOut>> {
        let state = self.rekey_pending.take().ok_or_else(|| {
            OpenMlsAuthorityError::Resilience("no re-key in progress to finish".into())
        })?;
        if self.completed_intents.contains(&state.intent_id) {
            return Ok(Vec::new()); // already broadcast — idempotent no-op
        }
        let version = self.epoch_ceiling();
        let key_delivery = vec![
            self.build_key_distribution(version)?,
            self.build_write_tier_distribution(version)?,
        ];
        self.completed_intents.insert(state.intent_id);
        tracing::info!(album_id = %self.album_id, intent_id = %state.intent_id, "re-key phase 2 broadcast");
        Ok(key_delivery)
    }

    /// Resume an interrupted re-key on restart (crash-resume): if a re-key was mid-flight
    /// ([`begin_rekey`](Self::begin_rekey) ran but [`finish_rekey`](Self::finish_rekey) did not),
    /// complete phase 2; otherwise `None`. The `intent_id` guarantees the epoch is never advanced a
    /// second time.
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id))]
    pub fn resume_rekey(&mut self) -> Result<Option<Vec<MlsMessageOut>>> {
        if self.rekey_pending.is_some() {
            tracing::info!(album_id = %self.album_id, "resuming interrupted re-key");
            Ok(Some(self.finish_rekey()?))
        } else {
            Ok(None)
        }
    }

    /// Whether a re-key is mid-flight (its commit merged but its broadcast not yet sent).
    pub fn rekey_in_progress(&self) -> bool {
        self.rekey_pending.is_some()
    }
}
