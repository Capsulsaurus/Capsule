import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - InvitationKind

/// Which kind of inbound `https` link is being redeemed.
public enum InvitationKind: String, Sendable, Hashable, Codable, CaseIterable {
    /// `/s/<opaque-id>#<secret>` — someone else's share.
    case share
    /// `/u/<opaque-id>#<key>` — an invitation to upload into someone's library.
    case guestUpload = "guest_upload"

    /// The URL path prefix that names this kind.
    public var pathPrefix: String {
        switch self {
        case .share: "s"
        case .guestUpload: "u"
        }
    }

    /// Where the redemption screen files itself, so the result has somewhere to
    /// land once it is accepted.
    public var owningSection: SidebarItem {
        switch self {
        case .share: .shares
        case .guestUpload: .drops
        }
    }
}

// MARK: - LinkSecret

/// A URL-fragment secret: a share key or a guest-upload key.
///
/// ## Why it is its own type
///
/// The fragment is the decryption key. A browser never sends it to the server,
/// which is the entire reason the share design puts it there — so the client is
/// the only place it can leak from, and it leaks by being *printed*. Wrapping
/// it means the two ways a value accidentally reaches a log — `"\(value)"` and
/// `String(describing:)` — both produce a redaction, and reading the real bytes
/// requires naming ``value`` deliberately.
///
/// It is pointedly **not** `Codable`. ``Route`` is persisted for state
/// restoration; a secret that could be encoded into a route would end up in a
/// restoration file, outliving the revocation it was supposed to respect.
public struct LinkSecret: Sendable, Hashable, CustomStringConvertible, CustomDebugStringConvertible {
    /// The raw fragment. Naming this is the deliberate act; nothing else on the
    /// type exposes it.
    public let value: String

    public init(_ value: String) {
        self.value = value
    }

    public var description: String { Self.redaction }
    public var debugDescription: String { Self.redaction }

    private static let redaction = "LinkSecret(redacted)"
}

// MARK: - DeepLink

/// A parsed inbound URL: where to go, and the secret that came with it.
///
/// Parsing is **total**. Every failure mode — an unknown scheme, a path that is
/// not a UUID, a share link whose fragment a chat app stripped — returns `nil`.
/// The alternative, guessing, means a mistyped link silently opens the wrong
/// album; and trapping means a malformed URL from a third-party app can crash
/// the whole photo library.
///
/// `Hashable` but not `Codable`, for the reason in ``LinkSecret``.
public struct DeepLink: Sendable, Hashable, CustomStringConvertible {
    /// Where the link points.
    public let route: Route
    /// The fragment secret, present only for inbound `https` invitations.
    public let secret: LinkSecret?

    public init(route: Route, secret: LinkSecret? = nil) {
        self.route = route
        self.secret = secret
    }

    /// Redacted by construction: the default reflection-based description would
    /// print the whole struct, and this type exists in the one code path where
    /// that is a key disclosure.
    public var description: String {
        "DeepLink(route: \(route), secret: \(secret == nil ? "none" : "redacted"))"
    }
}

// MARK: - Parsing

public extension DeepLink {
    /// The app's private URL scheme.
    static let scheme = "capsule"

    /// Parse an inbound URL, or `nil` if it names nothing this build knows.
    static func parse(_ url: URL) -> DeepLink? {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false) else {
            return nil
        }
        switch components.scheme?.lowercased() {
        case Self.scheme: return parsePrivate(components)
        case "https": return parseWeb(components)
        default: return nil
        }
    }

    /// The route an inbound URL names, discarding any secret.
    ///
    /// The convenience the shells actually call when they only need to
    /// navigate; anything that has to *decrypt* uses ``parse(_:)``.
    static func route(for url: URL) -> Route? {
        parse(url)?.route
    }

    /// `capsule://album/<uuid>`, `capsule://asset/<uuid>`, `capsule://search?q=…`.
    private static func parsePrivate(_ components: URLComponents) -> DeepLink? {
        let first = components.path.split(separator: "/").first.map(String.init)
        switch components.host?.lowercased() {
        case "album":
            guard let uuid = canonicalUUID(first) else { return nil }
            return DeepLink(route: .album(.managed(uuid: uuid)))
        case "asset":
            guard let uuid = canonicalUUID(first) else { return nil }
            return DeepLink(route: .viewer(.managed(uuid: uuid), context: .library))
        case "search":
            guard let text = queryValue(components, named: "q") else { return nil }
            return DeepLink(route: .search(.all, text: text))
        default:
            return nil
        }
    }

    /// `https://<host>/s/<opaque-id>#<secret>` and its `/u/` guest-upload twin.
    ///
    /// A missing fragment is treated as malformed rather than as a link to a
    /// redemption screen that cannot possibly succeed: without the key there is
    /// nothing to redeem, and saying so at the boundary beats a dead end three
    /// screens later.
    private static func parseWeb(_ components: URLComponents) -> DeepLink? {
        let segments = components.path.split(separator: "/").map(String.init)
        guard segments.count == 2,
              let kind = invitationKinds[segments[0].lowercased()],
              isOpaqueToken(segments[1]),
              let fragment = components.fragment,
              isOpaqueToken(fragment)
        else { return nil }
        return DeepLink(
            route: .linkRedemption(kind, opaqueID: segments[1]),
            secret: LinkSecret(fragment)
        )
    }

    private static let invitationKinds: [String: InvitationKind] = [
        "s": .share,
        "u": .guestUpload,
    ]

    /// Validate a UUID without re-minting it: the canonical text is what the
    /// catalog stores, and round-tripping through `UUID` can re-case it, which
    /// breaks a byte-for-byte comparison downstream.
    private static func canonicalUUID(_ text: String?) -> String? {
        guard let text, UUID(uuidString: text) != nil else { return nil }
        return text
    }

    /// Accept any URL-safe opaque token rather than pinning a length or an
    /// encoding. The share id is deliberately unstructured — 128 random bits —
    /// so asserting "32 hex characters" would encode a server choice this layer
    /// has no business knowing.
    private static func isOpaqueToken(_ text: String) -> Bool {
        !text.isEmpty && text.count <= 128 && text.allSatisfy(\.isOpaqueTokenCharacter)
    }

    private static func queryValue(_ components: URLComponents, named name: String) -> String? {
        guard let items = components.queryItems,
              let value = items.first(where: { $0.name == name })?.value,
              !value.isEmpty
        else { return nil }
        return value
    }
}

private extension Character {
    /// The unreserved URL character set, which is what both the opaque id and
    /// the fragment are drawn from.
    var isOpaqueTokenCharacter: Bool {
        (isASCII && (isLetter || isNumber)) || self == "-" || self == "_"
    }
}
