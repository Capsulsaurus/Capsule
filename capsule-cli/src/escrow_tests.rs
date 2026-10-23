use capsule_library::lifecycle::LifecycleError;
use capsule_sdk::recovery::{MintedSecret, RecoveryError};

use super::{describe_escrow_error, typed_back_matches};
use crate::i18n::{error_codes, error_message};
use crate::remote::RemoteError;

/// The hex the user was shown passes, in either case and with the newline a terminal adds.
#[test]
fn the_type_back_gate_accepts_the_shown_hex() {
    let secret = MintedSecret::generate();
    let hex = secret.to_hex();
    assert!(typed_back_matches(&secret, &hex));
    assert!(typed_back_matches(
        &secret,
        &format!("  {}\n", hex.to_uppercase())
    ));
}

/// Another secret, a truncated copy, and text that is not hex all fail the gate.
#[test]
fn the_type_back_gate_refuses_anything_else() {
    let secret = MintedSecret::generate();
    let hex = secret.to_hex();
    assert!(!typed_back_matches(
        &secret,
        &MintedSecret::generate().to_hex()
    ));
    assert!(!typed_back_matches(&secret, &hex[..62]));
    assert!(!typed_back_matches(&secret, ""));
    assert!(!typed_back_matches(&secret, &"z".repeat(64)));
}

/// Each refusal the command names renders its own line, and the rebind refusal tells the
/// user to use a fresh library (#624 decision).
#[test]
fn each_named_refusal_has_its_own_line() {
    let already = describe_escrow_error(&RemoteError::EscrowAlreadyStored);
    let mismatch = describe_escrow_error(&RemoteError::RecoverySecretNotConfirmed);
    let rebind = describe_escrow_error(&RemoteError::Lifecycle(LifecycleError::AccountRebind(
        "1 asset".into(),
    )));
    assert!(already.contains("--replace"), "{already}");
    assert!(mismatch.contains("does not match"), "{mismatch}");
    assert!(rebind.contains("fresh library"), "{rebind}");
    for line in [&already, &mismatch, &rebind] {
        assert!(!line.starts_with("Recovery setup failed"), "{line}");
    }
}

/// Any other failure is wrapped in the generic line, carrying the English catalog
/// message for its code when it has one.
#[test]
fn other_failures_are_wrapped_in_the_failed_line() {
    let line = describe_escrow_error(&RemoteError::Recovery(RecoveryError::NotEnrolled));
    assert_eq!(
        line,
        format!(
            "Recovery setup failed: {}",
            error_message(error_codes::ESCROW_NOT_STORED)
        )
    );
}
