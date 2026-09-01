import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - AuthState

/// Whether the app has a usable session.
///
/// Carries **no token** in any case. That is the whole point of the state
/// machine: a view model can render every screen from this and never hold, log,
/// or accidentally serialise a credential.
public enum AuthState: Sendable, Equatable, Hashable {
    /// No session on this device.
    case signedOut
    /// A session exists and is usable.
    case signedIn(AccountSummary)
    /// A session exists but needs fresh local authentication before a sensitive
    /// action — enrolling a device, opening Trash or Hidden.
    case requiresLocalAuth(AccountSummary)
    /// The session lapsed against its sliding or hard expiry. Re-authentication
    /// is required; nothing local is lost.
    case expired(AccountSummary)
}

// MARK: - AuthPort

/// Sessions and sign-in.
///
/// **This port never exposes a raw token.** Session and access tokens live
/// inside the SDK, which attaches them to requests; nothing above this line ever
/// sees one. A token that reached a view model is a token that can reach a log,
/// a crash report, or a screenshot — so the type system simply does not offer
/// one.
public protocol AuthPort: Sendable {
    /// The current state, without prompting.
    ///
    /// Maps to `auth.state`.
    func state() async -> AuthState

    /// Sign in with local credentials — password plus TOTP, or a passkey.
    /// Which factors are needed is decided by the server's configuration, and
    /// the ceremony is driven by the SDK.
    ///
    /// Maps to `auth.login_local`.
    func signInLocally(handle: String) async throws -> AccountSummary

    /// Sign in through an external identity provider.
    ///
    /// The IdP authenticates the **session**; the master key never derives from,
    /// and is never visible to, the credential verifier.
    ///
    /// Maps to `auth.login_oidc`.
    func signInWithIdentityProvider(issuer: URL) async throws -> AccountSummary

    /// Re-authenticate locally to satisfy a freshness gate, without a full
    /// sign-out and sign-in.
    ///
    /// Maps to `auth.refresh_local_auth`.
    func confirmLocalAuthentication() async throws

    /// Sign out this device's session only.
    ///
    /// Maps to `auth.logout`.
    func signOut() async throws

    /// A stream of state changes — expiry, revocation from another device, a
    /// completed sign-in.
    func changes() -> AsyncStream<AuthState>
}

// MARK: - DevicePort

/// The device directory and the session ledger.
public protocol DevicePort: Sendable {
    /// Every enrolled device, revoked ones included.
    ///
    /// A revoked device is **listed, not hidden**: its key stays in the
    /// directory forever so everything it signed remains verifiable, and a user
    /// auditing their account should see the same history the cryptography does.
    ///
    /// Maps to `devices.list`.
    func devices() async throws -> [DeviceRecord]

    /// Every session, with its expiries and its advisory cohort.
    ///
    /// Maps to `auth.list_sessions`.
    func sessions() async throws -> [SessionRecord]

    /// The durable cohort map, which outlives session expiry — without it the
    /// "seen before" question is unanswerable exactly when it matters.
    ///
    /// Maps to `auth.list_cohorts`.
    func cohorts() async throws -> [DeviceCohort]

    /// Revoke one session. Authenticated by any active session token.
    ///
    /// Maps to `auth.revoke_session`.
    func revokeSession(_ id: SessionID) async throws

    /// Revoke **every** session.
    ///
    /// Authenticated by proof of master-key possession, not by a session token —
    /// deliberately asymmetric, so an attacker holding a stolen token can revoke
    /// only that one session and cannot lock the legitimate user out of every
    /// device. Implementations drive the challenge-signature ceremony
    /// internally; a request without a valid proof revokes **nothing at all**,
    /// so there is no partial success to clean up.
    ///
    /// Maps to `auth.revoke_all_sessions`.
    func revokeAllSessions() async throws

    /// Revoke a device, removing it from the album groups it belongs to.
    ///
    /// Maps to `devices.revoke`.
    func revokeDevice(_ id: DeviceID) async throws

    /// Bundle the cohort hash and its device/session map for a support report.
    /// The client **asserts, it does not litigate** — there is no "this isn't my
    /// device" toggle, because a user cannot adjudicate a hash.
    ///
    /// Maps to `auth.cohort_support_bundle`.
    func supportBundle(for cohortHash: String) async throws -> DeviceCohort

    /// A stream that fires when the device or session set changes.
    func changes() -> AsyncStream<Void>
}

// MARK: - EnrollmentPort

/// Adding another device to the account.
public protocol EnrollmentPort: Sendable {
    /// Issue an enrollment code from an already-enrolled device.
    ///
    /// Requires **fresh local authentication**: a valid session token alone
    /// cannot start a cross-device add, so a stolen, stale token cannot enroll a
    /// rogue device.
    ///
    /// Maps to `enrollment.issue_code`.
    func issueEnrollmentCode() async throws -> EnrollmentCode

    /// Redeem a code on the joining device.
    ///
    /// Every failure — unknown, already redeemed, expired, rate-limited — is
    /// reported identically, so redemption is not an oracle.
    ///
    /// Maps to `enrollment.redeem_code`.
    func redeem(code: String) -> AsyncStream<EnrollmentProgress>

    /// Watch a ceremony started by ``issueEnrollmentCode()`` from the issuing
    /// side.
    ///
    /// Maps to `enrollment.observe`.
    func observeEnrollment(channelHandle: String) -> AsyncStream<EnrollmentProgress>

    /// Abandon an in-progress ceremony and invalidate its code.
    ///
    /// Maps to `enrollment.cancel`.
    func cancelEnrollment(channelHandle: String) async throws
}

// MARK: - RecoveryPort

/// The recovery secret, its escrow, and the verification cadence.
public protocol RecoveryPort: Sendable {
    /// The account's recovery setup, with no secret in it.
    ///
    /// Maps to `recovery.summary`.
    func summary() async throws -> RecoveryEscrowSummary

    /// Mint a recovery secret and store the wrapped master key in escrow. The
    /// returned secret is shown to the user **once** and never persisted by the
    /// app.
    ///
    /// Maps to `recovery.setup`.
    func setUpRecovery() async throws -> String

    /// Verify a passphrase against the cached escrow blob.
    ///
    /// **Local-only** — no server round-trip, so it works offline, creates no
    /// guessing surface, and a failure cannot lock anything. The implementation
    /// refreshes a stale escrow and retries once before reporting a mismatch;
    /// without that, every rotation from another device would manufacture false
    /// failures here.
    ///
    /// Maps to `recovery.verify_secret`.
    func verify(passphrase: String) async throws -> RecoveryVerificationOutcome

    /// Snooze the verification prompt. Advisory by design: the check **never**
    /// blocks sync, unlock, or any critical flow.
    ///
    /// Maps to `recovery.snooze_verification`.
    func snoozeVerification(until: CapsuleTimestamp) async throws

    /// Run the guided rotation after repeated failures or an explicit "I lost
    /// it".
    ///
    /// **Wrap rotation, not key rotation**: the same master key is re-wrapped
    /// under a fresh secret, an O(1) escrow replacement with no data
    /// re-encryption and no blob-hash changes. The old escrow is deleted, so the
    /// lost secret unwraps nothing.
    ///
    /// Maps to `recovery.rotate_secret`.
    func rotateRecoverySecret() async throws -> String

    /// Restore an account from a recovery secret on a fresh device.
    ///
    /// Maps to `recovery.restore_from_escrow`.
    func restore(usingRecoverySecret secret: String) async throws -> AccountSummary
}
