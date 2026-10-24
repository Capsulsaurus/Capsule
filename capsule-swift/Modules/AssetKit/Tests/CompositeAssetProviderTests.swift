import Foundation
import Testing

import AssetKit
import CapsuleFoundation
import CapsuleTestSupport

@Suite("CompositeAssetProvider")
struct CompositeAssetProviderTests {
    @Test("merges providers into one timeline, newest first")
    func mergesChronologically() async throws {
        let older = Fixtures.asset(captureDate: Date(timeIntervalSince1970: 1000))
        let newer = Fixtures.asset(captureDate: Date(timeIntervalSince1970: 5000))
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

    @Test("passes the only non-empty snapshot through without copying it")
    func passesSoleSnapshotThrough() async throws {
        let sentinel = SentinelSnapshot(assets: Fixtures.assets(count: 3))
        let composite = CompositeAssetProvider(providers: [
            SnapshotProvider(snapshot: sentinel),
            MockAssetProvider(assets: []),
        ])

        let snapshot = try await composite.loadTimeline()

        #expect((snapshot as? SentinelSnapshot) === sentinel)
    }

    @Test("an all-empty composite yields an empty timeline")
    func allEmptyIsEmpty() async throws {
        let composite = CompositeAssetProvider(providers: [
            MockAssetProvider(assets: []),
            MockAssetProvider(assets: []),
        ])

        let snapshot = try await composite.loadTimeline()

        #expect(snapshot.isEmpty)
    }
}

// MARK: - Fixtures

/// A snapshot with reference identity, so a test can tell the original from a copy.
private final class SentinelSnapshot: AssetSnapshot {
    private let assets: [Asset]

    init(assets: [Asset]) {
        self.assets = assets
    }

    var count: Int { assets.count }

    func asset(at index: Int) -> Asset { assets[index] }
}

/// A provider that answers one fixed snapshot.
private struct SnapshotProvider: AssetProvider {
    let snapshot: SentinelSnapshot

    func authorizationStatus() async -> AssetAuthorizationStatus { .authorized }

    func requestAuthorization() async -> AssetAuthorizationStatus { .authorized }

    func loadTimeline() async throws -> any AssetSnapshot { snapshot }

    func asset(for _: AssetID) async throws -> Asset? { nil }

    func changes() -> AsyncStream<AssetChange> { AsyncStream { $0.finish() } }

    func setFavorite(_: Bool, for _: AssetID) async throws {}

    func delete(_: [AssetID]) async throws {}
}
