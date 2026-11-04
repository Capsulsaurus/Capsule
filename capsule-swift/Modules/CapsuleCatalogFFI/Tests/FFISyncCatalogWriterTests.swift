import Foundation
import Testing

import CapsuleCatalog
@testable import CapsuleCatalogFFI

private let assetID = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a7a"
private let albumID = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a7b"
/// The plaintext every staged original holds, and its SHA-256.
private let plaintext = Data("synced original".utf8)
private let plaintextSHA256 = "d89abe1259be9a82c19f531987a0df135e748afaec8453b8147e8e03bad0fcd3"
/// `2026-01-01T00:00:00Z`, the capture instant the metadata carries.
private let captureSeconds: Int64 = 1767225600
/// `2026-02-03T04:05:06Z`, the instant the lifecycle records are authored at.
private let authoredSeconds: Int64 = 1770091506
/// The instant the writer's clock reads.
private let importedAt = Date(timeIntervalSince1970: 1780000000)

private func metadata(
    contentType: String = "image/jpeg",
    caption: String? = "Harbour at dusk",
    rating: UInt8? = 4,
    hidden: Bool = false
) -> FfiAssetMetadata {
    FfiAssetMetadata(
        sidecarSchema: 1,
        contentType: contentType,
        captureTimestamp: "2026-01-01T00:00:00Z",
        width: 4032,
        height: 3024,
        lqipChromahash: Data([1, 2, 3]),
        lqipDominantColor: Data([10, 20, 30]),
        caption: caption,
        tags: ["boats"],
        rating: rating,
        hidden: hidden
    )
}

private func facts(action: String, metadata: FfiAssetMetadata?) -> FfiAssetFacts {
    FfiAssetFacts(
        assetId: assetID,
        albumId: albumID,
        amkVersion: 1,
        ciphertextHash: String(repeating: "a", count: 64),
        plaintextSize: UInt64(plaintext.count),
        action: action,
        timestamp: "2026-02-03T04:05:06Z",
        createdByDevice: albumID,
        provenanceHead: Data(repeating: 7, count: 32),
        metadata: metadata
    )
}

/// A store root in a fresh temporary directory: originals under
/// `media/{YYYY}/{YYYY-MM}`, staged plaintexts under `staging/`.
private struct StoreRoot {
    let root = URL.temporaryDirectory.appending(path: "ffi-sync-catalog-\(UUID().uuidString)", directoryHint: .isDirectory)

    var media: URL {
        root.appending(path: "media", directoryHint: .isDirectory)
    }

    /// The partition the writer keeps the original captured at `date` in.
    func partition(_ date: Date) -> URL {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = .gmt
        let parts = calendar.dateComponents([.year, .month], from: date)
        let year = String(format: "%04d", parts.year ?? 0)
        return media
            .appending(path: year, directoryHint: .isDirectory)
            .appending(path: String(format: "%@-%02d", year, parts.month ?? 0), directoryHint: .isDirectory)
    }

    var capturePartition: URL {
        partition(Date(timeIntervalSince1970: TimeInterval(captureSeconds)))
    }

    /// A staged plaintext, as `openSyncedOriginal` leaves one.
    func stage() throws -> URL {
        let staging = root.appending(path: "staging", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)
        let url = staging.appending(path: UUID().uuidString)
        try plaintext.write(to: url)
        return url
    }

    func files(in directory: URL) throws -> [String] {
        guard FileManager.default.fileExists(atPath: directory.path) else { return [] }
        return try FileManager.default.contentsOfDirectory(atPath: directory.path).sorted()
    }
}

private func writer(_ catalog: FFIAssetCatalog, _ store: StoreRoot) -> FFISyncCatalogWriter {
    let partition: @Sendable (Date) -> URL = { store.partition($0) }
    return FFISyncCatalogWriter(openingCatalog: { catalog }, mediaDirectory: partition, now: { importedAt })
}

/// A row the store already holds for the asset.
private func heldRow(isDeleted: Bool = false) -> CatalogAsset {
    CatalogAsset(
        id: assetID,
        assetType: "photo",
        captureTimestamp: captureSeconds,
        importTimestamp: 1700000000,
        hashSHA256: plaintextSHA256,
        captureUTC: captureSeconds,
        albumID: albumID,
        rating: 1,
        isDeleted: isDeleted,
        deletedAt: isDeleted ? 1700000100 : nil,
        caption: "old caption",
        latitude: 51.5,
        longitude: -0.12
    )
}

@Suite("FFISyncCatalogWriter — applied sync entries into the managed catalog")
struct FFISyncCatalogWriterTests {
    @Test("an applied create upserts a row with the facts' fields and moves the verified original into its partition")
    func createUpsertsARow() async throws {
        let catalog = try FFIAssetCatalog.inMemory()
        let store = StoreRoot()
        let staged = try store.stage()

        let write = try await writer(catalog, store).write(facts(action: "create", metadata: metadata()), original: staged)

        #expect(write == .upserted(placedOriginal: true))
        let row = try #require(try await catalog.asset(id: assetID))
        #expect(row.assetType == "photo")
        #expect(row.captureTimestamp == captureSeconds)
        #expect(row.captureUTC == captureSeconds)
        #expect(row.importTimestamp == Int64(importedAt.timeIntervalSince1970))
        #expect(row.hashSHA256 == plaintextSHA256)
        #expect(row.width == 4032)
        #expect(row.height == 3024)
        #expect(row.albumID == albumID)
        #expect(row.rating == 4)
        #expect(row.caption == "Harbour at dusk")
        #expect(!row.isHidden)
        #expect(!row.isDeleted)
        #expect(row.chromahash == nil)
        let stem = assetID.replacingOccurrences(of: "-", with: "")
        #expect(try store.files(in: store.capturePartition) == ["\(stem).jpeg"])
        #expect(try Data(contentsOf: store.capturePartition.appending(path: "\(stem).jpeg")) == plaintext)
        #expect(!FileManager.default.fileExists(atPath: staged.path))
        #expect(try await catalog.timeline(filter: .all, offset: 0, limit: 10).map(\.id) == [assetID])
    }

    @Test("an update over a held row keeps what its facts do not carry, and keeps the original already held")
    func updateKeepsTheHeldRowAndOriginal() async throws {
        let catalog = try FFIAssetCatalog.inMemory()
        let store = StoreRoot()
        try await catalog.upsertAsset(heldRow(isDeleted: true))
        try FileManager.default.createDirectory(at: store.capturePartition, withIntermediateDirectories: true)
        let held = store.capturePartition.appending(path: assetID.replacingOccurrences(of: "-", with: "") + ".jpg")
        try Data("held original".utf8).write(to: held)
        let staged = try store.stage()

        let write = try await writer(catalog, store).write(
            facts(action: "metadata-update", metadata: metadata(caption: "", rating: nil, hidden: true)),
            original: staged
        )

        #expect(write == .upserted(placedOriginal: false))
        let row = try #require(try await catalog.asset(id: assetID))
        #expect(row.isDeleted)
        #expect(row.deletedAt == 1700000100)
        #expect(row.importTimestamp == 1700000000)
        #expect(row.latitude == 51.5)
        #expect(row.caption == nil)
        #expect(row.rating == 0)
        #expect(row.isHidden)
        #expect(try Data(contentsOf: held) == Data("held original".utf8))
        #expect(try store.files(in: store.capturePartition).count == 1)
        #expect(FileManager.default.fileExists(atPath: staged.path))
    }

    @Test("an applied delete moves the held row into the trash, and a trash-restore moves it out")
    func deleteAndRestoreMoveTheRow() async throws {
        let catalog = try FFIAssetCatalog.inMemory()
        let store = StoreRoot()
        try await catalog.upsertAsset(heldRow())
        let subject = writer(catalog, store)

        #expect(try await subject.write(facts(action: "delete", metadata: nil), original: nil) == .trashed)

        let trashed = try #require(try await catalog.asset(id: assetID))
        #expect(trashed.isDeleted)
        #expect(trashed.deletedAt == authoredSeconds)
        #expect(trashed.caption == "old caption")
        #expect(try await catalog.timeline(filter: .all, offset: 0, limit: 10).isEmpty)

        #expect(try await subject.write(facts(action: "trash-restore", metadata: nil), original: nil) == .restored)

        let restored = try #require(try await catalog.asset(id: assetID))
        #expect(!restored.isDeleted)
        #expect(restored.deletedAt == nil)
        #expect(try await catalog.timeline(filter: .all, offset: 0, limit: 10).map(\.id) == [assetID])
    }

    @Test(
        "a lifecycle entry for an asset with no row, or an action with no metadata it does not project, writes nothing",
        arguments: ["delete", "trash-restore", "purge"]
    )
    func lifecycleWithoutARowWritesNothing(action: String) async throws {
        let catalog = try FFIAssetCatalog.inMemory()

        let write = try await writer(catalog, StoreRoot()).write(facts(action: action, metadata: nil), original: nil)

        #expect(write == .unwritten)
        #expect(try await catalog.asset(id: assetID) == nil)
    }

    @Test("metadata with neither an original nor a held row writes nothing")
    func metadataWithoutAnOriginalOrRowWritesNothing() async throws {
        let catalog = try FFIAssetCatalog.inMemory()

        let write = try await writer(catalog, StoreRoot()).write(facts(action: "create", metadata: metadata()), original: nil)

        #expect(write == .unwritten)
        #expect(try await catalog.asset(id: assetID) == nil)
    }

    @Test("a video is a video row, and a MIME type with no known extension is stored under the fallback")
    func typesAndExtensions() {
        #expect(FFISyncCatalogWriter.assetType("video/quicktime") == "video")
        #expect(FFISyncCatalogWriter.assetType("image/heic") == "photo")
        #expect(FFISyncCatalogWriter.fileExtension("image/jpeg") == "jpeg")
        #expect(FFISyncCatalogWriter.fileExtension("video/quicktime") == "mov")
        #expect(FFISyncCatalogWriter.fileExtension("application/x-capsule-unknown") == FFISyncCatalogWriter.fallbackExtension)
        #expect(FFISyncCatalogWriter.fileStem("0190A1B2-C3D4-7E5F-8A9B-0C1D2E3F4A7A") == "0190a1b2c3d47e5f8a9b0c1d2e3f4a7a")
    }

    @Test("an unreadable capture timestamp is indexed as the epoch, as the workspace indexes one")
    func unreadableTimestampIsTheEpoch() {
        #expect(FFISyncCatalogWriter.seconds("not a time", field: "capture_timestamp", assetID: assetID) == 0)
        #expect(FFISyncCatalogWriter.seconds("2026-01-01T00:00:00Z", field: "capture_timestamp", assetID: assetID) == captureSeconds)
    }
}
