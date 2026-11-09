import CapsuleCatalog
import Foundation
import Testing

import CapsuleTestSupport

@testable import ManagedStore

@Suite("ImportService pipeline")
struct ImportServiceTests {
    private let layout = ManagedLibraryLayout(root: URL(filePath: "/capsule/Library"))

    private func makeService(
        catalog: MockCatalog,
        fileStore: MockFileStore,
        metadataExtractor: MockMetadataExtractor = MockMetadataExtractor()
    ) -> ImportService {
        ImportService(
            library: ManagedLibrary(
                layout: layout,
                fileStore: fileStore,
                catalog: catalog,
                hasher: MockContentHasher(),
                metadataExtractor: metadataExtractor
            ),
            fileStore: fileStore,
            hasher: MockContentHasher()
        )
    }

    @Test("imports a new file: media + sidecar on disk and a catalog row")
    func importsNewFile() async {
        let catalog = MockCatalog()
        let fileStore = MockFileStore()
        let source = URL(filePath: "/tmp/import/photo.jpg")
        await fileStore.seedFile(Data("photo-bytes".utf8), at: source)

        let service = makeService(catalog: catalog, fileStore: fileStore)
        let result = await service.importAssets(from: [
            ImportSource(url: source, originalFilename: "photo.jpg"),
        ])

        #expect(result.importedCount == 1)
        #expect(result.duplicateCount == 0)
        #expect(result.failureCount == 0)
        // The source plus a written media file and its sidecar.
        #expect(await fileStore.fileCount == 3)
        #expect(await catalog.timeline(filter: .all, offset: 0, limit: 10).count == 1)
    }

    @Test("skips a file whose content was already imported")
    func skipsDuplicate() async {
        let catalog = MockCatalog()
        let fileStore = MockFileStore()
        let data = Data("already-imported".utf8)
        let source = URL(filePath: "/tmp/import/dup.jpg")
        await fileStore.seedFile(data, at: source)
        // Seed the catalog with an asset carrying the same content hash.
        try? await catalog.insertAsset(
            Fixtures.catalogAsset(hashSHA256: MockContentHasher().hash(data))
        )

        let service = makeService(catalog: catalog, fileStore: fileStore)
        let result = await service.importAssets(from: [
            ImportSource(url: source, originalFilename: "dup.jpg"),
        ])

        #expect(result.importedCount == 0)
        #expect(result.duplicateCount == 1)
    }

    @Test("rolls back the media file and sidecar when the catalog insert fails")
    func rollsBackOnCatalogFailure() async {
        let catalog = MockCatalog()
        let fileStore = MockFileStore()
        let source = URL(filePath: "/tmp/import/fail.jpg")
        await fileStore.seedFile(Data("will-roll-back".utf8), at: source)
        await catalog.setFailInserts(true)

        let service = makeService(catalog: catalog, fileStore: fileStore)
        let result = await service.importAssets(from: [
            ImportSource(url: source, originalFilename: "fail.jpg"),
        ])

        #expect(result.failureCount == 1)
        #expect(result.importedCount == 0)
        // Only the original source survives — no partial media or sidecar.
        #expect(await fileStore.fileCount == 1)
    }

    @Test("reports each source independently across a mixed batch")
    func mixedBatch() async {
        let catalog = MockCatalog()
        let fileStore = MockFileStore()
        let fresh = URL(filePath: "/tmp/import/fresh.jpg")
        let dupe = URL(filePath: "/tmp/import/dupe.jpg")
        let dupeData = Data("seen-before".utf8)
        await fileStore.seedFile(Data("brand-new".utf8), at: fresh)
        await fileStore.seedFile(dupeData, at: dupe)
        try? await catalog.insertAsset(
            Fixtures.catalogAsset(hashSHA256: MockContentHasher().hash(dupeData))
        )

        let service = makeService(catalog: catalog, fileStore: fileStore)
        let result = await service.importAssets(from: [
            ImportSource(url: fresh, originalFilename: "fresh.jpg"),
            ImportSource(url: dupe, originalFilename: "dupe.jpg"),
        ])

        #expect(result.importedCount == 1)
        #expect(result.duplicateCount == 1)
    }

    @Test("an import's GPS fix lands in both its sidecar and its catalog row")
    func recordsGPS() async throws {
        let catalog = MockCatalog()
        let fileStore = MockFileStore()
        let source = URL(filePath: "/tmp/import/located.jpg")
        await fileStore.seedFile(Data("located".utf8), at: source)
        let service = makeService(
            catalog: catalog,
            fileStore: fileStore,
            metadataExtractor: MockMetadataExtractor(template: MediaMetadata(
                captureTimestamp: Fixtures.referenceTimestamp,
                latitude: -33.8568,
                longitude: 151.2153
            ))
        )

        let result = await service.importAssets(from: [ImportSource(url: source, originalFilename: "located.jpg")])

        let uuid = try #require(result.importedAssetIDs.first)
        let row = try #require(await catalog.asset(id: uuid))
        #expect(row.latitude == -33.8568)
        #expect(row.longitude == 151.2153)
        let captureDate = Date(timeIntervalSince1970: TimeInterval(row.captureTimestamp))
        let bytes = try #require(await fileStore.data(at: layout.sidecarFile(uuid: uuid, captureDate: captureDate)))
        let sidecar = try JSONSidecarCoder().decode(bytes)
        #expect(sidecar.gpsLatitude == -33.8568)
        #expect(sidecar.gpsLongitude == 151.2153)
    }

    @Test("a second import of the same bytes is a duplicate of the first")
    func reimportIsDuplicate() async {
        let catalog = MockCatalog()
        let fileStore = MockFileStore()
        let source = URL(filePath: "/tmp/import/again.jpg")
        await fileStore.seedFile(Data("again".utf8), at: source)
        let service = makeService(catalog: catalog, fileStore: fileStore)

        let first = await service.importAssets(from: [ImportSource(url: source, originalFilename: "again.jpg")])
        let second = await service.importAssets(from: [ImportSource(url: source, originalFilename: "again.jpg")])

        #expect(first.importedCount == 1)
        #expect(second.duplicateCount == 1)
    }

    @Test("the sealed name keeps the original's extension, or borrows the source file's")
    func sealFileName() {
        let named = ImportSource(url: URL(filePath: "/tmp/x.tmp"), originalFilename: "IMG_0001.HEIC")
        let bare = ImportSource(url: URL(filePath: "/tmp/copy.heic"), originalFilename: "IMG_0001")
        let neither = ImportSource(url: URL(filePath: "/tmp/copy"), originalFilename: "IMG_0001")
        #expect(ImportService.sealFileName(for: named) == "IMG_0001.HEIC")
        #expect(ImportService.sealFileName(for: bare) == "IMG_0001.heic")
        #expect(ImportService.sealFileName(for: neither) == "IMG_0001")
    }
}

@Suite("ImageIOMetadataExtractor EXIF parsing")
struct MetadataExtractorTests {
    @Test("parses an EXIF capture date into a Unix epoch")
    func parsesExifDate() {
        #expect(ImageIOMetadataExtractor.parseExifDate("2024:07:03 12:26:40") == 1720009600)
    }

    @Test("rejects a malformed EXIF date")
    func rejectsMalformedDate() {
        #expect(ImageIOMetadataExtractor.parseExifDate("not a date") == nil)
    }
}
