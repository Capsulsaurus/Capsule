import CapsulePorts
import Foundation

/// The local-authentication gate, in a world with no operating system behind it.
///
/// The mocked app composes no system services — the same rule that keeps
/// PhotoKit out of the timeline — and a gate is the one place where breaking it
/// is most visible: `LAContext` on a simulator with no enrolled biometry puts a
/// full-screen *Enter iPhone Passcode* sheet over the app, owned by SpringBoard
/// and dismissable only by someone who knows a passcode the device does not
/// have. A demo build and a UI sweep both stop dead there.
///
/// So this answers the ceremony directly. It still *is* a ceremony — it reports
/// a method, it can refuse — because a gate that always says yes would let a
/// screen forget it is gated at all.
public actor MockLocalAuthenticator: LocalAuthenticator {
    private let method: LocalAuthMethod
    private let grants: Bool

    /// - Parameters:
    ///   - method: what this device would use, as reported to Settings.
    ///   - grants: whether the challenge succeeds. `false` models the user
    ///     cancelling, which is the case screens most often get wrong.
    public init(method: LocalAuthMethod = .biometric, grants: Bool = true) {
        self.method = method
        self.grants = grants
    }

    public func availableMethod() async -> LocalAuthMethod { method }

    public func authenticate(reasonKey _: String) async throws -> Bool { grants }
}
