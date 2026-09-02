import CapsuleDomain
import CapsuleMock
import Foundation

// MARK: - PreviewServerDiscovery

/// A ``ServerDiscoveryPort`` over ``MockEnvironment``.
///
/// Lives in `Sources/` rather than in a test target for the same reason
/// `CapsuleMock` does: the app itself runs on the mock while the real surface is
/// being rebuilt, so every `#Preview` and every UI test needs this to be part of
/// the shipping module rather than something only `swift test` can see.
///
/// It routes through ``MockGate`` so a scenario stays coherent: in
/// ``MockScenario/offline`` this refuses exactly like every other networked
/// call, instead of cheerfully resolving a domain the device cannot reach.
public actor PreviewServerDiscovery: ServerDiscoveryPort {
    private let gate: MockGate
    private let seed: UInt64
    private var pinned: ServerInfo?

    public init(environment: MockEnvironment, pinned: ServerInfo? = nil) {
        gate = MockGate(behaviour: environment.configuration.behaviour)
        seed = environment.configuration.seed
        self.pinned = pinned
    }

    public func discover(domain: String) async throws -> ServerInfo {
        try await gate.admit()
        return Self.server(domain: domain, seed: seed)
    }

    public func pin(_ server: ServerInfo) async throws {
        pinned = server
    }

    public func pinnedServer() async throws -> ServerInfo? {
        pinned
    }

    /// A deployment that offers both auth paths, which is what makes the
    /// chooser screen reachable.
    public static func server(domain: String, seed: UInt64 = 0) -> ServerInfo {
        let origin = normalize(domain)
        return ServerInfo(
            origin: origin,
            apiBaseURL: url("https://\(origin)/api"),
            authMethods: [.local, .oidc],
            oidcIssuer: url("https://\(origin)/oidc"),
            signingKeyFingerprint: fingerprint(origin: origin, seed: seed),
            supportedProtocolVersions: 1 ... 2,
            minProtocolVersion: 1
        )
    }

    /// Normalise the way a real implementation must: a user types
    /// `Capsule.Example/`, `https://capsule.example`, and `capsule.example`
    /// interchangeably, and all three have to pin the same origin.
    private static func normalize(_ domain: String) -> String {
        var text = domain.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        for prefix in ["https://", "http://"] where text.hasPrefix(prefix) {
            text.removeFirst(prefix.count)
        }
        while text.hasSuffix("/") { text.removeLast() }
        return text.isEmpty ? "capsule.example" : text
    }

    /// A stable 16-hex-digit fingerprint for the origin.
    private static func fingerprint(origin: String, seed: UInt64) -> String {
        var hash = MockHash.mix(seed &+ 0x5F37_59DF)
        for byte in origin.utf8 {
            hash = MockHash.mix(hash ^ UInt64(byte))
        }
        return MockHash.hex(hash, digits: 16)
    }

    /// A non-optional `URL` without a force unwrap. The fallback is only
    /// reachable for input a normalised origin cannot produce, and a broken URL
    /// is better than a trap in a preview.
    private static func url(_ string: String) -> URL {
        URL(string: string) ?? URL(fileURLWithPath: "/")
    }
}
