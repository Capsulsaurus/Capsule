//! [`ReadAuthority`] — who may fetch a blob, and the `403` the contract asks for (`S-C39`,
//! `S-C51`).
//!
//! # The hole `S-C39` closed
//!
//! Before it, `GET /v1/blob/{hash}` authorized on "a valid access token" and nothing else, on
//! both the Salvo surface and its Kynos port. **Any authenticated account could fetch any live
//! ciphertext whose address it could name.** That was defended as a capability model — a content
//! address is the hash of ciphertext, so producing one without holding the bytes is producing a
//! preimage — and the defence is not wrong, but it is not what the contract describes and the
//! difference is invisible until it matters. It also stacks badly: an address leaks through a
//! backup, a log, a screenshot of a debug tool, and the capability is permanent because the
//! address is.
//!
//! So the serve path asks a question, and the question has a port.
//!
//! # Three answers, and the middle one is the whole disclosure argument
//!
//! | Answer | Status | What it tells the caller |
//! | --- | --- | --- |
//! | [`BlobReadAccess::Granted`] | `200`/`206` | the bytes |
//! | [`BlobReadAccess::Revoked`] | `403` | *"you had this and you do not now"* — re-sync membership, then degrade |
//! | [`BlobReadAccess::Unrelated`] | `404` | nothing. Byte-identical to an address the server never heard of |
//!
//! **A `403` is a disclosure and a `404` is not**, which is why the boundary is drawn where it
//! is. Answering `403` to a caller with no relationship to an asset would confirm that the
//! address is referenced by *somebody* — an existence oracle over content addresses, handed out
//! to anyone who can name one. [Download & Sync] describes the `403` as the signal for an
//! authorization *change*, and a change presupposes a prior state: the caller has to be someone
//! the server can see once had access. Everyone else is told what an unknown address is told.
//!
//! # Where the middle row's fact comes from (`S-C51`)
//!
//! [`MembershipAuthority`] grants a fetch to the account the referencing asset is filed under
//! and to any account on the current roster of the album it belongs to, in either role — a
//! reader reads, that is what the role is for. An account the roster once carried and no longer
//! does is [`BlobReadAccess::Revoked`]: the membership store keeps the row and marks it, rather
//! than deleting it, precisely so this answer has a stored fact behind it. An account the roster
//! never named is [`BlobReadAccess::Unrelated`], indistinguishable from a stranger, because it
//! is one.
//!
//! The roster itself is the album owner's signed statement, verified against the owner's
//! published device directory before it is stored ([`crate::membership`]). This server still
//! cannot read the MLS group, and the roster does not change that: it is a **transport**
//! control over who is handed bytes, not a confidentiality control over who can read them.
//!
//! The membership question is asked from the reference the index returned, which carries the
//! asset's `album_id` and `owner_id` for exactly this reason: the decision comes from the same
//! read that found the reference, so there is no window in which ownership and the answer
//! disagree. It costs one membership lookup per fetch by a non-owner and none for the owner.
//!
//! **The takedown `410` was deliberately left alone** by `S-C39`, and the reasoning still holds
//! with members in the picture: design/moderation.md states the per-surface rule as *"takedown
//! of known content → `410`"*, and changing a landed, tested contract on an inference is not
//! this module's to do. What `S-C51` adds is that a *former* member is answered `403` before any
//! `410` is reached, so the authority-first ordering `S-C39` established keeps every policy
//! refusal illegible to anyone who is not currently entitled to the bytes.
//!
//! [Download & Sync]: ../../../capsule-docs/src/content/docs/design/import/download-sync.md

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::index::BlobReference;
use crate::membership::{Membership, MembershipStore};
use crate::store::{OwnerId, UserId};

/// The future a read-authority question returns.
///
/// Boxed for the same reason every store port's is: the authority is held as
/// `Arc<dyn ReadAuthority>` so the serving module is not generic over who decides.
pub type ReadAuthorityFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ReadAuthorityError>> + Send + 'a>>;

/// A collaborator could not answer, so nothing was decided.
///
/// Deliberately not a refusal: an authority that answered "denied" when it could not reach its
/// store would make an outage indistinguishable from a revocation, and the client actions for
/// those are opposite — retry versus re-sync and degrade.
#[derive(Debug, thiserror::Error)]
#[error("the read authority could not decide: {detail}")]
pub struct ReadAuthorityError {
    /// What went wrong, for the log line.
    detail: String,
}

impl ReadAuthorityError {
    /// A collaborator could not answer.
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

/// Whether a caller may fetch the bytes behind a reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobReadAccess {
    /// Serve them.
    Granted,
    /// The caller was on the album's roster and has been removed (`S-C51`).
    ///
    /// Rendered as `403`: the one answer that discloses the address is live, given only to an
    /// account the server holds a revoked membership row for.
    Revoked,
    /// The caller has no relationship to the asset the server can see.
    ///
    /// Rendered as `404`, byte-identical to an address nothing references — which is the point.
    Unrelated,
}

/// Who may read a blob.
///
/// A port rather than a function on the serve context, for the reason
/// [`WriteAuthority`](crate::upload::WriteAuthority) is one: the facts it decides from live in
/// stores that will grow (federation next), and a serving path that reached into them directly
/// would have to grow with them.
pub trait ReadAuthority: fmt::Debug + Send + Sync {
    /// May `caller` fetch the bytes `reference` names?
    ///
    /// Takes the whole reference rather than an asset id so the decision comes from the same
    /// read that found it. An authority that re-looked-up the asset would open a window in
    /// which the two reads disagree, and would cost a round trip to do it.
    fn blob_read_access<'a>(
        &'a self,
        caller: &'a OwnerId,
        reference: &'a BlobReference,
    ) -> ReadAuthorityFuture<'a, BlobReadAccess>;
}

/// The authority the server runs on: an account reads its own assets' blobs and the blobs of
/// every album it is currently a member of.
#[derive(Debug, Clone)]
pub struct MembershipAuthority {
    members: Arc<dyn MembershipStore>,
}

impl MembershipAuthority {
    /// The authority over `members`.
    #[must_use]
    pub fn new(members: Arc<dyn MembershipStore>) -> Self {
        Self { members }
    }
}

impl ReadAuthority for MembershipAuthority {
    fn blob_read_access<'a>(
        &'a self,
        caller: &'a OwnerId,
        reference: &'a BlobReference,
    ) -> ReadAuthorityFuture<'a, BlobReadAccess> {
        Box::pin(async move {
            if &reference.owner_id == caller {
                return Ok(BlobReadAccess::Granted);
            }
            // Somebody else's asset: the album's roster decides. The store is asked with the
            // caller's account id, which is the same string the owner id is.
            let user = UserId::new(caller.as_str());
            let membership = self
                .members
                .membership(&reference.album_id, &user)
                .await
                .map_err(|error| {
                    tracing::error!(%error, album = %reference.album_id, "the membership store could not answer a fetch");
                    ReadAuthorityError::unavailable(error.to_string())
                })?;
            Ok(match membership {
                // Either role reads: that is what a reader is.
                Membership::Member { .. } => BlobReadAccess::Granted,
                Membership::Revoked(revocation) => {
                    tracing::info!(
                        asset = %reference.asset_id,
                        album = %reference.album_id,
                        at_version = revocation.at_version,
                        "a former member's blob fetch was refused"
                    );
                    BlobReadAccess::Revoked
                }
                // Never a member. Not `Revoked`: the caller never had it, and saying otherwise
                // would confirm the address is live — see the module docs on the boundary.
                Membership::Never => {
                    tracing::info!(
                        asset = %reference.asset_id,
                        "a blob fetch named an address belonging to another account"
                    );
                    BlobReadAccess::Unrelated
                }
            })
        })
    }
}

/// A convenience for wiring the production authority.
#[must_use]
pub fn membership_reads(members: Arc<dyn MembershipStore>) -> Arc<dyn ReadAuthority> {
    Arc::new(MembershipAuthority::new(members))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::AssetState;
    use crate::membership::{InMemoryMembership, MemberRole, RosterRecord};
    use crate::store::{AlbumId, AssetId, BlobRole};

    /// A reference to `owner`'s asset in the one album these cases share.
    fn reference(owner: &str) -> BlobReference {
        BlobReference {
            asset_id: AssetId::new("asset"),
            album_id: AlbumId::new("album"),
            owner_id: OwnerId::new(owner),
            role: BlobRole::Original,
            state: AssetState::Visible,
            original_held: true,
            hold: None,
        }
    }

    /// The album's roster at `version`, naming `members`.
    async fn roster(store: &InMemoryMembership, version: u64, members: &[(&str, MemberRole)]) {
        store
            .apply_roster(
                RosterRecord {
                    album_id: AlbumId::new("album"),
                    roster_version: version,
                    amk_epoch: version,
                    attested_by_device: uuid::Uuid::from_u128(0xD1),
                    received_at: jiff::Timestamp::UNIX_EPOCH,
                    document: format!("v{version}").into_bytes(),
                },
                members
                    .iter()
                    .map(|(user, role)| (UserId::new(*user), *role))
                    .collect(),
            )
            .await
            .expect("the store applies");
    }

    /// An authority over a store where `bob` is a reader, `carol` a writer and `dave` a former
    /// member of alice's album.
    async fn authority() -> MembershipAuthority {
        let store = Arc::new(InMemoryMembership::new());
        roster(
            &store,
            1,
            &[
                ("bob", MemberRole::Reader),
                ("carol", MemberRole::Writer),
                ("dave", MemberRole::Writer),
            ],
        )
        .await;
        roster(
            &store,
            2,
            &[("bob", MemberRole::Reader), ("carol", MemberRole::Writer)],
        )
        .await;
        MembershipAuthority::new(store)
    }

    async fn decide(
        authority: &MembershipAuthority,
        caller: &str,
        reference: &BlobReference,
    ) -> BlobReadAccess {
        authority
            .blob_read_access(&OwnerId::new(caller), reference)
            .await
            .expect("the authority decides")
    }

    #[tokio::test]
    async fn an_account_reads_its_own_without_asking_the_roster() {
        // No roster at all: the owner's access is the album record's fact, not the roster's.
        let authority = MembershipAuthority::new(Arc::new(InMemoryMembership::new()));
        assert_eq!(
            decide(&authority, "alice", &reference("alice")).await,
            BlobReadAccess::Granted
        );
    }

    #[tokio::test]
    async fn a_member_of_either_role_reads() {
        let authority = authority().await;
        assert_eq!(
            decide(&authority, "bob", &reference("alice")).await,
            BlobReadAccess::Granted,
            "a reader reads; that is what the role is for"
        );
        assert_eq!(
            decide(&authority, "carol", &reference("alice")).await,
            BlobReadAccess::Granted
        );
    }

    #[tokio::test]
    async fn a_former_member_is_revoked_and_a_stranger_is_unrelated() {
        // The disclosure boundary, at the unit that decides it. `Revoked` becomes the `403` the
        // contract describes; `Unrelated` becomes a `404` identical to an unknown address.
        let authority = authority().await;
        assert_eq!(
            decide(&authority, "dave", &reference("alice")).await,
            BlobReadAccess::Revoked
        );
        assert_eq!(
            decide(&authority, "mallory", &reference("alice")).await,
            BlobReadAccess::Unrelated
        );
    }

    /// State the caller cannot see does not change the answer.
    ///
    /// A tombstoned or held asset of somebody else's is `Unrelated` to a stranger and `Revoked`
    /// to a former member exactly as a live one is — the authority decides on membership alone,
    /// so no lifecycle fact leaks through it. The serving path relies on this by asking it
    /// **first**.
    #[tokio::test]
    async fn a_non_members_answer_does_not_vary_with_the_assets_state() {
        let authority = authority().await;
        for state in [AssetState::Visible, AssetState::Tombstoned] {
            let mut reference = reference("alice");
            reference.state = state;
            reference.hold = Some(crate::index::ServingHold::Takedown);
            assert_eq!(
                decide(&authority, "mallory", &reference).await,
                BlobReadAccess::Unrelated,
                "a stranger's refusal must not vary with facts about the owner's asset"
            );
            assert_eq!(
                decide(&authority, "dave", &reference).await,
                BlobReadAccess::Revoked,
                "nor a former member's"
            );
        }
    }

    #[tokio::test]
    async fn membership_is_asked_about_the_references_own_album() {
        // The roster is per album: a member of *this* album is a stranger to another one.
        let authority = authority().await;
        let mut elsewhere = reference("alice");
        elsewhere.album_id = AlbumId::new("another-album");
        assert_eq!(
            decide(&authority, "bob", &elsewhere).await,
            BlobReadAccess::Unrelated
        );
    }
}
