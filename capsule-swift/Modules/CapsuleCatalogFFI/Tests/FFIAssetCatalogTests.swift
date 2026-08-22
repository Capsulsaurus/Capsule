import Foundation
import Testing

@testable import CapsuleCatalog

/// Exercises the real ``CapsuleCatalog`` against an in-memory SQLite database
/// through the Rust UniFFI boundary — the Phase 1 exit criterion that the
/// catalog round-trips from Swift.
@Suite("CapsuleCatalog — Rust FFI round-trip")
struct CapsuleCatalogTests {
    private func makeAsset(
        id: String,
        type: String = "photo",
        captureTimestamp: Int64 = 1_720_000_000,
        hash: String? = nil
    ) -> CatalogAsset {
        CatalogAsset(
            id: id,
            assetType: type,
            captureTimestamp: captureTimestamp,
            importTimestamp: 1_720_000_000,
            hashSHA256: hash ?? String(repeating: id.first.map(String.init) ?? "0", count: 64),
            captureUTC: captureTimestamp,
            captureTimezoneSource: "offset_exif",
            width: 4032,
            height: 3024
        )
    }

    @Test("opens an in-memory catalog migrated to schema v2 or newer")
    func opensAtCurrentSchema() async throws {
        let catalog = try CapsuleCatalog.inMemory()
        let version = try await catalog.schemaVersion()
        #expect(version >= 2)
    }

    @Test("an inserted asset round-trips losslessly through SQLite")
    func assetRoundTrip() async throws {
        let catalog = try CapsuleCatalog.inMemory()
        let asset = makeAsset(id: "asset-1")

        try await catalog.insertAsset(asset)

        let byID = try await catalog.asset(id: "asset-1")
        #expect(byID == asset)

        let byHash = try await catalog.asset(hashSHA256: asset.hashSHA256)
        #expect(byHash?.id == "asset-1")

        let timeline = try await catalog.timeline(offset: 0, limit: 100)
        #expect(timeline == [asset])
    }

    @Test("inserting a duplicate id throws")
    func duplicateInsertThrows() async throws {
        let catalog = try CapsuleCatalog.inMemory()
        try await catalog.insertAsset(makeAsset(id: "dup"))
        await #expect(throws: CatalogError.self) {
            try await catalog.insertAsset(makeAsset(id: "dup"))
        }
    }

    @Test("the timeline is ordered by capture time, newest first")
    func timelineOrdering() async throws {
        let catalog = try CapsuleCatalog.inMemory()
        try await catalog.insertAsset(makeAsset(id: "old", captureTimestamp: 100))
        try await catalog.insertAsset(makeAsset(id: "new", captureTimestamp: 300))
        try await catalog.insertAsset(makeAsset(id: "mid", captureTimestamp: 200))

        let timeline = try await catalog.timeline(offset: 0, limit: 100)
        #expect(timeline.map(\.id) == ["new", "mid", "old"])
    }

    @Test("a windowed timeline page respects offset and limit")
    func timelinePaging() async throws {
        let catalog = try CapsuleCatalog.inMemory()
        for index in 0 ..< 10 {
            try await catalog.insertAsset(makeAsset(id: "a\(index)", captureTimestamp: Int64(index)))
        }
        let page = try await catalog.timeline(offset: 2, limit: 3)
        #expect(page.count == 3)
        // Newest first: index 9,8,7,6,5… so offset 2 → 7,6,5.
        #expect(page.map(\.id) == ["a7", "a6", "a5"])
    }

    @Test("soft delete hides an asset; restore returns it")
    func softDeleteAndRestore() async throws {
        let catalog = try CapsuleCatalog.inMemory()
        try await catalog.insertAsset(makeAsset(id: "trashed"))

        try await catalog.softDeleteAsset(id: "trashed", deletedAt: 1_720_000_500)
        #expect(try await catalog.timeline(offset: 0, limit: 100).isEmpty)

        try await catalog.restoreAsset(id: "trashed")
        #expect(try await catalog.timeline(offset: 0, limit: 100).count == 1)
    }

    @Test("a filtered timeline restricts by catalog asset type")
    func filteredByType() async throws {
        let catalog = try CapsuleCatalog.inMemory()
        try await catalog.insertAsset(makeAsset(id: "photo-1", type: "photo"))
        try await catalog.insertAsset(makeAsset(id: "video-1", type: "video"))

        let videos = try await catalog.timeline(
            filter: TimelineFilter(assetType: "video"),
            offset: 0,
            limit: 100
        )
        #expect(videos.map(\.id) == ["video-1"])
    }

    @Test("a filtered timeline restricts by capture-time window")
    func filteredByWindow() async throws {
        let catalog = try CapsuleCatalog.inMemory()
        try await catalog.insertAsset(makeAsset(id: "before", captureTimestamp: 100))
        try await catalog.insertAsset(makeAsset(id: "inside", captureTimestamp: 200))
        try await catalog.insertAsset(makeAsset(id: "after", captureTimestamp: 300))

        let windowed = try await catalog.timeline(
            filter: TimelineFilter(capturedAfter: 150, capturedBefore: 250),
            offset: 0,
            limit: 100
        )
        #expect(windowed.map(\.id) == ["inside"])
    }

    @Test("album membership: assign, query, and survive album deletion")
    func albumLifecycle() async throws {
        let catalog = try CapsuleCatalog.inMemory()
        let album = CatalogAlbum(id: "album-1", name: "Trip", createdAt: 1_720_000_000, modifiedAt: 1_720_000_000)
        try await catalog.insertAlbum(album)
        try await catalog.insertAsset(makeAsset(id: "asset-1"))
        try await catalog.setAssetAlbum(assetID: "asset-1", albumID: "album-1")

        let members = try await catalog.albumAssets(albumID: "album-1", offset: 0, limit: 100)
        #expect(members.map(\.id) == ["asset-1"])
        #expect(try await catalog.albums().count == 1)

        try await catalog.deleteAlbum(id: "album-1")
        #expect(try await catalog.album(id: "album-1") == nil)
        // The asset itself outlives its album.
        #expect(try await catalog.asset(id: "asset-1") != nil)
    }
}
