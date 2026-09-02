import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - Generation

/// The other half of the deep-link contract: turning state back into a URL.
///
/// Generation is deliberately **partial** where parsing is total. Only three
/// destinations have a stable external address, and only assets and albums that
/// live in the Capsule-managed store have one at all — a `PHAsset` local
/// identifier is meaningful on exactly one device, so minting a link to it
/// would produce a URL that is broken everywhere it could usefully be sent.
/// Returning `nil` says that honestly; the alternative is a share sheet that
/// hands someone a dead link.
public extension DeepLink {
    /// A `capsule://` URL for `route`, or `nil` if the destination has no
    /// external address.
    static func url(for route: Route) -> URL? {
        switch route {
        case let .album(.managed(uuid)): privateURL(host: "album", path: uuid)
        case let .viewer(.managed(uuid), _): privateURL(host: "asset", path: uuid)
        case let .search(_, text?): searchURL(text: text)
        default: nil
        }
    }

    /// A share link. The secret goes in the fragment, where no browser will
    /// send it to the server — the property the whole share design rests on.
    static func shareURL(host: String, opaqueID: String, secret: LinkSecret) -> URL? {
        webURL(host: host, kind: .share, opaqueID: opaqueID, secret: secret)
    }

    /// A guest-upload link, with the upload key in the fragment for the same
    /// reason.
    static func uploadURL(host: String, opaqueID: String, key: LinkSecret) -> URL? {
        webURL(host: host, kind: .guestUpload, opaqueID: opaqueID, secret: key)
    }

    private static func privateURL(host: String, path: String) -> URL? {
        var components = URLComponents()
        components.scheme = Self.scheme
        components.host = host
        components.path = "/\(path)"
        return components.url
    }

    private static func searchURL(text: String) -> URL? {
        guard !text.isEmpty else { return nil }
        var components = URLComponents()
        components.scheme = Self.scheme
        components.host = "search"
        components.queryItems = [URLQueryItem(name: "q", value: text)]
        return components.url
    }

    private static func webURL(
        host: String,
        kind: InvitationKind,
        opaqueID: String,
        secret: LinkSecret
    ) -> URL? {
        guard !host.isEmpty, !opaqueID.isEmpty, !secret.value.isEmpty else { return nil }
        var components = URLComponents()
        components.scheme = "https"
        components.host = host
        components.path = "/\(kind.pathPrefix)/\(opaqueID)"
        components.fragment = secret.value
        return components.url
    }
}
