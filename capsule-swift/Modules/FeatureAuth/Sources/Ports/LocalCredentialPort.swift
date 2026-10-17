import CapsuleDomain
import Foundation

// MARK: - LocalCredentialPort

/// The credential half of local auth: the screens that actually collect a
/// password, a TOTP digit group, or a passkey assertion.
///
/// **Why this is not ``AuthPort``.** `AuthPort.signInLocally(handle:)`
/// deliberately takes only a handle — the factor set is the server's decision
/// and the ceremony is the SDK's, which is the right shape for a caller that
/// merely wants a session. It leaves no room for a screen that has to *render*
/// the password field, so this narrow port sits alongside it rather than
/// widening it. It never returns, stores, or logs a token; a credential enters
/// as a ``RedactedSecret`` and does not come back out.
///
/// **Not yet in `CapsulePorts`.** Local auth is the default path a deployment
/// gets (*Authentication — Design Principles*); this protocol belongs in
/// `IdentityPorts.swift` once the SDK's ceremony surface is settled.
public protocol LocalCredentialPort: Sendable {
    /// Sign in with the server's own credential ceremony.
    ///
    /// - Parameters:
    ///   - handle: the `user@server.tld` handle.
    ///   - password: the password, which the implementation must consume and
    ///     never retain.
    ///   - totp: the second factor, when the server asked for one.
    /// - Throws: ``CapsuleError`` with `.authInvalidCredentials` or
    ///   `.authRateLimited`. Both are reported to the user by code, so an
    ///   attacker probing the form learns only what the server chose to say.
    func signIn(handle: String, password: RedactedSecret, totp: String?) async throws -> AccountSummary

    /// Create an account on this server.
    ///
    /// This establishes the **account** and its server-side metadata only — it
    /// confers no data access (*Device Enrollment — Account-creation auth*).
    /// The master key that actually authorises decryption is minted afterwards,
    /// by the first-device ceremony.
    ///
    /// - Throws: ``CapsuleError`` with `.authUserAlreadyExists` when the handle
    ///   is taken.
    func createAccount(handle: String, password: RedactedSecret) async throws -> AccountSummary

    /// Whether this server will accept a passkey instead of a password.
    func supportsPasskeys() async -> Bool

    /// Sign in with a passkey, which collects no secret this process can see.
    func signInWithPasskey(handle: String) async throws -> AccountSummary
}
