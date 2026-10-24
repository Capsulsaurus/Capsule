import AssetKit
import CapsuleFoundation
import CapsuleMock
import FeatureTimeline
import Foundation
import ManagedStore
import Testing

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

    // MARK: - Fixtures

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
