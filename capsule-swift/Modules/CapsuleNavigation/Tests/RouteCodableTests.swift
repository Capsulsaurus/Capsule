import Foundation
import Testing

import CapsuleDomain
import CapsuleFoundation
import CapsuleNavigation

/// Scene restoration round-trips the whole navigation stack through `Codable`.
/// If a case does not survive that, the symptom is not a crash — it is every
/// relaunch quietly dropping the user at a section root.
@Suite("Routes survive state restoration")
struct RouteCodableTests {
    private static func roundTrip(_ route: Route) throws -> Route {
        let data = try JSONEncoder().encode(route)
        return try JSONDecoder().decode(Route.self, from: data)
    }

    @Test("every route case round-trips unchanged")
    func everyCaseRoundTrips() throws {
        for route in RouteFixtures.routes {
            let restored = try Self.roundTrip(route)
            #expect(restored == route, "\(route)")
        }
    }

    @Test("a fully-populated timeline query keeps every facet")
    func timelineQueryKeepsEveryFacet() throws {
        let route = Route.culling(.timeline(RouteFixtures.fullQuery))

        let restored = try Self.roundTrip(route)

        #expect(restored == route)
        guard case let .culling(.timeline(query)) = restored else {
            Issue.record("the culling context did not survive the round-trip")
            return
        }
        #expect(query.slice == .userHidden)
        #expect(query.albumID == RouteFixtures.albumID)
        #expect(query.mediaKind == .video)
        #expect(query.capturedAfter?.epochSeconds == 1700000000)
        #expect(query.capturedBefore?.epochSeconds == 1800000000)
        #expect(query.cull == .pick)
        #expect(query.minimumRating == 4)
        #expect(query.includeStackHidden)
    }

    @Test("a search scope with several facets keeps all of them")
    func searchScopeKeepsItsFacets() throws {
        let scope: SearchScope = [.semantic, .people, .places]

        let restored = try Self.roundTrip(.search(scope, text: "harbour"))

        #expect(restored == .search(scope, text: "harbour"))
    }

    @Test("a map region keeps all four bounds, in the right corners")
    func mapRegionKeepsItsBounds() throws {
        let restored = try Self.roundTrip(.place(RouteFixtures.region))

        guard case let .place(region) = restored else {
            Issue.record("the region did not survive the round-trip")
            return
        }
        #expect(region.minimumLatitude == RouteFixtures.region.minimumLatitude)
        #expect(region.maximumLatitude == RouteFixtures.region.maximumLatitude)
        #expect(region.minimumLongitude == RouteFixtures.region.minimumLongitude)
        #expect(region.maximumLongitude == RouteFixtures.region.maximumLongitude)
    }

    @Test("a whole section stack round-trips as a unit")
    func aStackRoundTrips() throws {
        let stack: [Route] = [
            .albums,
            .album(RouteFixtures.albumID),
            .viewer(RouteFixtures.assetID, context: .album(RouteFixtures.albumID)),
        ]

        let data = try JSONEncoder().encode(stack)
        let restored = try JSONDecoder().decode([Route].self, from: data)

        #expect(restored == stack)
    }
}
