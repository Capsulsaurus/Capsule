import CapsuleCatalog
import Foundation

/// A scripted ``LocalAuthGate`` for tests and SwiftUI previews.
///
/// The production gate puts a Face ID / passcode sheet on screen, which no
/// automated test can answer, so every test of the SR1 gate drives this instead
/// and asserts on the *policy* — refuse without a grant, serve with one, one
/// challenge per grace window — rather than on `LAContext`.
public final class MockLocalAuthGate: LocalAuthGate, @unchecked Sendable {
    private let lock = NSLock()
    private let outcome: LocalAuthError?
    private var challenges = 0

    /// A gate that always grants (`outcome: nil`), or always refuses with a
    /// fixed platform error.
    public init(refusingWith outcome: LocalAuthError? = nil) {
        self.outcome = outcome
    }

    /// How many times the core actually challenged the platform — the assertion
    /// that a grant inside its grace window does not re-prompt.
    public var challengeCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return challenges
    }

    public func authenticate(view: GatedView) throws {
        lock.lock()
        challenges += 1
        lock.unlock()
        if let outcome { throw outcome }
    }
}
