//! The device-directory port's own suite.
//!
//! Everything here is about the three things the server is allowed to do with a signed
//! document — check it verifies under the account's identity anchor, compare one projected
//! field, and hand the bytes back unchanged.

use capsule_core::crypto::keys::hybrid_sig::HybridSigningKey;
use capsule_core::crypto::keys::{DeviceDirectory, DirectoryCore};
use uuid::Uuid;

use super::*;

/// The account every case here publishes under.
fn account() -> (Uuid, UserId) {
    let id = Uuid::parse_str("01937b7c-0000-7000-8000-000000000001").expect("a uuid");
    (id, UserId::new(id.to_string()))
}

/// The identity key a case anchors its account to.
fn ik() -> HybridSigningKey {
    HybridSigningKey::generate()
}

/// The anchor bytes for `ik`, as the record and the header carry them.
fn anchor(ik: &HybridSigningKey) -> Vec<u8> {
    ik.verifying_key().to_bytes()
}

/// A signed directory for `user` at `version`, and the bytes it travels as.
fn signed_by(ik: &HybridSigningKey, user: Uuid, version: u64) -> Vec<u8> {
    let core = DirectoryCore {
        user_id: user,
        directory_version: version,
        updated_at: "2026-01-01T00:00:00Z".to_owned(),
        devices: Vec::new(),
    };
    let directory: DeviceDirectory = core.sign(ik);
    capsule_core::cbor::to_canonical_vec(&directory).expect("a directory serializes")
}

/// A published record for `user` at `version`, anchored to `ik`.
fn record(
    user: &UserId,
    version: u64,
    document: Vec<u8>,
    ik: &HybridSigningKey,
) -> PublishedDirectory {
    PublishedDirectory {
        user_id: user.clone(),
        directory_version: version,
        document,
        identity_key: anchor(ik),
        published_at: Timestamp::UNIX_EPOCH,
    }
}

#[tokio::test]
async fn a_first_publish_is_accepted_and_served_back_verbatim() {
    let (uuid, user) = account();
    let store = InMemoryDeviceDirectory::new();
    let ik = ik();
    let document = signed_by(&ik, uuid, 1);

    let outcome = store
        .publish(record(&user, 1, document.clone(), &ik))
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
    let ik = ik();
    store
        .publish(record(&user, 7, signed_by(&ik, uuid, 7), &ik))
        .await
        .expect("the store accepts");

    // Invariant 23 is *strictly* greater, so equal is a refusal too — a re-published version
    // could carry different devices under the same number, which is the rollback in disguise.
    for version in [7_u64, 6, 1] {
        let outcome = store
            .publish(record(&user, version, signed_by(&ik, uuid, version), &ik))
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
    let ik = ik();
    store
        .publish(record(&user, 3, signed_by(&ik, uuid, 3), &ik))
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
    let ik = ik();
    let document = signed_by(&ik, uuid, 42);
    assert_eq!(
        project_version(&document, &user, &anchor(&ik)).expect("a well-formed directory"),
        42
    );
}

#[test]
fn a_document_signed_for_another_account_is_refused() {
    let (uuid, _) = account();
    let ik = ik();
    let document = signed_by(&ik, uuid, 1);
    let somebody_else = UserId::new("01937b7c-0000-7000-8000-0000000000ff");

    assert!(
        matches!(
            project_version(&document, &somebody_else, &anchor(&ik)),
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
        project_version(b"not cbor at all", &user, &anchor(&ik())),
        Err(MalformedDirectory::Undecodable(_))
    ));
}

#[test]
fn an_implausibly_large_document_is_refused_without_decoding_it() {
    let (_, user) = account();
    let huge = vec![0_u8; MAX_DIRECTORY_BYTES + 1];
    assert!(
        matches!(
            project_version(&huge, &user, &anchor(&ik())),
            Err(MalformedDirectory::TooLarge { size }) if size == MAX_DIRECTORY_BYTES + 1
        ),
        "the size check runs first, so a decoder is never handed an unbounded document"
    );
}

#[test]
fn a_document_that_does_not_verify_under_the_submitted_key_is_refused() {
    // Invariant 23's second clause (`S-C42`). Before this, an authenticated caller could
    // publish a document that verifies under no key at all, and the server would serve it to
    // every peer that asked — and, worse, `S-C23`'s revoke-all anchors on that document, so a
    // stolen session token could permanently disable the account's global sign-out.
    let (uuid, user) = account();
    let mine = ik();
    let theirs = ik();
    let document = signed_by(&theirs, uuid, 1);

    assert!(
        matches!(
            project_version(&document, &user, &anchor(&mine)),
            Err(MalformedDirectory::SignatureInvalid)
        ),
        "a document signed by one key must not verify under another"
    );
    assert_eq!(
        project_version(&document, &user, &anchor(&theirs)).expect("its own key verifies it"),
        1
    );
}

#[test]
fn an_unreadable_identity_key_is_its_own_refusal() {
    // Distinct from `SignatureInvalid` because the client's remedy differs: one is a malformed
    // header, the other is the wrong key.
    let (uuid, user) = account();
    let document = signed_by(&ik(), uuid, 1);
    assert!(matches!(
        project_version(&document, &user, b"not a key"),
        Err(MalformedDirectory::UnreadableIdentityKey(_))
    ));
}

#[tokio::test]
async fn the_first_publish_anchors_the_account_and_later_keys_are_refused() {
    // Trust-on-first-publish. The anchor is immutable, so a caller holding a valid session but
    // not the account's identity key cannot replace the directory — which is the property
    // `S-C23`'s revoke-all rests on.
    let (uuid, user) = account();
    let store = InMemoryDeviceDirectory::new();
    let anchored = ik();
    let attacker = ik();

    store
        .publish(record(&user, 1, signed_by(&anchored, uuid, 1), &anchored))
        .await
        .expect("the store accepts");

    assert_eq!(
        store
            .publish(record(&user, 2, signed_by(&attacker, uuid, 2), &attacker))
            .await
            .expect("the store answers"),
        PublishOutcome::IdentityMismatch,
        "a second key must not be able to take over an anchored account"
    );

    let fetched = store
        .fetch(&user)
        .await
        .expect("the store answers")
        .expect("a published directory");
    assert_eq!(
        fetched.directory_version, 1,
        "a refused publish leaves the anchored document in force"
    );

    assert_eq!(
        store
            .publish(record(&user, 2, signed_by(&anchored, uuid, 2), &anchored))
            .await
            .expect("the store answers"),
        PublishOutcome::Published {
            directory_version: 2
        },
        "the anchored key still publishes"
    );
}

#[tokio::test]
async fn a_wrong_key_is_refused_as_a_mismatch_even_when_its_version_is_stale() {
    // The anchor is checked before the version, so a caller is told which check they failed
    // only once — and it is the one that is true about them. Answering `Stale` here would let
    // somebody probe the stored version with a document they never had the key for.
    let (uuid, user) = account();
    let store = InMemoryDeviceDirectory::new();
    let anchored = ik();
    let attacker = ik();
    store
        .publish(record(&user, 9, signed_by(&anchored, uuid, 9), &anchored))
        .await
        .expect("the store accepts");

    assert_eq!(
        store
            .publish(record(&user, 1, signed_by(&attacker, uuid, 1), &attacker))
            .await
            .expect("the store answers"),
        PublishOutcome::IdentityMismatch
    );
}

#[test]
fn a_mismatch_refusal_discloses_nothing_about_the_anchor() {
    // Structural — the variant has no payload — and this is what says so as a test.
    let rendered = format!("{:?}", PublishOutcome::IdentityMismatch);
    assert_eq!(rendered, "IdentityMismatch");
}
