import CapsuleDomain
import Foundation

// MARK: - PasskeyRegistration

/// A passkey that now exists on this account.
///
/// Carries no key material — the private half never leaves the platform
/// authenticator, which is the entire point of enrolling one.
public struct PasskeyRegistration: Sendable, Equatable, Hashable, Identifiable {
    /// The credential's opaque id, for revocation.
    public var id: String
    /// The name the authenticator gave it, e.g. the device or password manager.
    public var authenticatorLabel: String
    public var createdAt: CapsuleTimestamp

    public init(id: String, authenticatorLabel: String, createdAt: CapsuleTimestamp) {
        self.id = id
        self.authenticatorLabel = authenticatorLabel
        self.createdAt = createdAt
    }
}

// MARK: - TotpEnrollment

/// An in-progress TOTP enrolment.
///
/// The seed is a ``RedactedSecret`` because it is one: anything that captures it
/// — a log line, a screenshot uploaded to a bug tracker, a pasteboard sync —
/// reproduces the second factor. It is shown, transcribed, and dropped.
public struct TotpEnrollment: Sendable {
    /// The shared seed, base32, as the user would type it into another
    /// authenticator.
    public var seed: RedactedSecret
    /// The `otpauth://` URI the QR code encodes. Also a secret: it *contains*
    /// the seed.
    public var provisioningURI: RedactedSecret
    /// The account label the authenticator will display.
    public var accountLabel: String
    /// The issuer the authenticator will display.
    public var issuer: String

    public init(
        seed: RedactedSecret,
        provisioningURI: RedactedSecret,
        accountLabel: String,
        issuer: String
    ) {
        self.seed = seed
        self.provisioningURI = provisioningURI
        self.accountLabel = accountLabel
        self.issuer = issuer
    }
}

// MARK: - SecondFactorPort

/// Enrolling a second factor on an existing account.
///
/// **Not yet in `CapsulePorts`.** *Authentication — Account Types* names
/// password+TOTP and passkeys as the local-auth factors; the enrolment surface
/// is not yet in the SDK contract, so it is declared next to the two screens
/// that drive it.
public protocol SecondFactorPort: Sendable {
    /// Whether this platform has an authenticator that can hold a passkey.
    /// A device with none must be told so, not shown a button that fails.
    func isPasskeyEnrollmentAvailable() async -> Bool

    /// Run the platform passkey ceremony and register the public half.
    ///
    /// The user-visible ceremony belongs to the OS. This returns only once the
    /// credential is registered server-side, so a partial success cannot be
    /// mistaken for an enrolled factor.
    func enrollPasskey(displayName: String) async throws -> PasskeyRegistration

    /// Mint a TOTP seed and return it for transcription.
    ///
    /// Nothing is armed until ``confirmTotp(code:)`` succeeds: an enrolment that
    /// took effect before the user proved they had transcribed it would lock out
    /// anyone whose authenticator app crashed mid-scan.
    func beginTotpEnrollment() async throws -> TotpEnrollment

    /// Confirm the seed was transcribed by checking one generated code.
    ///
    /// - Throws: ``CapsuleError`` with `.authInvalidCredentials` for a wrong
    ///   code, `.authRateLimited` when the user is guessing.
    func confirmTotp(code: String) async throws
}
