import Foundation
import LocalAuthentication

// MARK: - SystemLocalAuthenticator

/// The real local-authentication ceremony, on both platforms.
///
/// `LocalAuthentication` rather than a platform UI framework: the whole gate is
/// one system-owned sheet, so there is nothing here that needs `UIKit` or
/// `AppKit` and nothing that has to live in a `Platform/` island.
///
/// A fresh `LAContext` per call, deliberately. A reused context caches its own
/// grace period, and a *cached* grant is precisely what the per-view window in
/// ``LocalAuthGate`` is supposed to be the only source of — two overlapping
/// grace windows would make "five minutes" mean something else.
public struct SystemLocalAuthenticator: LocalAuthenticator {
    public init() {}

    /// Biometric where enrolled, else the device credential, else nothing —
    /// the fallback order *Local Gallery — SR1* specifies.
    public func availableMethod() async -> LocalAuthMethod {
        let context = LAContext()
        var evaluationError: NSError?
        if context.canEvaluatePolicy(
            .deviceOwnerAuthenticationWithBiometrics,
            error: &evaluationError
        ) {
            return .biometric
        }
        if context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &evaluationError) {
            return .deviceCredential
        }
        return .unavailable
    }

    /// Run the challenge.
    ///
    /// `.deviceOwnerAuthentication` rather than the biometrics-only policy, so
    /// a failed or unenrolled biometric falls through to the passcode instead of
    /// leaving the user with no way in — the doc's "else the device or account
    /// credential", enforced by the policy rather than by a retry loop here.
    public func authenticate(reasonKey: String) async throws -> Bool {
        let context = LAContext()
        let reason = SettingsPhrase.text(forKey: reasonKey)
        return try await context.evaluatePolicy(
            .deviceOwnerAuthentication,
            localizedReason: reason
        )
    }
}
