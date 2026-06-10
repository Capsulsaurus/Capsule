import Foundation
import Testing

import AssetKit
import CapsuleFoundation
import CapsuleTestSupport

@Suite("Asset value semantics")
struct AssetTests {
    @Test("aspectRatio derives from dimensions and falls back to square")
    func aspectRatio() {
        #expect(Fixtures.asset(pixelWidth: 4000, pixelHeight: 2000).aspectRatio == 2.0)
        #expect(Fixtures.asset(pixelWidth: 3000, pixelHeight: 4000).aspectRatio == 0.75)
        #expect(Fixtures.asset(pixelWidth: 0, pixelHeight: 0).aspectRatio == 1.0)
    }

    @Test("Asset round-trips through Codable")
    func codable() throws {
        let asset = Fixtures.asset(
            id: .photoKit(localIdentifier: "PK/L0/001"),
            mediaType: .livePhoto,
            duration: 2.5,
            isFavorite: true
        )
        let decoded = try JSONDecoder().decode(Asset.self, from: JSONEncoder().encode(asset))
        #expect(decoded == asset)
    }

    @Test("source predicates follow the identifier")
    func sourcePredicates() {
        #expect(Fixtures.asset(id: .photoKit(localIdentifier: "x")).isFromPhotoKit)
        #expect(Fixtures.asset(id: .managed(uuid: "y")).isManaged)
    }
}

@Suite("AssetSnapshot")
struct AssetSnapshotTests {
    @Test("InMemoryAssetSnapshot indexes assets and reports emptiness")
    func indexing() {
        let assets = Fixtures.assets(count: 3)
        let snapshot = InMemoryAssetSnapshot(assets)
        #expect(snapshot.count == 3)
        #expect(snapshot.isEmpty == false)
        #expect(snapshot.asset(at: 1) == assets[1])
        #expect(snapshot.assetIfPresent(at: 2) == assets[2])
        #expect(snapshot.assetIfPresent(at: 9) == nil)
        #expect(InMemoryAssetSnapshot([]).isEmpty)
    }
}

@Suite("AssetAuthorizationStatus")
struct AssetAuthorizationStatusTests {
    @Test("isUsable covers both full and limited access")
    func usability() {
        #expect(AssetAuthorizationStatus.authorized.isUsable)
        #expect(AssetAuthorizationStatus.limited.isUsable)
        #expect(AssetAuthorizationStatus.denied.isUsable == false)
        #expect(AssetAuthorizationStatus.notDetermined.isUsable == false)
        #expect(AssetAuthorizationStatus.restricted.isUsable == false)
    }
}

@Suite("MockAssetProvider conforms to the AssetProvider contract")
struct MockAssetProviderTests {
    @Test("serves a timeline snapshot when authorized")
    func servesTimeline() async throws {
        let provider = MockAssetProvider(assets: Fixtures.assets(count: 5))
        let snapshot = try await provider.loadTimeline()
        #expect(snapshot.count == 5)
    }

    @Test("denies the timeline until access is granted")
    func authorizationGate() async throws {
        let provider = MockAssetProvider(assets: Fixtures.assets(count: 2), status: .notDetermined)
        await #expect(throws: (any Error).self) {
            _ = try await provider.loadTimeline()
        }
        #expect(await provider.requestAuthorization() == .authorized)
        #expect(try await provider.loadTimeline().count == 2)
    }

    @Test("resolves an asset by identifier")
    func resolvesByID() async {
        let target = Fixtures.asset(id: .managed(uuid: "find-me"))
        let provider = MockAssetProvider(assets: [target])
        #expect(await provider.asset(for: .managed(uuid: "find-me")) == target)
        #expect(await provider.asset(for: .managed(uuid: "missing")) == nil)
    }

    @Test("emits a reload on the change stream when assets change")
    func emitsChanges() async throws {
        let provider = MockAssetProvider(assets: [])
        var iterator = provider.changes().makeAsyncIterator()
        // Allow the stream's observer to attach before emitting.
        try await Task.sleep(for: .milliseconds(20))
        await provider.setAssets(Fixtures.assets(count: 4))
        let change = await iterator.next()
        #expect(change?.snapshot.count == 4)
        #expect(change?.isIncremental == false)
    }
}
