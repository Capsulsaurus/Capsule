import AssetKit
import CapsuleFoundation
import CapsuleMock
import CoreGraphics
import FeatureTimeline
import Foundation
import ImageIO
import ImagePipeline
import ManagedStore
import Testing
import UniformTypeIdentifiers

@testable import Capsule

/// What the picker imports, the timeline shows.
///
/// The importer writes the on-disk managed store and the timeline reads the
/// port-backed mock library, so the composition root has to join the two. These
/// tests pin that join: the timeline is composed over the same `ManagedProvider`
/// the importer refreshes, and a file imported through it appears in the next
/// timeline read.
@MainActor
@Suite("Managed lane composition")
struct ManagedLaneTests {
    @Test("a picked file appears in the composed timeline once its import completes")
    func importedAssetAppearsInTimeline() async throws {
        let root = Self.temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let mock = MockEnvironment(scenario: .emptyLibrary)
        let lane = AppEnvironment.makeManagedLane(root: root.appending(path: "Library", directoryHint: .isDirectory))
        let timeline = AppEnvironment.composeTimeline(
            library: PortBackedAssetProvider(library: mock.library, organize: mock.organize),
            managed: lane.provider
        )
        let before = try await timeline.loadTimeline().count

        let picked = try Self.pickedFile(in: root)
        await lane.importer.importPicked([picked])

        let imported = try #require(lane.importer.lastResult?.importedAssetIDs.first)
        let after = try await timeline.loadTimeline()
        #expect(after.count == before + 1)
        let ids = (0 ..< after.count).map { after.asset(at: $0).id }
        #expect(ids.contains(.managed(uuid: imported)))
        #expect(try await timeline.asset(for: .managed(uuid: imported)) != nil)
    }

    /// The timeline screen updates only through `changes()`, so this is the
    /// path that has to carry an import. It holds only while the importer and
    /// the timeline share one `ManagedProvider`: with two, the importer's
    /// refresh reaches an observer nobody subscribed, no reload names the
    /// import, and the time limit fails the test.
    @Test(
        "an import reaches a timeline already subscribed to the composed change stream",
        .timeLimit(.minutes(1))
    )
    func importReachesSubscribedTimeline() async throws {
        let root = Self.temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let mock = MockEnvironment(scenario: .emptyLibrary)
        let lane = AppEnvironment.makeManagedLane(root: root.appending(path: "Library", directoryHint: .isDirectory))
        let timeline = AppEnvironment.composeTimeline(
            library: PortBackedAssetProvider(library: mock.library, organize: mock.organize),
            managed: lane.provider
        )
        let picked = try Self.pickedFile(in: root)
        let changes = timeline.changes()

        // The composite subscribes on a task of its own and the provider
        // registers its observer on its actor, so a refresh sent now could land
        // before anyone listens. Refresh until one reload proves the stream is
        // live, and finish refreshing before importing, so every later reload
        // was published after the import.
        let provider = lane.provider
        let probe = Task {
            while !Task.isCancelled {
                await provider.refresh()
                await Task.yield()
            }
        }
        var imported: AssetID?
        var reachedTimeline = false
        for await change in changes {
            guard let importedID = imported else {
                probe.cancel()
                await probe.value
                await lane.importer.importPicked([picked])
                let uuid = try #require(lane.importer.lastResult?.importedAssetIDs.first)
                imported = .managed(uuid: uuid)
                continue
            }
            let ids = (0 ..< change.snapshot.count).map { change.snapshot.asset(at: $0).id }
            if ids.contains(importedID) {
                reachedTimeline = true
                break
            }
        }
        probe.cancel()
        #expect(reachedTimeline)
    }

    @Test("an import is merged into a populated library, newest first")
    func importedAssetMergesWithLibrary() async throws {
        let root = Self.temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let mock = MockEnvironment(scenario: .healthy)
        let library = PortBackedAssetProvider(library: mock.library, organize: mock.organize)
        let lane = AppEnvironment.makeManagedLane(root: root.appending(path: "Library", directoryHint: .isDirectory))
        let timeline = AppEnvironment.composeTimeline(library: library, managed: lane.provider)
        let libraryCount = try await library.loadTimeline().count

        let picked = try Self.pickedFile(in: root)
        await lane.importer.importPicked([picked])

        let imported = try #require(lane.importer.lastResult?.importedAssetIDs.first)
        let after = try await timeline.loadTimeline()
        #expect(after.count == libraryCount + 1)
        // Captured at import time (the bytes carry no EXIF date), so newer than
        // every mock asset, which the mock's reference clock dates no later
        // than August 2026.
        #expect(after.asset(at: 0).id == .managed(uuid: imported))
    }

    @Test("with nothing imported the library's own snapshot is served unchanged")
    func emptyManagedStorePassesLibraryThrough() async throws {
        let root = Self.temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let mock = MockEnvironment(scenario: .healthy)
        let library = PortBackedAssetProvider(library: mock.library, organize: mock.organize)
        let lane = AppEnvironment.makeManagedLane(root: root.appending(path: "Library", directoryHint: .isDirectory))
        let timeline = AppEnvironment.composeTimeline(library: library, managed: lane.provider)

        let snapshot = try await timeline.loadTimeline()

        #expect(snapshot is PagedLibrarySnapshot)
    }

    // MARK: - Every surface an import reaches

    @Test("an imported photo renders in the grid and opens to pixels in the viewer")
    func importedPhotoRenders() async throws {
        let root = Self.temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let mock = MockEnvironment(scenario: .healthy)
        let library = PortBackedAssetProvider(library: mock.library, organize: mock.organize)
        let lane = AppEnvironment.makeManagedLane(root: root.appending(path: "Library", directoryHint: .isDirectory))
        let thumbnails = AppEnvironment.composeThumbnails(
            library: PortBackedThumbnailProvider(renderer: mock.thumbnails),
            managed: lane
        )
        let imported = try await Self.importPhoto(through: lane, in: root)

        let tile = await thumbnails.thumbnail(for: imported, pixelSize: CGSize(width: 128, height: 128))
        #expect(tile != nil)
        let viewer = ViewerMediaLoader(fallback: thumbnails)
        #expect(await viewer.fullImage(for: imported, targetSize: CGSize(width: 1024, height: 1024)) != nil)

        // The mock library still renders through the same composite.
        let first = try await library.loadTimeline().asset(at: 0)
        #expect(await thumbnails.thumbnail(for: first, pixelSize: CGSize(width: 128, height: 128)) != nil)
    }

    @Test("a deleted import is listed in Recently Deleted, restorable and purgeable")
    func deletedImportReachesTrash() async throws {
        let root = Self.temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let mock = MockEnvironment(scenario: .emptyLibrary)
        let lane = AppEnvironment.makeManagedLane(root: root.appending(path: "Library", directoryHint: .isDirectory))
        let timeline = AppEnvironment.composeTimeline(
            library: PortBackedAssetProvider(library: mock.library, organize: mock.organize),
            managed: lane.provider
        )
        let trash = AppEnvironment.composeTrash(
            library: PortBackedTrashProvider(
                organize: mock.organize,
                library: mock.library,
                authenticator: MockLocalAuthenticator()
            ),
            managed: lane
        )
        let imported = try await Self.importPhoto(through: lane, in: root).id

        try await timeline.delete([imported])
        try await trash.unlockTrash()
        #expect(try await trash.trashedAssets().filter { $0.id == imported }.count == 1)

        try await trash.restore(imported)
        #expect(try await trash.trashedAssets().isEmpty)
        #expect(try await timeline.asset(for: imported) != nil)

        try await timeline.delete([imported])
        try await trash.purge(imported)
        #expect(try await trash.trashedAssets().isEmpty)
        #expect(await lane.provider.owns(imported) == false)
    }

    @Test("an import's info panel reads its own file, not the mock's fixture")
    func importedMetadataIsTheFiles() async throws {
        let root = Self.temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let mock = MockEnvironment(scenario: .healthy)
        let library = PortBackedAssetProvider(library: mock.library, organize: mock.organize)
        let lane = AppEnvironment.makeManagedLane(root: root.appending(path: "Library", directoryHint: .isDirectory))
        let metadata = AppEnvironment.composeMetadata(locations: library, managed: lane)
        let imported = try await Self.importPhoto(through: lane, in: root)

        let facts = try #require(await metadata.metadata(for: imported))
        #expect(facts.cameraMake == Self.fixtureMake)
        #expect(facts.codec == "JPEG")
        let latitude = try #require(facts.latitude)
        #expect(abs(latitude - Self.fixtureLatitude) < 0.001)

        let first = try await library.loadTimeline().asset(at: 0)
        #expect(await metadata.metadata(for: first) != nil)
    }

    @Test("an import whose original is gone shows empty facts, never the mock's fabricated ones")
    func importWithMissingOriginalIsNotFabricated() async throws {
        let root = Self.temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let mock = MockEnvironment(scenario: .healthy)
        let library = PortBackedAssetProvider(library: mock.library, organize: mock.organize)
        let lane = AppEnvironment.makeManagedLane(root: root.appending(path: "Library", directoryHint: .isDirectory))
        let metadata = AppEnvironment.composeMetadata(locations: library, managed: lane)
        let imported = try await Self.importPhoto(through: lane, in: root)
        guard case let .managed(uuid) = imported.id else {
            Issue.record("an import is a managed asset")
            return
        }
        let original = try #require(await lane.reader.mediaFile(uuid: uuid))
        try FileManager.default.removeItem(at: original)

        let facts = try #require(await metadata.metadata(for: imported))

        #expect(facts == AssetExifMetadata())
    }

    @Test("an import's caption is refused by its owner instead of landing in the mock library")
    func importedCaptionIsRouted() async throws {
        let root = Self.temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let mock = MockEnvironment(scenario: .emptyLibrary)
        let lane = AppEnvironment.makeManagedLane(root: root.appending(path: "Library", directoryHint: .isDirectory))
        let captions = AppEnvironment.composeCaptions(
            library: PortBackedCaptionStore(library: mock.library, organize: mock.organize),
            managed: lane
        )
        let imported = try await Self.importPhoto(through: lane, in: root).id

        await #expect(throws: CaptionStoreError.unsupported) {
            try await captions.setCaption("harbour", for: imported)
        }
        #expect(await captions.caption(for: imported) == nil)
    }

    // MARK: - Fixtures

    private static let fixtureMake = "Fujifilm"
    private static let fixtureLatitude = 48.8584

    /// Import a real JPEG through the lane's importer and return its asset.
    private static func importPhoto(through lane: AppEnvironment.ManagedLane, in root: URL) async throws -> Asset {
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let url = root.appending(path: "picked-photo.jpg")
        try jpeg(width: 320, height: 240).write(to: url)
        await lane.importer.importPicked([ImportSource(url: url, originalFilename: "picked-photo.jpg")])
        let uuid = try #require(lane.importer.lastResult?.importedAssetIDs.first)
        return try #require(await lane.provider.asset(for: .managed(uuid: uuid)))
    }

    /// Real JPEG bytes with a camera make and a GPS fix, encoded at test time so
    /// no image ships in the repository.
    private static func jpeg(width: Int, height: Int) throws -> Data {
        let context = try #require(CGContext(
            data: nil,
            width: width,
            height: height,
            bitsPerComponent: 8,
            bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
        ))
        context.setFillColor(red: 0.8, green: 0.4, blue: 0.1, alpha: 1)
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        let image = try #require(context.makeImage())
        let properties: [CFString: Any] = [
            kCGImagePropertyTIFFDictionary: [kCGImagePropertyTIFFMake: fixtureMake] as [CFString: Any],
            kCGImagePropertyGPSDictionary: [
                kCGImagePropertyGPSLatitude: fixtureLatitude,
                kCGImagePropertyGPSLatitudeRef: "N",
                kCGImagePropertyGPSLongitude: 2.2945,
                kCGImagePropertyGPSLongitudeRef: "E",
            ] as [CFString: Any],
        ]
        let output = NSMutableData()
        let destination = try #require(CGImageDestinationCreateWithData(
            output as CFMutableData,
            UTType.jpeg.identifier as CFString,
            1,
            nil
        ))
        CGImageDestinationAddImage(destination, image, properties as CFDictionary)
        #expect(CGImageDestinationFinalize(destination))
        return output as Data
    }

    private static func temporaryRoot() -> URL {
        URL.temporaryDirectory.appending(path: "ManagedLaneTests-\(UUID().uuidString)", directoryHint: .isDirectory)
    }

    /// A file standing in for the picker's output. Its bytes are not an image,
    /// which the importer stores anyway: metadata extraction falls back to
    /// defaults rather than refusing the original.
    private static func pickedFile(in root: URL) throws -> ImportSource {
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let url = root.appending(path: "picked.jpg")
        try Data("picked-photo-\(UUID().uuidString)".utf8).write(to: url)
        return ImportSource(url: url, originalFilename: "picked.jpg")
    }
}
