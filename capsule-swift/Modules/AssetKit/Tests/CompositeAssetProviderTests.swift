import Foundation
import Testing

import AssetKit
import CapsuleTestSupport

@Suite("CompositeAssetProvider")
struct CompositeAssetProviderTests {
    @Test("merges providers into one timeline, newest first")
    func mergesChronologically() async throws {
        let older = Fixtures.asset(captureDate: Date(timeIntervalSince1970: 1_000))
        let newer = Fixtures.asset(captureDate: Date(timeIntervalSince1970: 5_000))
        let composite = CompositeAssetProvider(providers: [
            MockAssetProvider(assets: [older]),
            MockAssetProvider(assets: [newer]),
        ])

        let snapshot = try await composite.loadTimeline()

        #expect(snapshot.count == 2)
        #expect(snapshot.asset(at: 0).captureDate > snapshot.asset(at: 1).captureDate)
    }

    @Test("skips a provider that fails to load, keeping the rest")
    func skipsFailingProvider() async throws {
        let composite = CompositeAssetProvider(providers: [
            MockAssetProvider(assets: [], status: .denied),
            MockAssetProvider(assets: Fixtures.assets(count: 3)),
        ])

        let snapshot = try await composite.loadTimeline()

        #expect(snapshot.count == 3)
    }

    @Test("authorization is usable when any provider is usable")
    func authorizationIsUnion() async {
        let composite = CompositeAssetProvider(providers: [
            MockAssetProvider(assets: [], status: .denied),
            MockAssetProvider(assets: [], status: .authorized),
        ])
        #expect(await composite.authorizationStatus() == .authorized)
    }

    @Test("asset lookup resolves through whichever provider owns the id")
    func resolvesAssetByID() async throws {
        let target = Fixtures.asset(id: .managed(uuid: "owned"))
        let composite = CompositeAssetProvider(providers: [
            MockAssetProvider(assets: Fixtures.assets(count: 2)),
            MockAssetProvider(assets: [target]),
        ])
        let found = try await composite.asset(for: .managed(uuid: "owned"))
        #expect(found == target)
    }
}
