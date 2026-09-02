import CapsuleDomain
import CapsuleMock
import CapsulePorts
import Foundation

// MARK: - PreviewCredentialBehaviour

/// Which failure a preview or a test wants to see.
///
/// Deterministic switches rather than random failure: the error states are
/// exactly the states that are unreachable from a happy path, so they need a
/// way to be *asked for*, and a mock that failed by chance would make every
/// test flaky.
public struct PreviewCredentialBehaviour: Sendable, Equatable, Hashable {
    public var rejectsPassword: Bool
    public var isRateLimited: Bool
    public var handleIsTaken: Bool
    public var supportsPasskeys: Bool

    public init(
        rejectsPassword: Bool = false,
        isRateLimited: Bool = false,
        handleIsTaken: Bool = false,
        supportsPasskeys: Bool = true
    ) {
        self.rejectsPassword = rejectsPassword
        self.isRateLimited = isRateLimited
        self.handleIsTaken = handleIsTaken
        self.supportsPasskeys = supportsPasskeys
    }

    public static let healthy = PreviewCredentialBehaviour()
}

// MARK: - PreviewCredentials

/// A ``LocalCredentialPort`` over ``MockEnvironment``.
///
/// A successful sign-in is delegated to the mock identity store rather than
/// faked here, so the whole app agrees about who is signed in: the auth state
/// stream fires, the session ledger gains a current session, and every other
/// screen sees the same world.
///
/// The password it is handed is never stored and never logged — it is checked
/// against the behaviour flags and dropped, which is the only thing a mock has
/// any business doing with a credential.
public actor PreviewCredentials: LocalCredentialPort {
    private let store: MockIdentityStore
    private let behaviour: PreviewCredentialBehaviour

    public init(environment: MockEnvironment, behaviour: PreviewCredentialBehaviour = .healthy) {
        store = environment.identityStore
        self.behaviour = behaviour
    }

    public func signIn(handle: String, password: RedactedSecret, totp: String?) async throws -> AccountSummary {
        _ = password
        _ = totp
        try checkCredentialFailures()
        return try await store.signInLocally(handle: handle)
    }

    public func createAccount(handle: String, password: RedactedSecret) async throws -> AccountSummary {
        _ = password
        if behaviour.handleIsTaken {
            throw CapsuleError(
                code: .authUserAlreadyExists,
                detail: "CapsuleMock: the handle is already registered"
            )
        }
        try checkCredentialFailures()
        return try await store.signInLocally(handle: handle)
    }

    public func supportsPasskeys() async -> Bool {
        behaviour.supportsPasskeys
    }

    public func signInWithPasskey(handle: String) async throws -> AccountSummary {
        try checkCredentialFailures()
        return try await store.signInLocally(handle: handle)
    }

    /// Rate limiting is checked first: a server that is refusing to talk to you
    /// has not evaluated your password, and telling the user it was wrong would
    /// send them to change a credential that was fine.
    private func checkCredentialFailures() throws {
        if behaviour.isRateLimited {
            throw CapsuleError(code: .authRateLimited, detail: "CapsuleMock: too many attempts")
        }
        if behaviour.rejectsPassword {
            throw CapsuleError(code: .authInvalidCredentials, detail: "CapsuleMock: rejected credential")
        }
    }
}
