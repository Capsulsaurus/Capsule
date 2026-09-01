//! The album port and the authority it grounds.

use std::sync::Arc;

use super::authority::ProvisionedAuthority;
use super::*;
use crate::directory::{DeviceDirectoryStore, InMemoryDeviceDirectory, PublishedDirectory};
use crate::store::UserId;
use crate::upload::{AlbumWriteAccess, WriteAuthority};

/// The account every case provisions under.
fn owner() -> OwnerId {
    OwnerId::new("01937b7c-0000-7000-8000-000000000001")
}

/// A derived album id.
fn album() -> AlbumId {
    AlbumId::new("0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e35")
}

/// A record for `who`.
fn record(who: &OwnerId) -> AlbumRecord {
    AlbumRecord {
        album_id: album(),
        owner_id: who.clone(),
        protocol_version: "2026-01-01".to_owned(),
        upgrade: None,
        created_at: Timestamp::UNIX_EPOCH,
    }
}

#[tokio::test]
async fn provisioning_is_idempotent_for_its_owner() {
    let albums = InMemoryAlbums::new();

    assert!(matches!(
        albums.provision(record(&owner())).await.expect("provision"),
        ProvisionOutcome::Created(_)
    ));
    assert!(
        matches!(
            albums.provision(record(&owner())).await.expect("provision"),
            ProvisionOutcome::AlreadyProvisioned(_)
        ),
        "the same id arrives from every device the user owns and again after a recovery, so \
         re-provisioning must be a success that writes nothing"
    );
}

#[tokio::test]
async fn an_id_bound_elsewhere_is_refused_without_saying_why() {
    let albums = InMemoryAlbums::new();
    albums
        .provision(record(&OwnerId::new("somebody-else")))
        .await
        .expect("provision");

    assert_eq!(
        albums.provision(record(&owner())).await.expect("provision"),
        ProvisionOutcome::NotAvailable,
        "a derived album id is unguessable before creation, so a refusal that distinguished \
         'taken' from any other reason would be an existence oracle over other accounts' ids"
    );
}

#[test]
fn only_a_canonical_uuid_is_an_album_id() {
    assert!(is_canonical_album_id(
        "0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e35"
    ));
    for spelling in [
        "0198F3C2-9C4A-7B3D-8F21-4D7C9A1B2E35",
        "{0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e35}",
        "urn:uuid:0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e35",
        "0198f3c29c4a7b3d8f214d7c9a1b2e35",
        "not-a-uuid",
    ] {
        assert!(
            !is_canonical_album_id(spelling),
            "{spelling:?} round-trips to a different string, so two devices deriving the same \
             album would disagree about its name"
        );
    }
}

// -------------------------------------------------------------------------------------------
// The authority
// -------------------------------------------------------------------------------------------

/// A directory for `user` carrying `device`, added at `added_at` and revoked at `revoked_at`.
fn directory(
    user: &UserId,
    device: uuid::Uuid,
    added_at: &str,
    revoked_at: Option<&str>,
) -> Vec<u8> {
    use capsule_core::crypto::keys::hybrid_sig::HybridSigningKey;
    use capsule_core::crypto::keys::{DeviceDirectory, DeviceEntry, DirectoryCore};

    let signing = HybridSigningKey::generate();
    let entry = DeviceEntry {
        device_id: device,
        dsk_public: signing.verifying_key(),
        dek_public: None,
        added_at: added_at.to_owned(),
        revoked_at: revoked_at.map(str::to_owned),
    };
    let directory: DeviceDirectory = DirectoryCore {
        user_id: uuid::Uuid::parse_str(user.as_str()).expect("a uuid"),
        directory_version: 1,
        updated_at: "2026-01-01T00:00:00Z".to_owned(),
        devices: vec![entry],
    }
    .sign(&signing);
    capsule_core::cbor::to_canonical_vec(&directory).expect("a directory serializes")
}

/// The authority over two freshly seeded stores.
fn authority(
    albums: Arc<InMemoryAlbums>,
    directories: Arc<InMemoryDeviceDirectory>,
) -> ProvisionedAuthority {
    ProvisionedAuthority::new(
        albums,
        directories,
        std::sync::Arc::new(crate::store::SystemClock),
    )
}

#[tokio::test]
async fn an_album_is_writable_by_the_account_it_was_provisioned_to() {
    let albums = Arc::new(InMemoryAlbums::new());
    albums.provision(record(&owner())).await.expect("provision");
    let authority = authority(albums, Arc::new(InMemoryDeviceDirectory::new()));

    assert_eq!(
        authority
            .album_write_access(&owner(), &album())
            .await
            .expect("the authority answers"),
        AlbumWriteAccess::Writable {
            quiescing_under: None,
            protocol_pin: "2026-01-01".to_owned()
        },
        "the pin is the album's own, which is what invariant 6 compares a write against"
    );
    assert_eq!(
        authority
            .album_write_access(&OwnerId::new("somebody-else"), &album())
            .await
            .expect("the authority answers"),
        AlbumWriteAccess::Denied,
    );
    assert_eq!(
        authority
            .album_write_access(
                &owner(),
                &AlbumId::new("0198f3c2-0000-7b3d-8f21-4d7c9a1b2e35")
            )
            .await
            .expect("the authority answers"),
        AlbumWriteAccess::Denied,
        "an unprovisioned album and somebody else's are one answer",
    );
}

#[tokio::test]
async fn the_invariant_seven_floor_is_the_devices_own_added_at() {
    let user = UserId::new("01937b7c-0000-7000-8000-000000000001");
    let device = uuid::Uuid::parse_str("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f").expect("a uuid");
    let directories = Arc::new(InMemoryDeviceDirectory::new());
    directories
        .publish(PublishedDirectory {
            user_id: user.clone(),
            directory_version: 1,
            document: directory(&user, device, "2026-03-04T05:06:07Z", None),
            // The authority reads a *stored* directory, which the publish path has already
            // anchored and verified (`S-C42`); these cases seed the store directly, so the
            // anchor is only along for the ride.
            identity_key: Vec::new(),
            published_at: Timestamp::UNIX_EPOCH,
        })
        .await
        .expect("publish");
    let authority = authority(Arc::new(InMemoryAlbums::new()), directories);

    assert_eq!(
        authority
            .device_added_at(&user, device)
            .await
            .expect("the authority answers"),
        Some(
            "2026-03-04T05:06:07Z"
                .parse::<Timestamp>()
                .expect("an instant")
        ),
        "the floor is this device's own admission, not the account's creation time — which is \
         what makes invariant 7 mean what it says"
    );

    let unknown = uuid::Uuid::parse_str("018f3f1e-0000-7c9d-8e2f-1a2b3c4d5e6f").expect("a uuid");
    assert_eq!(
        authority
            .device_added_at(&user, unknown)
            .await
            .expect("the authority answers"),
        None,
    );
}

#[tokio::test]
async fn a_revoked_device_may_not_sign_new_manifests() {
    let user = UserId::new("01937b7c-0000-7000-8000-000000000001");
    let device = uuid::Uuid::parse_str("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f").expect("a uuid");
    let directories = Arc::new(InMemoryDeviceDirectory::new());
    directories
        .publish(PublishedDirectory {
            user_id: user.clone(),
            directory_version: 1,
            document: directory(
                &user,
                device,
                "2026-03-04T05:06:07Z",
                Some("2026-04-01T00:00:00Z"),
            ),
            identity_key: Vec::new(),
            published_at: Timestamp::UNIX_EPOCH,
        })
        .await
        .expect("publish");

    assert_eq!(
        authority(Arc::new(InMemoryAlbums::new()), directories)
            .device_added_at(&user, device)
            .await
            .expect("the authority answers"),
        None,
        "the entry is retained so manifests signed before revocation stay verifiable, but the \
         device may not sign new ones"
    );
}

#[tokio::test]
async fn an_account_with_no_published_directory_has_no_floor() {
    let user = UserId::new("01937b7c-0000-7000-8000-000000000001");
    let device = uuid::Uuid::parse_str("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f").expect("a uuid");

    assert_eq!(
        authority(
            Arc::new(InMemoryAlbums::new()),
            Arc::new(InMemoryDeviceDirectory::new()),
        )
        .device_added_at(&user, device)
        .await
        .expect("the authority answers"),
        None,
        "the retired fallback to account-creation time made invariant 7 vacuous for exactly \
         the accounts most likely to be wrong about their devices"
    );
}
