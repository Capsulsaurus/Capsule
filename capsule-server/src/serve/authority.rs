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
//! | [`BlobReadAccess::ScopeInsufficient`] | `403` | *"this grant does not cover originals"* — a peer only (`S-E5`) |
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
//! # And where a peer's fact comes from (`S-E5`)
//!
//! A federated peer is not an account, so it is not asked the account's question. Its
//! relationship to an asset is the **capability** this server minted: one album, one roster
//! member, one epoch, one scope. So [`ReadPrincipal::Peer`] is decided as — is this blob in the
//! album the capability names (anything else is a stranger's, `404`), is that member still on
//! the roster at the epoch the grant was made at (removed, or re-admitted later, is `403`
//! [`BlobReadAccess::Revoked`]: the peer held the grant, so the change is a disclosure it is
//! owed), was that member ever on it at all (`404`), and finally does the grant's scope cover
//! this blob's **role** — a `read-derivative-only` capability is refused an `original` with
//! [`BlobReadAccess::ScopeInsufficient`]. A **backup** is refused under every scope and is
//! refused as [`BlobReadAccess::Unrelated`] rather than as a scope failure: it is the owner's own
//! durability artefact rather than part of what was shared, the feed never names one, and the
//! `403`'s justification — the peer already knows the asset is there — does not hold for a blob
//! it was never told about.
//!
//! Whether the grant is still *live* — unrevoked, unexpired — is not asked here: it has no
//! clock, and the route admits the capability through
//! [`federation::admit`](crate::federation::admit) before it resolves anything. What is asked
//! here is only what the stores know.
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

use crate::federation::VerifiedCapability;
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
    /// The caller is entitled to the album, but its grant does not cover this blob's role
    /// (`S-E5`).
    ///
    /// Only a peer under a capability ever sees this: an account's membership carries no scope.
    /// Rendered as `403 error.federation.scope_insufficient` rather than `404`, because the peer
    /// already knows the album holds the asset — the feed told it — and a `404` would send it
    /// looking for an address that is there.
    ScopeInsufficient,
}

/// Who a blob is being served to (`S-C39`, `S-E5`).
///
/// An account and a peer are decided from the same stores, but they are not the same reader:
/// an account's relationship to an asset is its own membership, while a peer's is the
/// membership of the roster member its capability was minted for, inside the one album that
/// capability names. A bare identifier would have let either be read as the other, and a peer
/// origin and an account id can spell the same string.
#[derive(Debug, Clone, Copy)]
pub enum ReadPrincipal<'a> {
    /// An account, through a session access token.
    Account(&'a OwnerId),
    /// A peer server, through a federation capability (`S-E5`).
    Peer(&'a VerifiedCapability),
}

impl<'a> ReadPrincipal<'a> {
    /// The account whose own in-flight uploads may answer a fetch (`S-C40`), or `None`.
    ///
    /// A peer has none. The transient `409` reports the caller's *own device* still sending
    /// exactly these bytes; a peer has no device here, so it is told what an unreferenced
    /// address tells everyone and waits for the feed's `original_held` to flip instead.
    #[must_use]
    pub fn own_account(self) -> Option<&'a OwnerId> {
        match self {
            Self::Account(owner) => Some(owner),
            Self::Peer(_) => None,
        }
    }
}

impl fmt::Display for ReadPrincipal<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Account(owner) => write!(f, "account {owner}"),
            Self::Peer(capability) => write!(f, "peer {}", capability.record.peer_id),
        }
    }
}

/// Who may read a blob.
///
/// A port rather than a function on the serve context, for the reason
/// [`WriteAuthority`](crate::upload::WriteAuthority) is one: the facts it decides from live in
/// stores that will grow (federation next), and a serving path that reached into them directly
/// would have to grow with them.
pub trait ReadAuthority: fmt::Debug + Send + Sync {
    /// May `principal` fetch the bytes `reference` names?
    ///
    /// Takes the whole reference rather than an asset id so the decision comes from the same
    /// read that found it. An authority that re-looked-up the asset would open a window in
    /// which the two reads disagree, and would cost a round trip to do it.
    fn blob_read_access<'a>(
        &'a self,
        principal: ReadPrincipal<'a>,
        reference: &'a BlobReference,
    ) -> ReadAuthorityFuture<'a, BlobReadAccess>;
}

/// The authority the server runs on: an account reads its own assets' blobs and the blobs of
/// every album it is currently a member of, and a peer reads what its capability names.
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

impl MembershipAuthority {
    /// What an account may read: its own assets, and the albums it is on the roster of.
    async fn account_access(
        &self,
        caller: &OwnerId,
        reference: &BlobReference,
    ) -> Result<BlobReadAccess, ReadAuthorityError> {
        {
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
        }
    }

    /// What a peer may read: the album its capability names, as the member it was minted for,
    /// within the scope it was granted (`S-E5`).
    async fn peer_access(
        &self,
        capability: &VerifiedCapability,
        reference: &BlobReference,
    ) -> Result<BlobReadAccess, ReadAuthorityError> {
        let record = &capability.record;
        // A capability covers exactly one album. A blob in any other is answered as a
        // stranger's: the peer holds no fact about that album and must not acquire one here,
        // and `404` is byte-identical to an address nothing references.
        if reference.album_id != record.album_id {
            tracing::info!(
                peer = %record.peer_id,
                asset = %reference.asset_id,
                "a peer named an address outside its capability's album"
            );
            return Ok(BlobReadAccess::Unrelated);
        }
        let membership = self
            .members
            .membership(&reference.album_id, &record.member)
            .await
            .map_err(|error| {
                tracing::error!(%error, album = %reference.album_id, "the membership store could not answer a peer's fetch");
                ReadAuthorityError::unavailable(error.to_string())
            })?;
        match membership {
            Membership::Member { granted_epoch, .. } if granted_epoch == record.granted_epoch => {
                // Entitled to the album. The last question is the grant's own: a scope is
                // enforced against the blob's server-visible **role**, so a derivative-only
                // capability cannot fetch an original whatever the peer says it is fetching.
                if record.scope.permits(reference.role) {
                    return Ok(BlobReadAccess::Granted);
                }
                // A **backup** is not part of what was shared at all — it is the owner's own
                // durability artefact — so no capability over the album covers it and a peer has
                // no relationship to it to be told about. `404`, as a stranger gets, and *not*
                // the `403` below: that answer's whole justification is that the feed already
                // told the peer the asset is there, which is true of an original under a
                // derivative-only grant and false of a backup, which the feed never names.
                if reference.role == crate::store::BlobRole::Backup {
                    tracing::info!(
                        peer = %record.peer_id,
                        asset = %reference.asset_id,
                        "a peer named a backup, which no capability covers"
                    );
                    return Ok(BlobReadAccess::Unrelated);
                }
                tracing::info!(
                    peer = %record.peer_id,
                    asset = %reference.asset_id,
                    role = reference.role.as_str(),
                    scope = record.scope.as_str(),
                    "a peer's capability does not cover this blob's role"
                );
                Ok(BlobReadAccess::ScopeInsufficient)
            }
            // The member was never on this roster at all. Not the peer's business that the
            // album exists, so it is told what a stranger is told.
            Membership::Never => {
                tracing::info!(
                    peer = %record.peer_id,
                    member = %record.member,
                    album = %reference.album_id,
                    "a peer's capability names a member the roster never carried"
                );
                Ok(BlobReadAccess::Unrelated)
            }
            // Removed, or re-admitted at a later epoch: either way the membership this grant
            // was minted for has ended. The peer held it, so the change is a disclosure it is
            // owed — the same `403` a former member gets.
            membership => {
                tracing::info!(
                    peer = %record.peer_id,
                    member = %record.member,
                    album = %reference.album_id,
                    ?membership,
                    granted_epoch = record.granted_epoch,
                    "a peer's capability outlived the membership it was minted for"
                );
                Ok(BlobReadAccess::Revoked)
            }
        }
    }
}

impl ReadAuthority for MembershipAuthority {
    fn blob_read_access<'a>(
        &'a self,
        principal: ReadPrincipal<'a>,
        reference: &'a BlobReference,
    ) -> ReadAuthorityFuture<'a, BlobReadAccess> {
        Box::pin(async move {
            match principal {
                ReadPrincipal::Account(caller) => self.account_access(caller, reference).await,
                ReadPrincipal::Peer(capability) => self.peer_access(capability, reference).await,
            }
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
    use crate::federation::{CapabilityRecord, PeerId, Scope};
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

    /// An authority over a store where `bob` is a reader and `carol` a writer — both since
    /// version 1, so their membership was granted at epoch 1 — `dave` a former member removed
    /// at version 2, and `erin` a member removed at version 2 and **re-admitted** at version 3,
    /// whose membership was therefore granted at epoch 3.
    ///
    /// The re-admission is what a peer capability's epoch binding is tested against: the
    /// membership store keeps a member's original `granted_epoch` while they stay listed, so
    /// only an interruption moves it.
    async fn authority() -> MembershipAuthority {
        let store = Arc::new(InMemoryMembership::new());
        roster(
            &store,
            1,
            &[
                ("bob", MemberRole::Reader),
                ("carol", MemberRole::Writer),
                ("dave", MemberRole::Writer),
                ("erin", MemberRole::Reader),
            ],
        )
        .await;
        roster(
            &store,
            2,
            &[("bob", MemberRole::Reader), ("carol", MemberRole::Writer)],
        )
        .await;
        roster(
            &store,
            3,
            &[
                ("bob", MemberRole::Reader),
                ("carol", MemberRole::Writer),
                ("erin", MemberRole::Reader),
            ],
        )
        .await;
        MembershipAuthority::new(store)
    }

    async fn decide(
        authority: &MembershipAuthority,
        caller: &str,
        reference: &BlobReference,
    ) -> BlobReadAccess {
        let owner = OwnerId::new(caller);
        authority
            .blob_read_access(ReadPrincipal::Account(&owner), reference)
            .await
            .expect("the authority decides")
    }

    /// A capability over the shared album, minted for `member` at `granted_epoch` with `scope`.
    ///
    /// Built as the store holds one rather than through the codec: what this unit decides from
    /// is the *record*, and the token behind it is `federation::capability`'s subject.
    fn capability(member: &str, granted_epoch: u64, scope: Scope) -> VerifiedCapability {
        let record = CapabilityRecord {
            jti: "01937b7c-0000-7000-8000-0000000000aa".to_owned(),
            album_id: AlbumId::new("album"),
            peer_id: PeerId::new("other.test"),
            member: UserId::new(member),
            scope,
            granted_epoch,
            min_protocol_version: "2026-06-01".to_owned(),
            issued_at: jiff::Timestamp::UNIX_EPOCH,
            expires_at: jiff::Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_hours(6),
            not_after: jiff::Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_hours(6),
            revoked_at: None,
            refreshed_to: None,
        };
        VerifiedCapability {
            grant: record.grant(),
            record,
        }
    }

    async fn decide_peer(
        authority: &MembershipAuthority,
        capability: &VerifiedCapability,
        reference: &BlobReference,
    ) -> BlobReadAccess {
        authority
            .blob_read_access(ReadPrincipal::Peer(capability), reference)
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
    async fn a_peer_reads_the_album_its_capability_names_as_the_member_it_was_minted_for() {
        // Bob is on the roster at epoch 2, which is what the grant is bound to.
        let authority = authority().await;
        assert_eq!(
            decide_peer(
                &authority,
                &capability("bob", 1, Scope::Read),
                &reference("alice")
            )
            .await,
            BlobReadAccess::Granted
        );
        assert_eq!(
            decide_peer(
                &authority,
                &capability("erin", 3, Scope::Read),
                &reference("alice")
            )
            .await,
            BlobReadAccess::Granted,
            "the epoch a re-admission was granted at"
        );
    }

    #[tokio::test]
    async fn a_peer_outside_its_capabilitys_album_is_a_stranger() {
        // Not `Revoked`: the peer has no relationship to another album, and a `403` would tell
        // it the address is referenced by somebody.
        let authority = authority().await;
        let mut elsewhere = reference("alice");
        elsewhere.album_id = AlbumId::new("another-album");
        assert_eq!(
            decide_peer(&authority, &capability("bob", 1, Scope::Read), &elsewhere).await,
            BlobReadAccess::Unrelated
        );
        // And a member the roster never carried is a stranger inside the album too.
        assert_eq!(
            decide_peer(
                &authority,
                &capability("mallory", 1, Scope::Read),
                &reference("alice")
            )
            .await,
            BlobReadAccess::Unrelated
        );
    }

    #[tokio::test]
    async fn a_peers_grant_does_not_outlive_the_membership_it_was_minted_for() {
        // Dave was removed at version 2 and never came back. Erin was removed at 2 and
        // re-admitted at 3, so a grant naming epoch 1 covers a membership that ended even
        // though she is on the roster right now. Both are the `403` a former member gets,
        // because the peer held the grant and the change is a disclosure it is owed.
        let authority = authority().await;
        assert_eq!(
            decide_peer(
                &authority,
                &capability("dave", 1, Scope::Read),
                &reference("alice")
            )
            .await,
            BlobReadAccess::Revoked
        );
        assert_eq!(
            decide_peer(
                &authority,
                &capability("erin", 1, Scope::Read),
                &reference("alice")
            )
            .await,
            BlobReadAccess::Revoked,
            "re-admission at a later epoch does not revive an older grant"
        );
    }

    #[tokio::test]
    async fn a_derivative_only_grant_is_refused_an_original_and_every_grant_a_backup() {
        // The scope is enforced against the blob's server-visible role, never against what the
        // peer says it is fetching.
        let authority = authority().await;
        let mut original = reference("alice");
        original.role = BlobRole::Original;
        assert_eq!(
            decide_peer(
                &authority,
                &capability("bob", 1, Scope::ReadDerivativeOnly),
                &original
            )
            .await,
            BlobReadAccess::ScopeInsufficient
        );
        assert_eq!(
            decide_peer(&authority, &capability("bob", 1, Scope::Read), &original).await,
            BlobReadAccess::Granted
        );

        for role in [
            BlobRole::Derivative,
            BlobRole::Metadata,
            BlobRole::Provenance,
        ] {
            let mut derived = reference("alice");
            derived.role = role;
            assert_eq!(
                decide_peer(
                    &authority,
                    &capability("bob", 1, Scope::ReadDerivativeOnly),
                    &derived
                )
                .await,
                BlobReadAccess::Granted,
                "{role:?} is what a derivative-only grant is for"
            );
        }

        // A backup is refused as a *stranger's* blob, not as a scope failure: the feed never
        // names one, so the peer holds no fact about it and the `403`'s premise does not apply.
        let mut backup = reference("alice");
        backup.role = BlobRole::Backup;
        for scope in [Scope::Read, Scope::ReadDerivativeOnly] {
            assert_eq!(
                decide_peer(&authority, &capability("bob", 1, scope), &backup).await,
                BlobReadAccess::Unrelated,
                "a backup is the owner's durability artefact, not part of what was shared"
            );
        }
    }

    /// A peer's refusal does not vary with the asset's state either.
    #[tokio::test]
    async fn a_peers_answer_does_not_vary_with_the_assets_state() {
        let authority = authority().await;
        for state in [AssetState::Visible, AssetState::Tombstoned] {
            let mut reference = reference("alice");
            reference.state = state;
            reference.hold = Some(crate::index::ServingHold::Takedown);
            assert_eq!(
                decide_peer(
                    &authority,
                    &capability("mallory", 1, Scope::Read),
                    &reference
                )
                .await,
                BlobReadAccess::Unrelated
            );
            assert_eq!(
                decide_peer(&authority, &capability("dave", 1, Scope::Read), &reference).await,
                BlobReadAccess::Revoked
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
