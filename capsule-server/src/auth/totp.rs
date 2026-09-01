//! The second factor (slice `S-C55`) — enrolling a TOTP authenticator, and demanding one at
//! sign-in.
//!
//! # The defect this slice fixes, stated plainly
//!
//! The retired Salvo tree had all four TOTP operations and **never asked for a code**. Its login
//! handler called `authenticate_user`, which returns a token pair unconditionally; the
//! `generate_mfa_token` that would have issued a challenge had no caller outside its own tests.
//! So an account could enroll a second factor, see it confirmed, and still be signed into with a
//! password alone — a security control that reported success and did nothing, which is worse
//! than not offering one. design/authentication.md names password + TOTP as a **first-class**
//! local auth path, so the answer is to make the path real rather than to withdraw it.
//!
//! # Where the algorithm lives, and why it is not behind the port
//!
//! [`TotpCodes`] is a concrete type, for exactly the reason
//! [`SessionTokens`](super::SessionTokens) is one: verifying a code is a pure function of a
//! secret and a clock, with no state and nothing to reach. Putting it behind the port would push
//! RFC 6238's parameters — the digit count, the step, the drift window — into whatever adapter
//! happens to be loaded, and the only adapter this crate has is a test double. The algorithm
//! would then live in the suite, which is the one place it must not.
//!
//! What *is* behind [`TotpStore`] is the enrollment record: a secret, whether it has been
//! confirmed, and the last step a code was accepted at.
//!
//! # Replay is a store operation, not a check
//!
//! RFC 6238 §5.2 requires that a code be accepted at most once. A code is valid for a whole step
//! and, with drift, for three — so "somebody shoulder-surfed the six digits and typed them in
//! twelve seconds later" is a real attack that verification alone cannot see. The defence is a
//! **compare-and-set** on the highest step this account has already used, and it is
//! [`TotpStore::consume`] rather than a read followed by a write, because two sign-ins racing on
//! the same code is precisely the case a read-then-write loses.
//!
//! # Disabling needs a code, not just a session
//!
//! [`TotpStore::disable`] is reached only after a live code verifies. A stolen access token
//! would otherwise turn off the control that exists to make a stolen access token insufficient.
//! The retired surface got this right and it is preserved deliberately.
//!
//! # No adapter here
//!
//! Same reason [`AccountDirectory`](super::AccountDirectory) has none: the real one is Postgres,
//! and a shared-secret store in `src/` is a fake credential store shipped inside the server
//! binary. The suite's lives in `tests/support/`.

use std::fmt;
use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};
use subtle::ConstantTimeEq as _;
use totp_rs::{Algorithm, Secret, TOTP};

use super::directory::DirectoryFuture;
use crate::store::UserId;

/// The time step every Capsule TOTP runs on, in seconds.
///
/// Thirty, which is what every authenticator app defaults to and what RFC 6238 recommends. Not
/// configurable: a deployment that chose forty would issue provisioning URIs that a client
/// honouring the default silently mis-generates for, and the failure looks like a broken app.
pub const STEP_SECONDS: u64 = 30;

/// How many digits a code carries.
pub const DIGITS: usize = 6;

/// How many steps either side of the current one a code is still accepted at.
///
/// One, so the accepted window is ninety seconds wide. Zero would refuse a correct code from a
/// phone whose clock is two seconds slow; more than one widens the guessing surface for no
/// usability gain, and every step of drift is a step a replay can hide in.
pub const DRIFT_STEPS: u64 = 1;

/// How long a second-factor challenge is good for.
///
/// Five minutes. Long enough to open an authenticator app and read a code, short enough that a
/// challenge intercepted in transit is worthless by the time it is used. It is deliberately much
/// shorter than an access token's lifetime: this credential proves *one* factor, and a long-lived
/// half-authentication is a password that never expires.
pub const CHALLENGE_TTL: SignedDuration = SignedDuration::from_mins(5);

/// A TOTP shared secret, base32 as the provisioning URI carries it.
///
/// `Debug` is hand-written and prints nothing. This value is a credential in exactly the sense a
/// password is — anyone holding it can mint codes forever — and a derived `Debug` would publish
/// it to any `tracing` field or panic message that formatted the record it sits in.
#[derive(Clone, PartialEq, Eq)]
pub struct TotpSecret(String);

impl TotpSecret {
    /// Wrap a base32 secret.
    pub fn new(base32: impl Into<String>) -> Self {
        Self(base32.into())
    }

    /// The base32 form, for an adapter that has to persist it.
    ///
    /// Named to be conspicuous at a call site: every use of this is a use of a shared secret.
    pub fn expose_base32(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for TotpSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TotpSecret(<redacted>)")
    }
}

/// Whether an enrollment has been confirmed with a live code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollmentState {
    /// A secret has been issued and no code has confirmed it. Sign-in is **not** gated.
    Pending,
    /// A code confirmed the secret. Sign-in demands one.
    Active,
}

/// One account's second factor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TotpEnrollment {
    /// The account it belongs to.
    pub user_id: UserId,
    /// The shared secret.
    pub secret: TotpSecret,
    /// Whether a code has confirmed it.
    pub state: EnrollmentState,
    /// The highest step a code has already been accepted at, if any.
    ///
    /// The replay defence's whole state. `None` on a fresh enrollment, which is why the first
    /// code ever accepted is the confirming one.
    pub last_step: Option<u64>,
    /// When the secret was issued.
    pub enrolled_at: Timestamp,
    /// When a code first confirmed it.
    pub activated_at: Option<Timestamp>,
}

/// What starting an enrollment did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeginOutcome {
    /// A secret was issued and is waiting for a code to confirm it.
    Started,
    /// This account already has a **confirmed** second factor, and nothing was written.
    ///
    /// The one refusal this operation makes, and it exists because the alternative is worse: an
    /// enroll that overwrote an active secret would let a stolen session silently replace the
    /// factor with one the attacker holds, without ever presenting a code.
    AlreadyActive,
}

/// What confirming an enrollment did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivateOutcome {
    /// The enrollment is now active.
    Activated,
    /// There was no pending enrollment to confirm.
    NotPending,
}

/// What a code's step did against the replay ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumeOutcome {
    /// The step is new, and is now recorded as used.
    Fresh,
    /// The step is at or below one already used. The code is a replay.
    Replayed,
    /// There is no enrollment to consume against.
    NotEnrolled,
}

/// Where second-factor enrollments live.
pub trait TotpStore: fmt::Debug + Send + Sync {
    /// Record `record` as this account's pending enrollment, unless one is already **active**.
    ///
    /// **The check and the write are one operation**, as
    /// [`AccountRegistry::create`](super::AccountRegistry::create) is: a caller that read, saw
    /// no active enrollment and then wrote has a window in which a confirmation lands, and the
    /// confirmed factor is then silently replaced.
    ///
    /// A *pending* enrollment is replaced without ceremony — somebody who abandoned a QR code
    /// and started again is the ordinary case, and nothing is protecting an unconfirmed secret.
    fn begin(&self, record: TotpEnrollment) -> DirectoryFuture<'_, BeginOutcome>;

    /// This account's enrollment, or `None`.
    fn read<'a>(&'a self, user: &'a UserId) -> DirectoryFuture<'a, Option<TotpEnrollment>>;

    /// Promote a pending enrollment to active, recording `step` as used and `at` as the moment.
    ///
    /// One operation, and it carries the replay ledger's first entry: the code that confirmed an
    /// enrollment must not also be the code that completes a sign-in a moment later.
    fn activate<'a>(
        &'a self,
        user: &'a UserId,
        step: u64,
        at: Timestamp,
    ) -> DirectoryFuture<'a, ActivateOutcome>;

    /// Record `step` as used, if it is higher than every step used before.
    ///
    /// Compare-and-set, not read-then-write; see the module docs.
    fn consume<'a>(&'a self, user: &'a UserId, step: u64) -> DirectoryFuture<'a, ConsumeOutcome>;

    /// Remove this account's enrollment. Reports whether there was one.
    fn disable<'a>(&'a self, user: &'a UserId) -> DirectoryFuture<'a, bool>;
}

/// RFC 6238 codes, over one issuer name.
///
/// A concrete type rather than a port; see the module docs. Holds no secret — every operation
/// takes the one it is working with.
#[derive(Debug, Clone)]
pub struct TotpCodes {
    issuer: String,
}

/// A secret could not be turned into a usable authenticator.
///
/// A server fault rather than a caller's: it means the secret this server generated or stored is
/// not one RFC 6238 admits, which is a corrupted record or a bad generator.
#[derive(Debug, thiserror::Error)]
#[error("the stored second-factor secret is unusable: {detail}")]
pub struct UnusableSecret {
    /// What was wrong with it. Never the secret.
    pub detail: String,
}

impl TotpCodes {
    /// Codes issued under `issuer`, which is the name an authenticator app shows.
    pub fn new(issuer: impl Into<String>) -> Self {
        Self {
            issuer: issuer.into(),
        }
    }

    /// A fresh, RFC-compliant shared secret.
    #[must_use]
    pub fn generate_secret() -> TotpSecret {
        TotpSecret::new(Secret::generate_secret().to_encoded().to_string())
    }

    /// The step `at` falls in.
    #[must_use]
    pub fn step_at(at: Timestamp) -> u64 {
        // A pre-epoch instant is not a time any TOTP is generated at; treating it as step zero
        // keeps this total rather than introducing a failure mode nothing can act on.
        u64::try_from(at.as_second()).unwrap_or(0) / STEP_SECONDS
    }

    /// The `otpauth://` URI an authenticator app scans.
    ///
    /// # Errors
    ///
    /// Returns [`UnusableSecret`] if the secret is not one RFC 6238 admits.
    pub fn provisioning_uri(
        &self,
        secret: &TotpSecret,
        account: &str,
    ) -> Result<String, UnusableSecret> {
        Ok(self.authenticator(secret, account)?.get_url())
    }

    /// Check `code` against `secret` at `at`, and report the step it matched.
    ///
    /// The **step** rather than a bool, because the caller has to record it: a code accepted
    /// without its step recorded is a code that can be presented again inside the same window.
    ///
    /// Candidate steps are walked from the newest backwards, so a code that matches two steps —
    /// which a 30-second step and a six-digit code make possible, if unlikely — records the
    /// higher one and closes the wider window.
    ///
    /// # Errors
    ///
    /// Returns [`UnusableSecret`] if the secret is not one RFC 6238 admits.
    pub fn verify(
        &self,
        secret: &TotpSecret,
        code: &str,
        at: Timestamp,
    ) -> Result<Option<u64>, UnusableSecret> {
        // Cheap structural rejection first, so a body of arbitrary length never reaches the HMAC
        // loop. It is not a security property — the comparison below is constant-time — it is a
        // refusal to do work on something that cannot be a code.
        if code.len() != DIGITS || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Ok(None);
        }

        // No account name is needed to *generate*, and passing one here would make the answer
        // depend on a value the caller might get wrong.
        let authenticator = self.authenticator(secret, "")?;
        let current = Self::step_at(at);
        for offset in 0..=(DRIFT_STEPS * 2) {
            // Saturating, because the first `DRIFT_STEPS` of the Unix epoch have no earlier step
            // to look back at. That window is not reachable in production and *is* reachable in
            // a test whose clock starts at zero, which is exactly the kind of arithmetic that
            // panics in the one place nobody exercised.
            let Some(step) = current.saturating_add(DRIFT_STEPS).checked_sub(offset) else {
                continue;
            };
            let expected = authenticator.generate(step * STEP_SECONDS);
            // Constant-time. A byte-wise `==` on a code leaks, through timing, how many leading
            // digits were right — which turns one guess against a million into six against ten.
            // `subtle` rather than a hand-rolled fold, because a fold is what the optimizer is
            // free to short-circuit and the resulting code looks correct forever.
            if bool::from(expected.as_bytes().ct_eq(code.as_bytes())) {
                return Ok(Some(step));
            }
        }
        Ok(None)
    }

    /// The `totp-rs` authenticator for one secret.
    fn authenticator(&self, secret: &TotpSecret, account: &str) -> Result<TOTP, UnusableSecret> {
        let bytes = Secret::Encoded(secret.expose_base32().to_owned())
            .to_bytes()
            .map_err(|error| UnusableSecret {
                detail: error.to_string(),
            })?;
        // Skew is zero here on purpose: the drift window is walked by `verify`, which needs to
        // know *which* step matched, and `totp_rs::check` reports only that one did.
        TOTP::new(
            Algorithm::SHA1,
            DIGITS,
            0,
            STEP_SECONDS,
            bytes,
            Some(self.issuer.clone()),
            account.to_owned(),
        )
        .map_err(|error| UnusableSecret {
            detail: error.to_string(),
        })
    }
}

/// The second factor's collaborators.
#[derive(Debug, Clone)]
pub struct TotpContext {
    enrollments: Arc<dyn TotpStore>,
    codes: Arc<TotpCodes>,
}

impl TotpContext {
    /// Assembles the module.
    pub fn new(enrollments: Arc<dyn TotpStore>, codes: Arc<TotpCodes>) -> Self {
        Self { enrollments, codes }
    }

    /// Where enrollments live.
    pub fn enrollments(&self) -> &dyn TotpStore {
        self.enrollments.as_ref()
    }

    /// The code generator and verifier.
    pub fn codes(&self) -> &TotpCodes {
        self.codes.as_ref()
    }
}

#[cfg(test)]
mod tests {
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
}
