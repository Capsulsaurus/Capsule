import CapsuleDomain
import Foundation
import Observation

// MARK: - ServerConnectViewModel

/// Drives domain entry, `.well-known/capsule/server-info` discovery, and the
/// pinning decision.
///
/// Discovery and pinning are two steps on purpose. Fetching the document tells
/// the client where the API is; **trusting** the signing key it names is the
/// user's decision, and it is the decision every later connection is checked
/// against. Collapsing them would mean the first response a domain ever sent
/// silently became the trust anchor.
@MainActor
@Observable
public final class ServerConnectViewModel {
    public private(set) var state: ScreenState = .idle
    public private(set) var server: ServerInfo?
    public private(set) var isPinned = false

    /// The domain the user is typing.
    public var domainInput = ""

    private let discovery: any ServerDiscoveryPort
    private let clientProtocolVersion: Int

    public init(discovery: any ServerDiscoveryPort, clientProtocolVersion: Int = 1) {
        self.discovery = discovery
        self.clientProtocolVersion = clientProtocolVersion
    }

    /// Whether the typed domain is worth a request yet. Deliberately permissive
    /// — the server-info fetch is the real validator, and a strict client-side
    /// pattern would reject valid hosts (an onion address, a LAN name, a
    /// non-ASCII IDN) that the fetch would have resolved.
    public var canDiscover: Bool {
        !domainInput.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// The signing key in the same chunked grouping every other comparable code
    /// in the app uses, so a user checking it against a value their admin sent
    /// them is comparing like with like.
    public var signingKeyDisplay: String {
        guard let server else { return "" }
        return ChunkedCodeFormatter.chunked(server.signingKeyFingerprint)
    }

    /// Whether this build can talk to the discovered server at all. A version
    /// mismatch is not negotiable, so it is reported before the user is asked
    /// to pin anything.
    public var isProtocolCompatible: Bool {
        server?.isCompatible(withClientProtocolVersion: clientProtocolVersion) ?? true
    }

    /// Restore a server pinned on an earlier launch, so a returning user is not
    /// asked to re-type a domain they already trusted.
    public func loadPinnedServer() async {
        state = .loading
        do {
            if let pinned = try await discovery.pinnedServer() {
                server = pinned
                domainInput = pinned.origin
                isPinned = true
                state = .ready
            } else {
                state = .empty
            }
        } catch {
            state = .failed(AuthPresentableError(error))
        }
    }

    /// Fetch `.well-known/capsule/server-info`.
    public func discover() async {
        guard canDiscover else { return }
        state = .loading
        isPinned = false
        do {
            server = try await discovery.discover(domain: domainInput)
            state = .ready
        } catch {
            server = nil
            state = .failed(AuthPresentableError(error))
        }
    }

    /// Trust the discovered signing key for this origin.
    ///
    /// Refuses when the protocol versions do not overlap: pinning a server this
    /// build cannot speak to would leave the user with a trusted anchor and no
    /// working connection, and the honest answer is "update the app".
    @discardableResult
    public func pin() async -> Bool {
        guard let server, isProtocolCompatible else { return false }
        do {
            try await discovery.pin(server)
            isPinned = true
            return true
        } catch {
            state = .failed(AuthPresentableError(error))
            return false
        }
    }

    /// Start over with a different domain.
    public func reset() {
        server = nil
        isPinned = false
        state = .idle
    }
}
