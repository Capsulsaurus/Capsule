import CapsuleDomain
import Foundation
import Observation

// MARK: - AuthPathChooserViewModel

/// Offers the auth paths a *specific* deployment enables
/// (*Authentication — Choosing an Auth Path*).
///
/// The list comes from the discovered server rather than from a build-time
/// constant, because a deployment may enable either path or both: showing a
/// password form to an OIDC-only server, or an SSO button to a server with no
/// IdP, is a dead end the user cannot diagnose.
///
/// Neither choice weakens the cryptographic binding, and the screen says so:
/// the IdP or the password authenticates the **session**, while the master key
/// never derives from, and is never visible to, the credential verifier.
@MainActor
@Observable
public final class AuthPathChooserViewModel {
    public private(set) var state: ScreenState = .idle
    public private(set) var selection: ServerAuthMethod?

    public let server: ServerInfo

    public init(server: ServerInfo) {
        self.server = server
        state = server.authMethods.isEmpty ? .empty : .ready
    }

    /// The methods on offer, in the order the screen shows them: the server's
    /// own ceremony first, since it is the default a deployment gets without
    /// configuring an IdP.
    public var methods: [ServerAuthMethod] {
        var ordered: [ServerAuthMethod] = []
        if server.offersLocalCredentials { ordered.append(.local) }
        if server.offersIdentityProvider { ordered.append(.oidc) }
        return ordered
    }

    /// A server that advertised only methods this build does not know.
    ///
    /// Reported rather than silently ignored: the values are preserved verbatim
    /// by ``ServerAuthMethod``, and the honest message is "this server offers a
    /// sign-in method this version does not support".
    public var hasOnlyUnknownMethods: Bool {
        !server.authMethods.isEmpty && methods.isEmpty
    }

    public func select(_ method: ServerAuthMethod) {
        guard methods.contains(method) else { return }
        selection = method
    }
}
