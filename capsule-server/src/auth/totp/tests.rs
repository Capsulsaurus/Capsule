use jiff::Timestamp;

use super::{DRIFT_STEPS, STEP_SECONDS, TotpCodes, TotpSecret};

/// An instant a whole number of steps after the epoch.
fn at(step: u64) -> Timestamp {
    Timestamp::from_second(i64::try_from(step * STEP_SECONDS).expect("in range"))
        .expect("a representable instant")
}

fn codes() -> TotpCodes {
    TotpCodes::new("Capsule")
}

/// The code an authenticator app would show at `step`.
///
/// Built through `TotpCodes`'s own authenticator rather than a second RFC 6238
/// implementation — a helper that generated codes independently could agree with nothing and
/// the tests would still pass.
fn code_at(codes: &TotpCodes, secret: &TotpSecret, step: u64) -> String {
    codes
        .authenticator(secret, "")
        .expect("a usable secret")
        .generate(step * STEP_SECONDS)
}

#[test]
fn a_secret_round_trips_through_a_provisioning_uri() {
    let codes = codes();
    let secret = TotpCodes::generate_secret();
    let uri = codes
        .provisioning_uri(&secret, "somebody@example.test")
        .expect("a usable secret");

    assert!(uri.starts_with("otpauth://totp/"));
    assert!(uri.contains("issuer=Capsule"));
    assert!(
        uri.contains(secret.expose_base32()),
        "the URI carries the secret the app must scan"
    );
}

#[test]
fn a_code_matches_its_own_step() {
    let codes = codes();
    let secret = TotpCodes::generate_secret();
    let step = 58_000_000;
    let code = code_at(&codes, &secret, step);

    assert_eq!(
        codes.verify(&secret, &code, at(step)).expect("usable"),
        Some(step)
    );
}

#[test]
fn a_code_is_accepted_one_step_either_side_and_not_two() {
    // The whole of the drift contract. Zero would refuse a correct code from a phone two
    // seconds slow; two would widen the window a replay hides in for no usability gain.
    let codes = codes();
    let secret = TotpCodes::generate_secret();
    let step = 58_000_100;
    let code = code_at(&codes, &secret, step);

    for offset in 0..=DRIFT_STEPS {
        assert_eq!(
            codes
                .verify(&secret, &code, at(step + offset))
                .expect("usable"),
            Some(step),
            "a code must be accepted {offset} steps late, and report its own step"
        );
        assert_eq!(
            codes
                .verify(&secret, &code, at(step - offset))
                .expect("usable"),
            Some(step)
        );
    }
    assert_eq!(
        codes
            .verify(&secret, &code, at(step + DRIFT_STEPS + 1))
            .expect("usable"),
        None
    );
    assert_eq!(
        codes
            .verify(&secret, &code, at(step - DRIFT_STEPS - 1))
            .expect("usable"),
        None
    );
}

#[test]
fn something_that_is_not_a_code_is_refused_without_reaching_the_hmac() {
    let codes = codes();
    let secret = TotpCodes::generate_secret();
    for presented in ["", "12345", "1234567", "abcdef", "12 456"] {
        assert_eq!(
            codes
                .verify(&secret, presented, at(58_000_200))
                .expect("usable"),
            None,
            "{presented:?} cannot be a code"
        );
    }
}

#[test]
fn the_drift_window_does_not_run_off_the_start_of_the_epoch() {
    // A clock at zero has no step before it. Reachable in a suite whose clock starts there,
    // unreachable in production, and a subtraction overflow either way.
    let codes = codes();
    let secret = TotpCodes::generate_secret();
    assert_eq!(
        codes.verify(&secret, "000000", at(0)).expect("usable"),
        None
    );

    let code = code_at(&codes, &secret, 0);
    assert_eq!(
        codes.verify(&secret, &code, at(0)).expect("usable"),
        Some(0)
    );
}

#[test]
fn a_secret_that_is_not_base32_is_a_server_fault_and_not_a_refusal() {
    // The distinction matters: a corrupted stored secret must not read as "your code is
    // wrong", which would send somebody round a loop that cannot succeed.
    let codes = codes();
    let broken = TotpSecret::new("not base32 at all!!");
    assert!(codes.verify(&broken, "123456", at(58_000_300)).is_err());
}

#[test]
fn a_secret_is_not_printed_by_debug() {
    let secret = TotpSecret::new("JBSWY3DPEHPK3PXP");
    assert_eq!(format!("{secret:?}"), "TotpSecret(<redacted>)");
    assert!(!format!("{secret:?}").contains("JBSWY"));
}
