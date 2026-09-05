//! **E2E case 12** — cross-device enrollment, the server leg through the SDK.
//!
//! Device A authorizes new device B over a verified channel: fresh local auth → an enrollment
//! code → B redeems it into a relay channel → payloads cross in both directions, each
//! delivered once and never to the wrong mailbox → the initiator closes the channel. Includes
//! one MITM-on-relay abort: the initiator sees a payload that is not the key material it
//! expected and closes, and the enrollee's next drain finds no channel.
//!
//! The client ceremony — B's hardware keys, the safety-code check, A cross-signing B into the
//! directory, B's MLS joins — is blocked on seams the tree does not have and is filed by the
//! pull request that landed this crate; `libraries match` waits on server-side membership.

use capsule_e2e::{Device, PASSWORD, PROTOCOL_VERSION, Server};
use capsule_sdk::rest;
use capsule_sdk::rest::types::{ReauthenticateRequest, RedeemRequest, RelayRequest};

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
