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
        foreseen: None,
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
async fn only_the_owner_reads_a_backup() {
    // A backup is the owner's durability artefact, not part of what the album shares, so no
    // membership covers it — and the refusal is a stranger's, not a `403`, for everyone.
    let authority = authority().await;
    let mut backup = reference("alice");
    backup.role = BlobRole::Backup;
    assert_eq!(
        decide(&authority, "alice", &backup).await,
        BlobReadAccess::Granted,
        "the owner reads their own backup"
    );
    for caller in ["bob", "carol", "dave", "erin", "mallory"] {
        assert_eq!(
            decide(&authority, caller, &backup).await,
            BlobReadAccess::Unrelated,
            "{caller} is not the owner, whatever their membership"
        );
    }
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
