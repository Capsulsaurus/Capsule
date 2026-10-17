import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - LoginViewModel

/// Drives sign-in on a server that runs its own credential ceremony, and hands
/// off to the identity provider when the user picked that path instead.
///
/// The credential never becomes state that outlives the attempt: the password
/// is wrapped in a ``RedactedSecret`` on its way to the port and the input is
/// cleared on every exit path. Nothing here ever holds a token — that is
/// ``AuthPort``'s contract, and it is why a view model can render every screen
/// in this module without being able to leak a credential.
@MainActor
@Observable
public final class LoginViewModel {
    public private(set) var state: ScreenState = .idle
    public private(set) var account: AccountSummary?
    public private(set) var isSubmitting = false
    /// Whether the server accepts a passkey, so the button is offered only when
    /// it will work.
    public private(set) var supportsPasskeys = false

    public var handleInput = ""
    /// Bound to a `SecureField`; cleared after every attempt.
    public var passwordInput = ""
    /// The second factor, when the server asks for one. Kept separate so an
    /// `.authInvalidCredentials` on the password is distinguishable in the UI
    /// from one on the code.
    public var totpInput = ""

    private let credentials: any LocalCredentialPort
    private let auth: any AuthPort
    private let server: ServerInfo

    public init(credentials: any LocalCredentialPort, auth: any AuthPort, server: ServerInfo) {
        self.credentials = credentials
        self.auth = auth
        self.server = server
    }

    /// The handle needs an `@`, because `user@server.tld` is the handle shape
    /// and a bare username would be sent to the wrong server.
    public var canSubmit: Bool {
        !isSubmitting
            && handleInput.contains("@")
            && !passwordInput.isEmpty
    }

    /// Whether the failure should be shown against the credential fields rather
    /// than as a banner. Both codes are deliberately indistinguishable as to
    /// *which* credential was wrong.
    public var showsCredentialFailure: Bool {
        state.failure?.code == .authInvalidCredentials
    }

    /// Whether the server is telling the user to slow down.
    public var isRateLimited: Bool {
        state.failure?.code == .authRateLimited
    }

    public func load() async {
        supportsPasskeys = await credentials.supportsPasskeys()
        state = .ready
    }

    /// Sign in with the server's own ceremony.
    public func signIn() async {
        guard canSubmit else { return }
        let secret = RedactedSecret(passwordInput)
        let totp = totpInput.trimmingCharacters(in: .whitespacesAndNewlines)
        defer {
            passwordInput = ""
            totpInput = ""
        }
        await attempt {
            try await self.credentials.signIn(
                handle: self.handleInput,
                password: secret,
                totp: totp.isEmpty ? nil : totp
            )
        }
    }

    /// Sign in with a passkey — no secret this process can see, and no field to
    /// clear afterwards.
    public func signInWithPasskey() async {
        guard supportsPasskeys, handleInput.contains("@") else { return }
        await attempt {
            try await self.credentials.signInWithPasskey(handle: self.handleInput)
        }
    }

    /// Hand off to the identity provider.
    ///
    /// The IdP authenticates the **session**; the master key never derives from,
    /// and is never visible to, the credential verifier — so an IdP compromise
    /// costs a session, not the photographs.
    public func signInWithIdentityProvider() async {
        guard let issuer = server.oidcIssuer else { return }
        await attempt {
            try await self.auth.signInWithIdentityProvider(issuer: issuer)
        }
    }

    private func attempt(_ work: @Sendable () async throws -> AccountSummary) async {
        isSubmitting = true
        state = .loading
        defer { isSubmitting = false }
        do {
            account = try await work()
            state = .ready
        } catch {
            account = nil
            state = .failed(AuthPresentableError(error))
        }
    }
}
