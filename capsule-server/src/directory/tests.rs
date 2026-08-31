//! The device-directory port's own suite.
//!
//! Everything here is about the two things the server is allowed to do with a signed
//! document — compare one projected field, and hand the bytes back unchanged.

use capsule_core::crypto::keys::hybrid_sig::HybridSigningKey;
use capsule_core::crypto::keys::{DeviceDirectory, DirectoryCore};
use uuid::Uuid;

use super::*;

/// The account every case here publishes under.
fn account() -> (Uuid, UserId) {
    let id = Uuid::parse_str("01937b7c-0000-7000-8000-000000000001").expect("a uuid");
    (id, UserId::new(id.to_string()))
}

/// A signed directory for `user` at `version`, and the bytes it travels as.
fn signed(user: Uuid, version: u64) -> Vec<u8> {
    let ik = HybridSigningKey::generate();
    let core = DirectoryCore {
        user_id: user,
        directory_version: version,
        updated_at: "2026-01-01T00:00:00Z".to_owned(),
        devices: Vec::new(),
    };
    let directory: DeviceDirectory = core.sign(&ik);
    capsule_core::cbor::to_canonical_vec(&directory).expect("a directory serializes")
}

/// A published record for `user` at `version`.
fn record(user: &UserId, version: u64, document: Vec<u8>) -> PublishedDirectory {
    PublishedDirectory {
        user_id: user.clone(),
        directory_version: version,
        document,
        published_at: Timestamp::UNIX_EPOCH,
    }
}

#[tokio::test]
async fn a_first_publish_is_accepted_and_served_back_verbatim() {
    let (uuid, user) = account();
    let store = InMemoryDeviceDirectory::new();
    let document = signed(uuid, 1);

    let outcome = store
        .publish(record(&user, 1, document.clone()))
        .await
        .expect("the store accepts");
    assert_eq!(
        outcome,
        PublishOutcome::Published {
            directory_version: 1
        }
    );

    let fetched = store
        .fetch(&user)
        .await
        .expect("the store answers")
        .expect("a published directory");
    assert_eq!(
        fetched.document, document,
        "the bytes the client signed are the bytes the server serves; re-encoding one would \
         detach it from its signature and look like the client's bug"
    );
}

#[tokio::test]
async fn a_version_that_does_not_advance_is_refused() {
    let (uuid, user) = account();
    let store = InMemoryDeviceDirectory::new();
    store
        .publish(record(&user, 7, signed(uuid, 7)))
        .await
        .expect("the store accepts");

    // Invariant 23 is *strictly* greater, so equal is a refusal too — a re-published version
    // could carry different devices under the same number, which is the rollback in disguise.
    for version in [7_u64, 6, 1] {
        let outcome = store
            .publish(record(&user, version, signed(uuid, version)))
            .await
            .expect("the store answers");
        assert_eq!(
            outcome,
            PublishOutcome::Stale { stored: 7 },
            "version {version} was accepted over a stored 7"
        );
    }

    let fetched = store
        .fetch(&user)
        .await
        .expect("the store answers")
        .expect("a published directory");
    assert_eq!(
        fetched.directory_version, 7,
        "a refused publish must leave the stored document untouched"
    );
}

#[tokio::test]
async fn a_directory_is_scoped_to_its_account() {
    let (uuid, user) = account();
    let store = InMemoryDeviceDirectory::new();
    store
        .publish(record(&user, 3, signed(uuid, 3)))
        .await
        .expect("the store accepts");

    assert_eq!(
        store
            .fetch(&UserId::new("01937b7c-0000-7000-8000-0000000000ff"))
            .await
            .expect("the store answers"),
        None,
        "one account's directory is not another's"
    );
}

#[test]
fn the_projected_version_comes_from_the_signed_core() {
    let (uuid, user) = account();
    let document = signed(uuid, 42);
    assert_eq!(
        project_version(&document, &user).expect("a well-formed directory"),
        42
    );
}

#[test]
fn a_document_signed_for_another_account_is_refused() {
    let (uuid, _) = account();
    let document = signed(uuid, 1);
    let somebody_else = UserId::new("01937b7c-0000-7000-8000-0000000000ff");

    assert!(
        matches!(
            project_version(&document, &somebody_else),
            Err(MalformedDirectory::WrongAccount)
        ),
        "the account is taken from the signed core, so a caller cannot publish somebody \
         else's signed document under their own name"
    );
}

#[test]
fn bytes_that_are_not_a_directory_are_refused_before_anything_is_stored() {
    let (_, user) = account();
    assert!(matches!(
        project_version(b"not cbor at all", &user),
        Err(MalformedDirectory::Undecodable(_))
    ));
}

#[test]
fn an_implausibly_large_document_is_refused_without_decoding_it() {
    let (_, user) = account();
    let huge = vec![0_u8; MAX_DIRECTORY_BYTES + 1];
    assert!(
        matches!(
            project_version(&huge, &user),
            Err(MalformedDirectory::TooLarge { size }) if size == MAX_DIRECTORY_BYTES + 1
        ),
        "the size check runs first, so a decoder is never handed an unbounded document"
    );
}
