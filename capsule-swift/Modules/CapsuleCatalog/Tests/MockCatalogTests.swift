import Foundation
import Testing

import CapsuleCatalog
import CapsuleTestSupport

/// Verifies that `MockCatalog` is a *faithful* ``AssetCatalog`` — it must obey
/// the same documented semantics as the real catalog, or every consumer test
/// built on it is testing against a fiction.
@Suite("MockCatalog conforms to the AssetCatalog contract")
struct MockCatalogTests {
    @Test("the timeline is newest-first and excludes soft-deleted assets")
    func timelineOrderAndDeletion() async throws {
        let catalog = MockCatalog()
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "old", captureTimestamp: 100))
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "new", captureTimestamp: 300))
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "gone", captureTimestamp: 200))
        await catalog.softDeleteAsset(id: "gone", deletedAt: 250)

        let timeline = await catalog.timeline(filter: .all, offset: 0, limit: 100)
        #expect(timeline.map(\.id) == ["new", "old"])
    }

    @Test("stack-hidden assets are excluded from the timeline")
    func stackHiddenExcluded() async throws {
        let catalog = MockCatalog()
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "shown"))
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "hidden", isStackHidden: true))

        let timeline = await catalog.timeline(filter: .all, offset: 0, limit: 100)
        #expect(timeline.map(\.id) == ["shown"])
    }

    @Test("a duplicate insert throws, matching the real catalog")
    func duplicateInsertThrows() async throws {
        let catalog = MockCatalog()
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "x"))
        await #expect(throws: (any Error).self) {
            try await catalog.insertAsset(Fixtures.catalogAsset(id: "x"))
        }
    }

    @Test("a type filter restricts the timeline")
    func typeFilter() async throws {
        let catalog = MockCatalog()
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "p", assetType: "photo"))
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "v", assetType: "video"))

        let videos = await catalog.timeline(
            filter: TimelineFilter(assetType: "video"),
            offset: 0,
            limit: 100
        )
        #expect(videos.map(\.id) == ["v"])
    }

    @Test("deleting an album clears membership but keeps the assets")
    func albumDeletionClearsMembership() async throws {
        let catalog = MockCatalog()
        try await catalog.insertAlbum(Fixtures.catalogAlbum(id: "alb"))
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "member"))
        await catalog.setAssetAlbum(assetID: "member", albumID: "alb")
        #expect(await catalog.albumAssets(albumID: "alb", offset: 0, limit: 10).count == 1)

        await catalog.deleteAlbum(id: "alb")
        let asset = await catalog.asset(id: "member")
        #expect(asset != nil)
        #expect(asset?.albumID == nil)
    }

    @Test("expiredTrash selects only assets deleted before the cutoff")
    func expiredTrashCutoff() async throws {
        let catalog = MockCatalog()
        await catalog.setNow(10_000)
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "fresh"))
        try await catalog.insertAsset(Fixtures.catalogAsset(id: "stale"))
        await catalog.softDeleteAsset(id: "fresh", deletedAt: 9_000)
        await catalog.softDeleteAsset(id: "stale", deletedAt: 1_000)

        let expired = await catalog.expiredTrash(olderThanSeconds: 5_000)
        #expect(expired.map(\.id) == ["stale"])
    }
}
