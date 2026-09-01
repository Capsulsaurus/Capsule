import Foundation
import Testing

import CapsuleDomain
import CapsuleFoundation
import CapsuleNavigation

/// Parsing is the app's most exposed surface: any app on the device can hand it
/// a URL. It has to be total.
@Suite("Deep links parse every supported form")
struct DeepLinkParsingTests {
    private static let uuid = "0192f0c0-1111-7000-8000-000000000001"

    @Test("capsule://album/<uuid> opens the album")
    func albumLink() throws {
        let url = try #require(URL(string: "capsule://album/\(Self.uuid)"))
        #expect(DeepLink.route(for: url) == .album(.managed(uuid: Self.uuid)))
    }

    @Test("capsule://asset/<uuid> opens the viewer on the whole library")
    func assetLink() throws {
        let url = try #require(URL(string: "capsule://asset/\(Self.uuid)"))
        #expect(DeepLink.route(for: url) == .viewer(.managed(uuid: Self.uuid), context: .library))
    }

    @Test("capsule://search?q= carries the query text")
    func searchLink() throws {
        let url = try #require(URL(string: "capsule://search?q=golden%20hour"))
        #expect(DeepLink.route(for: url) == .search(.all, text: "golden hour"))
    }

    @Test("a share link yields a redemption route and its fragment secret")
    func shareLink() throws {
        let url = try #require(URL(string: "https://capsule.example/s/abc123#deadbeef"))
        let link = try #require(DeepLink.parse(url))

        #expect(link.route == .linkRedemption(.share, opaqueID: "abc123"))
        #expect(link.secret?.value == "deadbeef")
    }

    @Test("a guest-upload link is the same shape, filed under Drops")
    func guestUploadLink() throws {
        let url = try #require(URL(string: "https://capsule.example/u/xyz789#cafebabe"))
        let link = try #require(DeepLink.parse(url))

        #expect(link.route == .linkRedemption(.guestUpload, opaqueID: "xyz789"))
        #expect(link.route.owningSection == .drops)
        #expect(link.secret?.value == "cafebabe")
    }

    @Test("malformed and unknown URLs return nil rather than guessing")
    func malformedInputIsRejected() {
        let rejected = [
            "capsule://album/not-a-uuid",
            "capsule://album/",
            "capsule://album",
            "capsule://asset/12345",
            "capsule://search",
            "capsule://search?q=",
            "capsule://sasquatch/\(Self.uuid)",
            "https://capsule.example/s/abc123",
            "https://capsule.example/s/#secret",
            "https://capsule.example/x/abc123#secret",
            "https://capsule.example/s/abc/123#secret",
            "https://capsule.example/s/abc 123#secret",
            "http://capsule.example/s/abc123#secret",
            "mailto:someone@example.com",
            "capsule://",
        ]
        for text in rejected {
            guard let url = URL(string: text) else { continue }
            #expect(DeepLink.parse(url) == nil, "should not have parsed: \(text)")
        }
    }
}

@Suite("Deep links generate the forms they parse")
struct DeepLinkGenerationTests {
    @Test("album, asset, and search links round-trip through generation")
    func generatedLinksReparse() throws {
        let routes: [Route] = [
            .album(RouteFixtures.albumID),
            .viewer(RouteFixtures.assetID, context: .library),
            .search(.all, text: "beach"),
        ]
        for route in routes {
            let url = try #require(DeepLink.url(for: route), "\(route) should be linkable")
            #expect(DeepLink.route(for: url) == route)
        }
    }

    @Test("device-local assets have no external address, so no link is minted")
    func photoKitIdentifiersAreNotLinkable() {
        #expect(DeepLink.url(for: .viewer(RouteFixtures.photoKitAsset, context: .library)) == nil)
        #expect(DeepLink.url(for: .album(.smart(localIdentifier: "SMART/1"))) == nil)
    }

    @Test("destinations with no external address return nil")
    func unlinkableRoutesReturnNil() {
        #expect(DeepLink.url(for: .settings(.security)) == nil)
        #expect(DeepLink.url(for: .search(.all, text: nil)) == nil)
        #expect(DeepLink.url(for: .federation) == nil)
    }

    @Test("share and upload links put the secret in the fragment, and parse back")
    func webLinksRoundTrip() throws {
        let secret = LinkSecret("f00dfeed")
        let share = try #require(
            DeepLink.shareURL(host: "capsule.example", opaqueID: "abc123", secret: secret)
        )
        #expect(share.absoluteString == "https://capsule.example/s/abc123#f00dfeed")
        #expect(DeepLink.parse(share)?.secret == secret)

        let upload = try #require(
            DeepLink.uploadURL(host: "capsule.example", opaqueID: "abc123", key: secret)
        )
        #expect(upload.absoluteString == "https://capsule.example/u/abc123#f00dfeed")
    }
}

/// The fragment is a decryption key. The only way it leaks from a client is by
/// being printed, so both description paths are pinned.
@Suite("The share secret is never printable")
struct LinkSecretRedactionTests {
    private static let secret = "s3cr3tk3y"

    @Test("a secret redacts in both description forms")
    func secretRedacts() {
        let secret = LinkSecret(Self.secret)

        #expect(!"\(secret)".contains(Self.secret))
        #expect(!String(reflecting: secret).contains(Self.secret))
        #expect(secret.value == Self.secret)
    }

    @Test("a parsed link redacts too, so logging the link is safe")
    func parsedLinkRedacts() throws {
        let url = try #require(URL(string: "https://capsule.example/s/abc123#\(Self.secret)"))
        let link = try #require(DeepLink.parse(url))

        #expect(!"\(link)".contains(Self.secret))
        #expect(!String(reflecting: link).contains(Self.secret))
    }

    @Test("the route a link produces carries no secret, so restoration cannot")
    func routesCarryNoSecret() throws {
        let url = try #require(URL(string: "https://capsule.example/s/abc123#\(Self.secret)"))
        let route = try #require(DeepLink.route(for: url))
        let encoded = try JSONEncoder().encode(route)
        let json = try #require(String(bytes: encoded, encoding: .utf8))

        #expect(!json.contains(Self.secret))
    }
}

@Suite("Opening a URL navigates")
@MainActor
struct RouterDeepLinkTests {
    @Test("an inbound link selects the owning section and hands back the secret")
    func openNavigatesAndReturnsTheLink() throws {
        // The split shell: a link's *owning section* is the thing under test,
        // and on a phone a non-tab section is hosted by Browse instead.
        let router = Router(shell: .split)
        let url = try #require(URL(string: "https://capsule.example/s/abc123#deadbeef"))

        let link = try #require(router.open(url))

        #expect(router.selection == .shares)
        #expect(link.secret?.value == "deadbeef")
    }

    @Test("an unrecognised URL leaves navigation exactly where it was")
    func unknownURLsDoNotNavigate() throws {
        let router = Router(shell: .split)
        router.select(.person(RouteFixtures.personID))
        let url = try #require(URL(string: "capsule://nonsense/1"))

        #expect(router.open(url) == nil)
        #expect(router.selection == .people)
        #expect(router.path == [.person(RouteFixtures.personID)])
    }
}
