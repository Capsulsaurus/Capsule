import CapsuleDomain
import Foundation

// MARK: - ServerAuthMethod

/// Which login ceremonies a deployment offers
/// (*Authentication — Choosing an Auth Path*).
///
/// A deployment may enable either or both, so this is a set the chooser screen
/// renders, never a single value: a server with only OIDC must not show a
/// password form, and a server with only local auth must not show an IdP
/// button.
public enum ServerAuthMethod: ClosedWireEnum {
    /// Password + TOTP, or a passkey. Verified by the server itself.
    case local
    /// An external identity provider, authorization-code + PKCE. Capsule is a
    /// relying party only.
    case oidc
    case unknown(String)

    public static let knownCases: [ServerAuthMethod] = [.local, .oidc]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .local: "local"
        case .oidc: "oidc"
        case let .unknown(raw): raw
        }
    }
}

// MARK: - ServerInfo

/// What `.well-known/capsule/server-info` returns
/// (*Authentication — Identity and Discovery*).
///
/// Server-scoped facts only. The document **never** enumerates users, and this
/// type has nowhere to put a user list even if a server sent one — the shape is
/// the enforcement.
public struct ServerInfo: Sendable, Equatable, Hashable, Identifiable {
    /// The origin the user typed, normalised. `capsule.example`.
    public var origin: String
    /// The API base the client will talk to.
    public var apiBaseURL: URL
    /// The methods this deployment offers.
    public var authMethods: [ServerAuthMethod]
    /// The IdP issuer, when ``authMethods`` includes ``ServerAuthMethod/oidc``.
    public var oidcIssuer: URL?
    /// The server's signing key, as the user pins it.
    ///
    /// Displayed in the same chunked, fixed-length format as an enrollment
    /// safety code, for the same reason: a human comparing two hex strings
    /// needs the grouping to be identical on both surfaces or the comparison is
    /// worthless.
    public var signingKeyFingerprint: String
    /// The protocol versions this server speaks.
    public var supportedProtocolVersions: ClosedRange<Int>
    /// The floor below which this server refuses a client, for an active
    /// deprecation window.
    public var minProtocolVersion: Int

    public var id: String { origin }

    public init(
        origin: String,
        apiBaseURL: URL,
        authMethods: [ServerAuthMethod],
        oidcIssuer: URL? = nil,
        signingKeyFingerprint: String,
        supportedProtocolVersions: ClosedRange<Int>,
        minProtocolVersion: Int
    ) {
        self.origin = origin
        self.apiBaseURL = apiBaseURL
        self.authMethods = authMethods
        self.oidcIssuer = oidcIssuer
        self.signingKeyFingerprint = signingKeyFingerprint
        self.supportedProtocolVersions = supportedProtocolVersions
        self.minProtocolVersion = minProtocolVersion
    }

    /// Whether this build can speak to this server at all.
    public func isCompatible(withClientProtocolVersion version: Int) -> Bool {
        supportedProtocolVersions.contains(version) && version >= minProtocolVersion
    }

    /// Whether the credential ceremony this server runs itself is on offer.
    public var offersLocalCredentials: Bool { authMethods.contains(.local) }

    /// Whether an external identity provider is on offer.
    public var offersIdentityProvider: Bool { authMethods.contains(.oidc) }
}

// MARK: - ServerDiscoveryPort

/// Resolving a typed domain to a server, and pinning what it claims to be.
///
/// **Not yet in `CapsulePorts`.** `.well-known/capsule/server-info` lands with
/// slice `S-C18` (*Authentication — The `.well-known/capsule/*` Registry*), so
/// this protocol is declared here, next to its only consumer, and moves to
/// `CapsulePorts/IdentityPorts.swift` unchanged when the SDK surface exists.
public protocol ServerDiscoveryPort: Sendable {
    /// Fetch `.well-known/capsule/server-info` for a typed domain.
    ///
    /// The domain is normalised by the implementation, not by the view model: a
    /// user types `Capsule.Example/`, `https://capsule.example`, and
    /// `capsule.example` interchangeably, and all three must reach the same
    /// origin or the pinned key will not match on the next launch.
    func discover(domain: String) async throws -> ServerInfo

    /// Record the server's signing key as trusted for this origin.
    ///
    /// Separate from ``discover(domain:)`` because pinning is the user's
    /// decision: the client shows the key, the user accepts it, and only then
    /// does it become the value future connections are checked against.
    func pin(_ server: ServerInfo) async throws

    /// The server already pinned on this device, if any.
    func pinnedServer() async throws -> ServerInfo?
}
