use std::sync::Arc;

use jiff::SignedDuration;

use super::*;
use crate::federation::capability::Scope;
use crate::federation::store::CapabilityRecord;
use crate::federation::{InMemoryCapabilities, InMemoryPeers};
use crate::store::memory::ManualClock;
use crate::store::{AlbumId, UserId};

struct Harness {
    admin: PeerAdministration,
    capabilities: Arc<InMemoryCapabilities>,
    clock: Arc<ManualClock>,
}

fn harness() -> Harness {
    let clock = Arc::new(ManualClock::default());
    let capabilities = Arc::new(InMemoryCapabilities::new(clock.clone()));
    let admin = PeerAdministration::new(
        Arc::new(InMemoryPeers::new()),
        capabilities.clone(),
        clock.clone(),
    );
    Harness {
        admin,
        capabilities,
        clock,
    }
}

fn peer() -> PeerId {
    PeerId::new("other.test")
}

/// Issue a live grant to `peer`, and return its `jti`.
async fn grant(harness: &Harness, peer: &PeerId, jti: &str) -> String {
    let now = harness.clock.now();
    let expires = crate::store::deadline(now, SignedDuration::from_hours(1));
    harness
        .capabilities
        .issue(CapabilityRecord {
            jti: jti.to_owned(),
            album_id: AlbumId::new("album"),
            peer_id: peer.clone(),
            member: UserId::new("bob"),
            scope: Scope::Read,
            granted_epoch: 1,
            min_protocol_version: "2026-06-01".to_owned(),
            issued_at: now,
            expires_at: expires,
            not_after: expires,
            revoked_at: None,
            refreshed_to: None,
        })
        .await
        .expect("issues");
    jti.to_owned()
}

/// Whether the grant `jti` names has been revoked.
async fn revoked(harness: &Harness, jti: &str) -> bool {
    harness
        .capabilities
        .find(jti)
        .await
        .expect("answers")
        .expect("issued")
        .revoked_at
        .is_some()
}

#[test]
fn a_signing_key_is_read_in_the_spelling_server_info_publishes() {
    let key = [9_u8; 32];
    let published = BASE64.encode(key);
    assert_eq!(parse_signing_key(&published), Ok(key));
    assert_eq!(parse_signing_key(&format!("  {published}\n")), Ok(key));
    for refused in [
        String::new(),
        "not base64!".to_owned(),
        BASE64.encode([9_u8; 31]),
        BASE64.encode([9_u8; 33]),
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0xfb_u8; 32]),
    ] {
        assert_eq!(
            parse_signing_key(&refused),
            Err(InvalidSigningKey),
            "{refused:?}"
        );
    }
}

#[tokio::test]
async fn pinning_reports_a_first_pin_a_rotation_and_a_retry_apart() {
    let harness = harness();
    assert_eq!(
        harness.admin.pin(&peer(), [1; 32]).await.expect("pins"),
        PinOutcome::Pinned
    );
    let first_seen = harness
        .admin
        .read(&peer())
        .await
        .expect("reads")
        .expect("recorded")
        .first_seen_at;

    harness.clock.advance(SignedDuration::from_hours(1));
    assert_eq!(
        harness.admin.pin(&peer(), [1; 32]).await.expect("pins"),
        PinOutcome::Unchanged
    );
    assert_eq!(
        harness.admin.pin(&peer(), [2; 32]).await.expect("pins"),
        PinOutcome::Rotated
    );
    let record = harness
        .admin
        .read(&peer())
        .await
        .expect("reads")
        .expect("recorded");
    assert_eq!(record.signing_key, Some([2; 32]));
    assert_eq!(
        record.first_seen_at, first_seen,
        "a rotation does not restart the peer's probation"
    );
}

#[tokio::test]
async fn blocking_cuts_every_live_grant_the_peer_holds_and_no_other_peers() {
    let harness = harness();
    let first = grant(&harness, &peer(), "01937b7c-0000-7000-8000-0000000000a1").await;
    let second = grant(&harness, &peer(), "01937b7c-0000-7000-8000-0000000000a2").await;
    let other = grant(
        &harness,
        &PeerId::new("third.test"),
        "01937b7c-0000-7000-8000-0000000000a3",
    )
    .await;

    let blocked = harness
        .admin
        .block(&peer(), Some("spam".to_owned()))
        .await
        .expect("blocks");
    assert_eq!(
        blocked,
        Blocked {
            outcome: BlockOutcome::Blocked,
            revoked: 2
        }
    );
    let record = harness
        .admin
        .read(&peer())
        .await
        .expect("reads")
        .expect("recorded");
    assert!(record.is_blocked());
    assert_eq!(record.note.as_deref(), Some("spam"));

    for jti in [&first, &second] {
        assert!(revoked(&harness, jti).await, "{jti} is revoked");
    }
    assert!(
        !revoked(&harness, &other).await,
        "another peer's grant is not this peer's block"
    );

    // A repeat block is not a new fact, and still runs the cascade: a grant the first cascade
    // left live (here, one issued after it, standing in for a cascade that failed part-way)
    // is revoked by the second, which is how an operator finishes publication.
    let straggler = grant(&harness, &peer(), "01937b7c-0000-7000-8000-0000000000a4").await;
    assert_eq!(
        harness.admin.block(&peer(), None).await.expect("blocks"),
        Blocked {
            outcome: BlockOutcome::AlreadyBlocked,
            revoked: 1
        }
    );
    assert!(
        revoked(&harness, &straggler).await,
        "the straggler is revoked"
    );
    assert_eq!(
        harness.admin.block(&peer(), None).await.expect("blocks"),
        Blocked {
            outcome: BlockOutcome::AlreadyBlocked,
            revoked: 0
        }
    );
}

#[tokio::test]
async fn unblocking_keeps_the_key_and_restores_no_grant() {
    let harness = harness();
    harness.admin.pin(&peer(), [1; 32]).await.expect("pins");
    let jti = grant(&harness, &peer(), "01937b7c-0000-7000-8000-0000000000b1").await;
    harness.admin.block(&peer(), None).await.expect("blocks");

    assert_eq!(
        harness.admin.unblock(&peer()).await.expect("unblocks"),
        UnblockOutcome::Unblocked
    );
    assert_eq!(
        harness.admin.unblock(&peer()).await.expect("unblocks"),
        UnblockOutcome::NotBlocked
    );
    let record = harness
        .admin
        .read(&peer())
        .await
        .expect("reads")
        .expect("recorded");
    assert!(!record.is_blocked());
    assert_eq!(record.signing_key, Some([1; 32]));
    assert!(
        revoked(&harness, &jti).await,
        "the block's revocation outlives the block"
    );
}

#[tokio::test]
async fn an_unknown_peer_reads_as_nothing() {
    assert_eq!(harness().admin.read(&peer()).await.expect("reads"), None);
}
