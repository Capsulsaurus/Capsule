import Foundation

// MARK: - LocalAuthMethod

/// Which authenticator the gate will actually use on this device.
///
/// *SR1* specifies "biometric where enrolled (Face ID / Touch ID /
/// BiometricPrompt), else the device or account credential" — so this is an
/// ordered fallback, not a user preference, and the screen reports what will
/// happen rather than offering a choice the platform does not give.
public enum LocalAuthMethod: Sendable, Equatable, Hashable {
    /// A biometric is enrolled and will be used.
    case biometric
    /// No biometric is enrolled; the device passcode or account password will
    /// be used.
    case deviceCredential
    /// The device has no credential set at all, so the gate cannot challenge.
    /// Reported plainly rather than silently letting the views open — the user
    /// needs to know their trash is unprotected, and only they can fix it.
    case unavailable

    /// The catalog key describing this method.
    public var descriptionKey: String {
        switch self {
        case .biometric: "app.settings.security.method.biometric"
        case .deviceCredential: "app.settings.security.method.credential"
        case .unavailable: "app.settings.security.method.unavailable"
        }
    }
}

// MARK: - LocalAuthenticator

/// The seam over the platform's local-authentication ceremony.
///
/// A protocol rather than a direct `LAContext` call so a view model is testable
/// without a device: the ceremony itself is the one part of this screen that
/// cannot be exercised in a unit test, and it is therefore the one part that
/// must be behind a stub.
public protocol LocalAuthenticator: Sendable {
    /// Which authenticator this device would actually use.
    func availableMethod() async -> LocalAuthMethod

    /// Run the ceremony. Returns `false` when the user cancelled — a cancel is
    /// not an error and must not be reported as one.
    func authenticate(reasonKey: String) async throws -> Bool
}
