//! **E2E case 12** — cross-device enrollment: the server leg and the client ceremony, through
//! the SDK.
//!
//! Device A authorizes new device B over a verified channel: fresh local auth → an enrollment
//! code → B redeems it into a relay channel → payloads cross in both directions, each
//! delivered once and never to the wrong mailbox → the initiator closes the channel. Includes
//! one MITM-on-relay abort: the initiator sees a payload that is not the key material it
//! expected and closes, and the enrollee's next drain finds no channel.
//!
//! The client ceremony rides the same relay, every message an [`EnrollmentPayload`] in its wire
//! form: B commits to its keys → A reveals its keys → B reveals the keys it committed to and A
//! checks the commitment → both derive the safety code over what they received → A cross-signs
//! B into the directory, publishes it, and relays it → B verifies it under A's advertised user
//! identity key. Its MITM variant swaps B's keys on the relay: the codes diverge and A aborts
//! before it signs anything.
//!
//! Out of reach and not exercised here: the master-key transfer under a channel key (no
//! ephemeral key agreement is defined yet), and B's MLS joins and `libraries match`, which wait
//! on server-side membership (#405).

use capsule_core::crypto::keys::{DeviceDirectory, SAFETY_CODE_DIGITS};
use capsule_core::lifecycle::{EnrollmentPayload, Workspace, enrollment_safety_code};
use capsule_e2e::{Device, FAST_KDF, PASSPHRASE, PASSWORD, PROTOCOL_VERSION, Server};
use capsule_sdk::rest;
use capsule_sdk::rest::types::{ReauthenticateRequest, RedeemRequest, RelayRequest};
use tempfile::TempDir;

const TO_ENROLLEE: &str = "to_enrollee";
const TO_INITIATOR: &str = "to_initiator";

/// The enrollee's client: no account yet, but it is a Capsule build and speaks the handshake.
fn enrollee(server: &Server) -> rest::Client {
    rest::Client::with_client(
        capsule_sdk::net::http_client().expect("the SDK client builds"),
        server.base_url(),
    )
    .expect("the API root parses")
}

async fn open_channel(server: &Server, a: &Device) -> String {
    let issued = a
        .generated(server)
        .issue_enrollment_code(PROTOCOL_VERSION, None)
        .await
        .expect("a freshly authenticated initiator issues a code")
        .into_inner();
    assert!(!issued.code.is_empty());
    assert!(!issued.text_fallback.is_empty());
    enrollee(server)
        .redeem_enrollment_code(PROTOCOL_VERSION, None, &RedeemRequest { code: issued.code })
        .await
        .expect("the enrollee redeems the code")
        .into_inner()
        .channel_id
}

fn relay(direction: &str, payload: &str) -> RelayRequest {
    RelayRequest {
        direction: direction.to_owned(),
        payload: payload.to_owned(),
    }
}

/// The new device's own library: a fresh one on its own root, holding nothing of A's account —
/// B has received no master key, and the ceremony needs only the device keys it generated.
fn new_device_library() -> (TempDir, Workspace) {
    let root = tempfile::tempdir().expect("a temp library root");
    let workspace = Workspace::create_with_params(root.path(), PASSPHRASE, FAST_KDF)
        .expect("the new device's library");
    (root, workspace)
}

/// Relay one ceremony payload in its wire form.
async fn send(client: &rest::Client, channel: &str, direction: &str, payload: &EnrollmentPayload) {
    let encoded = payload.encode().expect("a ceremony payload encodes");
    client
        .relay_enrollment_payload(channel, PROTOCOL_VERSION, None, &relay(direction, &encoded))
        .await
        .expect("the relay accepts a ceremony payload");
}

/// Drain `direction` and decode the one payload it must hold.
async fn receive(client: &rest::Client, channel: &str, direction: &str) -> EnrollmentPayload {
    let drained = client
        .drain_enrollment_channel(channel, direction, PROTOCOL_VERSION, None)
        .await
        .expect("the mailbox drains")
        .into_inner();
    let [payload] = drained.payloads.as_slice() else {
        panic!(
            "{direction} held {} payloads, expected exactly one",
            drained.payloads.len()
        );
    };
    EnrollmentPayload::decode(payload).expect("a relayed payload decodes")
}

/// Whether `code` has the displayed shape: the fixed digit count, in space-separated groups.
fn is_safety_code(code: &str) -> bool {
    code.chars().filter(char::is_ascii_digit).count() == SAFETY_CODE_DIGITS
        && code.chars().all(|c| c.is_ascii_digit() || c == ' ')
}

/// A drain of a closed channel is refused as not found.
async fn assert_channel_gone(client: &rest::Client, channel: &str, direction: &str) {
    let gone = client
        .drain_enrollment_channel(channel, direction, PROTOCOL_VERSION, None)
        .await
        .expect_err("a closed channel delivers nothing");
    assert!(
        matches!(
            gone,
            rest::Error::Api(ref response)
                if matches!(response.inner(), rest::DrainEnrollmentChannelError::Status404(_))
        ),
        "got {gone:?}"
    );
}

#[tokio::test]
async fn e2e_case_12_a_code_opens_a_relay_channel_that_delivers_each_payload_once() {
    let server = Server::boot().await;
    let a = Device::register(&server, "initiator").await;
    let initiator = a.generated(&server);
    let b = enrollee(&server);

    // Fresh local auth on the initiator: the password, on the already-authenticated session.
    let fresh = initiator
        .reauthenticate(
            PROTOCOL_VERSION,
            None,
            &ReauthenticateRequest {
                password: PASSWORD.to_owned(),
            },
        )
        .await
        .expect("the password re-authenticates the session")
        .into_inner();
    assert!(!fresh.authenticated_at.is_empty());

    let channel = open_channel(&server, &a).await;

    // A → B, then B → A; each mailbox holds only its own direction.
    initiator
        .relay_enrollment_payload(
            &channel,
            PROTOCOL_VERSION,
            None,
            &relay(TO_ENROLLEE, "wrapped-album-keys"),
        )
        .await
        .expect("the initiator relays");
    b.relay_enrollment_payload(
        &channel,
        PROTOCOL_VERSION,
        None,
        &relay(TO_INITIATOR, "device-b-public-keys"),
    )
    .await
    .expect("the enrollee relays");
    let to_b = b
        .drain_enrollment_channel(&channel, TO_ENROLLEE, PROTOCOL_VERSION, None)
        .await
        .expect("the enrollee drains")
        .into_inner();
    assert_eq!(to_b.payloads, vec!["wrapped-album-keys"]);
    let to_a = initiator
        .drain_enrollment_channel(&channel, TO_INITIATOR, PROTOCOL_VERSION, None)
        .await
        .expect("the initiator drains")
        .into_inner();
    assert_eq!(to_a.payloads, vec!["device-b-public-keys"]);

    // Delivered once: both mailboxes are now empty.
    for direction in [TO_ENROLLEE, TO_INITIATOR] {
        let again = b
            .drain_enrollment_channel(&channel, direction, PROTOCOL_VERSION, None)
            .await
            .expect("a drained mailbox still answers")
            .into_inner();
        assert!(again.payloads.is_empty(), "{direction} delivered twice");
    }

    // The initiator closes; the channel is gone for the enrollee.
    initiator
        .close_enrollment_channel(&channel, PROTOCOL_VERSION, None)
        .await
        .expect("the initiator closes its channel");
    let closed = b
        .drain_enrollment_channel(&channel, TO_ENROLLEE, PROTOCOL_VERSION, None)
        .await
        .expect_err("a closed channel is not found");
    match closed {
        rest::Error::Api(response) => match response.into_inner() {
            rest::DrainEnrollmentChannelError::Status404(problem) => {
                assert_eq!(problem.code, "error.enrollment.channel_not_found");
            }
            other => panic!("expected 404, got {other:?}"),
        },
        other => panic!("expected an API refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn e2e_case_12_the_initiator_aborts_on_a_payload_it_did_not_expect() {
    let server = Server::boot().await;
    let a = Device::register(&server, "initiator").await;
    let initiator = a.generated(&server);
    let b = enrollee(&server);
    // Registration is fresh local auth: the code issues without a separate reauthentication.
    let channel = open_channel(&server, &a).await;

    // What B advertised out of band (the safety code the users compare) versus what arrives.
    const ADVERTISED: &str = "device-b-public-keys";
    b.relay_enrollment_payload(
        &channel,
        PROTOCOL_VERSION,
        None,
        &relay(TO_INITIATOR, "device-m-public-keys"),
    )
    .await
    .expect("the relay accepts what it is given");
    let arrived = initiator
        .drain_enrollment_channel(&channel, TO_INITIATOR, PROTOCOL_VERSION, None)
        .await
        .expect("the initiator drains")
        .into_inner();
    assert_ne!(
        arrived.payloads,
        vec![ADVERTISED],
        "the relay was tampered with"
    );

    // Abort: close, and never send the wrapped keys.
    initiator
        .close_enrollment_channel(&channel, PROTOCOL_VERSION, None)
        .await
        .expect("the initiator aborts by closing");
    let aborted = b
        .drain_enrollment_channel(&channel, TO_ENROLLEE, PROTOCOL_VERSION, None)
        .await
        .expect_err("nothing reaches the enrollee after the abort");
    assert!(
        matches!(
            aborted,
            rest::Error::Api(ref response)
                if matches!(response.inner(), rest::DrainEnrollmentChannelError::Status404(_))
        ),
        "got {aborted:?}"
    );
    let relayed_late = b
        .relay_enrollment_payload(
            &channel,
            PROTOCOL_VERSION,
            None,
            &relay(TO_INITIATOR, ADVERTISED),
        )
        .await;
    assert!(relayed_late.is_err(), "a closed channel accepts nothing");
}

#[tokio::test]
async fn e2e_case_12_the_client_ceremony_admits_the_new_device_over_the_relay() {
    let server = Server::boot().await;
    let mut a = Device::register(&server, "initiator").await;
    let initiator = a.generated(&server);
    let b = enrollee(&server);
    let (_b_root, b_library) = new_device_library();
    let channel = open_channel(&server, &a).await;
    let before = a.directory().core.directory_version;

    // 1. B commits to the keys it will reveal, before it has seen anything of A's.
    let b_keys = b_library.enrollment_enrollee_keys();
    send(
        &b,
        &channel,
        TO_INITIATOR,
        &EnrollmentPayload::Commitment(b_keys.commitment()),
    )
    .await;
    let EnrollmentPayload::Commitment(commitment) =
        receive(&initiator, &channel, TO_INITIATOR).await
    else {
        panic!("the initiator expected the enrollee's commitment first");
    };

    // 2. A reveals its keys, under a nonce drawn for this ceremony.
    let a_keys = a.workspace.enrollment_initiator_keys();
    assert_eq!(a_keys.user_id, a.user_id);
    send(
        &initiator,
        &channel,
        TO_ENROLLEE,
        &EnrollmentPayload::InitiatorKeys(a_keys.clone()),
    )
    .await;
    let EnrollmentPayload::InitiatorKeys(a_seen_by_b) = receive(&b, &channel, TO_ENROLLEE).await
    else {
        panic!("the enrollee expected the initiator's keys");
    };

    // 3. B reveals what it committed to; A opens the commitment.
    send(
        &b,
        &channel,
        TO_INITIATOR,
        &EnrollmentPayload::EnrolleeKeys(b_keys.clone()),
    )
    .await;
    let EnrollmentPayload::EnrolleeKeys(b_seen_by_a) =
        receive(&initiator, &channel, TO_INITIATOR).await
    else {
        panic!("the initiator expected the enrollee's keys");
    };
    assert!(
        b_seen_by_a.matches_commitment(&commitment),
        "the revealed keys open the commitment"
    );

    // 4. Each side derives the code over what it holds; the users compare them.
    let code_on_a = enrollment_safety_code(&a_keys, &b_seen_by_a);
    let code_on_b = enrollment_safety_code(&a_seen_by_b, &b_keys);
    assert!(is_safety_code(&code_on_a), "displayed shape: {code_on_a}");
    assert_eq!(code_on_a, code_on_b, "an untampered relay shows one code");

    // 5. A cross-signs B, dated by A's clock, and publishes the directory that admits it.
    let added_at = jiff::Timestamp::now().to_string();
    let directory = a
        .workspace
        .cross_sign_device(b_seen_by_a.into_device_entry(added_at))
        .expect("the matched device cross-signs into the directory");
    assert_eq!(directory.core.directory_version, before + 1);
    assert_eq!(a.publish_directory(&server).await, before + 1);
    let on_server = a
        .directory_client(&server)
        .fetch(a.user_id, &a.workspace.user_ik_public())
        .await
        .expect("the published directory fetches and verifies");
    assert_eq!(on_server, directory);

    // 6. A relays it; B verifies it under the identity key A advertised and finds itself.
    send(
        &initiator,
        &channel,
        TO_ENROLLEE,
        &EnrollmentPayload::Directory(directory),
    )
    .await;
    let EnrollmentPayload::Directory(received) = receive(&b, &channel, TO_ENROLLEE).await else {
        panic!("the enrollee expected the cross-signed directory");
    };
    assert!(received.verify(&a_seen_by_b.user_ik_public));
    assert_eq!(received.core.user_id, a_seen_by_b.user_id);
    let admitted = received
        .device(&b_library.device_id())
        .expect("the enrollee is in the directory it received");
    assert_eq!(admitted.dsk_public, b_keys.dsk_public);
    assert_eq!(
        admitted
            .dek_public
            .as_ref()
            .map(|dek| dek.as_bytes().to_vec()),
        Some(b_library.device_dek_public())
    );
    assert_eq!(admitted.revoked_at, None);
    assert!(received.device(&a.workspace.device_id()).is_some());

    initiator
        .close_enrollment_channel(&channel, PROTOCOL_VERSION, None)
        .await
        .expect("the initiator closes the finished channel");
    assert_channel_gone(&b, &channel, TO_ENROLLEE).await;
}

#[tokio::test]
async fn e2e_case_12_a_relay_that_swaps_the_enrollee_keys_diverges_the_codes_and_a_aborts() {
    let server = Server::boot().await;
    let a = Device::register(&server, "initiator").await;
    let initiator = a.generated(&server);
    let b = enrollee(&server);
    // Whoever holds the channel id can post to it: the man in the middle needs no account.
    let mitm = enrollee(&server);
    let (_b_root, b_library) = new_device_library();
    let (_m_root, mallory) = new_device_library();
    let channel = open_channel(&server, &a).await;
    let before: DeviceDirectory = a.directory();

    // The relay carries M's commitment toward A in place of B's, and later M's keys in place of
    // B's: a consistent swap, so the commitment alone cannot catch it.
    let b_keys = b_library.enrollment_enrollee_keys();
    let m_keys = mallory.enrollment_enrollee_keys();
    send(
        &mitm,
        &channel,
        TO_INITIATOR,
        &EnrollmentPayload::Commitment(m_keys.commitment()),
    )
    .await;
    let EnrollmentPayload::Commitment(commitment) =
        receive(&initiator, &channel, TO_INITIATOR).await
    else {
        panic!("the initiator expected a commitment first");
    };

    let a_keys = a.workspace.enrollment_initiator_keys();
    send(
        &initiator,
        &channel,
        TO_ENROLLEE,
        &EnrollmentPayload::InitiatorKeys(a_keys.clone()),
    )
    .await;
    let EnrollmentPayload::InitiatorKeys(a_seen_by_b) = receive(&b, &channel, TO_ENROLLEE).await
    else {
        panic!("the enrollee expected the initiator's keys");
    };

    send(
        &mitm,
        &channel,
        TO_INITIATOR,
        &EnrollmentPayload::EnrolleeKeys(m_keys),
    )
    .await;
    let EnrollmentPayload::EnrolleeKeys(seen_by_a) =
        receive(&initiator, &channel, TO_INITIATOR).await
    else {
        panic!("the initiator expected the enrollee's keys");
    };
    assert!(
        seen_by_a.matches_commitment(&commitment),
        "a consistent swap opens its own commitment"
    );
    assert!(
        !seen_by_a.matches_commitment(&b_keys.commitment()),
        "but not the commitment B made"
    );

    // The users compare the codes and they differ.
    let code_on_a = enrollment_safety_code(&a_keys, &seen_by_a);
    let code_on_b = enrollment_safety_code(&a_seen_by_b, &b_keys);
    assert_ne!(code_on_a, code_on_b, "the swap must show on the codes");

    // A aborts: it signs nothing, publishes nothing, and closes the channel.
    initiator
        .close_enrollment_channel(&channel, PROTOCOL_VERSION, None)
        .await
        .expect("the initiator aborts by closing");
    assert_eq!(a.directory(), before, "nothing was cross-signed");
    let on_server = a
        .directory_client(&server)
        .fetch(a.user_id, &a.workspace.user_ik_public())
        .await
        .expect("the published directory still fetches");
    assert_eq!(on_server, before, "nothing was published");
    assert!(on_server.device(&mallory.device_id()).is_none());
    assert_channel_gone(&b, &channel, TO_ENROLLEE).await;
}
