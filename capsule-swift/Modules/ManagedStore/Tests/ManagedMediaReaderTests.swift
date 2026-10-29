import CapsuleCatalog
import CoreGraphics
import Foundation
import ImageIO
import Testing
import UniformTypeIdentifiers

import CapsuleTestSupport

@testable import ManagedStore

@Suite("ManagedMediaReader")
struct ManagedMediaReaderTests {
    private let layout = ManagedLibraryLayout(root: URL(filePath: "/capsule/Library"))

    /// A reader over one catalogued asset whose original is `bytes`.
    private func makeReader(
        uuid: String,
        bytes: Data,
        fileExtension: String = "jpg"
    ) async throws -> (ManagedMediaReader, MockFileStore) {
        let catalog = MockCatalog()
        let fileStore = MockFileStore()
        let row = Fixtures.catalogAsset(id: uuid)
        try await catalog.insertAsset(row)
        let captureDate = Date(timeIntervalSince1970: TimeInterval(row.captureTimestamp))
        let media = layout.mediaFile(uuid: uuid, fileExtension: fileExtension, captureDate: captureDate)
        await fileStore.seedFile(bytes, at: media)
        await fileStore.seedFile(Data("sidecar".utf8), at: layout.sidecarFile(forMediaFile: media))
        let library = ManagedLibrary(layout: layout, fileStore: fileStore, catalog: catalog)
        return (ManagedMediaReader(library: library, fileStore: fileStore), fileStore)
    }

    @Test("an imported JPEG decodes with its longest edge bounded by the request")
    func decodesBounded() async throws {
        let (reader, _) = try await makeReader(uuid: "a", bytes: try ImageFixture.jpeg(width: 400, height: 200))

        let image = try #require(await reader.image(uuid: "a", maxPixelEdge: 100))

        #expect(max(image.width, image.height) <= 100)
        #expect(image.width > image.height)
    }

    @Test("the media file is the original, never its sidecar")
    func mediaFileSkipsSidecar() async throws {
        let (reader, _) = try await makeReader(uuid: "a", bytes: try ImageFixture.jpeg(width: 8, height: 8))

        let url = try #require(await reader.mediaFile(uuid: "a"))

        #expect(url.pathExtension == "jpg")
    }

    @Test("an identifier the catalog does not index answers nil everywhere")
    func unknownIdentifier() async throws {
        let (reader, _) = try await makeReader(uuid: "a", bytes: try ImageFixture.jpeg(width: 8, height: 8))

        #expect(await reader.owns(uuid: "b") == false)
        #expect(await reader.image(uuid: "b", maxPixelEdge: 64) == nil)
        #expect(await reader.facts(uuid: "b") == nil)
        #expect(await reader.owns(uuid: "a"))
    }

    @Test("bytes ImageIO cannot decode give no image but still a size")
    func undecodableOriginal() async throws {
        let bytes = Data("not-an-image".utf8)
        let (reader, _) = try await makeReader(uuid: "a", bytes: bytes)

        #expect(await reader.image(uuid: "a", maxPixelEdge: 64) == nil)
        let facts = try #require(await reader.facts(uuid: "a"))
        #expect(facts.byteCount == Int64(bytes.count))
        #expect(facts.cameraMake == nil)
    }

    @Test("a repeat request is served from the cache, not the disk")
    func cachesDecodes() async throws {
        let (reader, fileStore) = try await makeReader(uuid: "a", bytes: try ImageFixture.jpeg(width: 64, height: 64))
        _ = try #require(await reader.image(uuid: "a", maxPixelEdge: 32))

        // With reads failing, only the cache can answer.
        await fileStore.injectFailure(on: .read)
        #expect(await reader.image(uuid: "a", maxPixelEdge: 32) != nil)

        reader.evictAll()
        #expect(await reader.image(uuid: "a", maxPixelEdge: 32) == nil)
    }

    @Test("a purged asset's cached pixels are not served once its row is gone")
    func purgedAssetMissesCache() async throws {
        let catalog = MockCatalog()
        let fileStore = MockFileStore()
        let row = Fixtures.catalogAsset(id: "a")
        try await catalog.insertAsset(row)
        let captureDate = Date(timeIntervalSince1970: TimeInterval(row.captureTimestamp))
        let media = layout.mediaFile(uuid: "a", fileExtension: "jpg", captureDate: captureDate)
        await fileStore.seedFile(try ImageFixture.jpeg(width: 64, height: 64), at: media)
        let library = ManagedLibrary(layout: layout, fileStore: fileStore, catalog: catalog)
        let reader = ManagedMediaReader(library: library, fileStore: fileStore)
        _ = try #require(await reader.image(uuid: "a", maxPixelEdge: 32))

        await catalog.removeAsset(id: "a")

        #expect(await reader.image(uuid: "a", maxPixelEdge: 32) == nil)
    }

    @Test("camera and signed GPS facts are read from the container")
    func readsFacts() async throws {
        let bytes = try ImageFixture.jpeg(
            width: 16,
            height: 16,
            make: "Fujifilm",
            model: "X-T5",
            latitude: 33.5,
            longitude: 151.25,
            latitudeRef: "S",
            longitudeRef: "E"
        )
        let (reader, _) = try await makeReader(uuid: "a", bytes: bytes)

        let facts = try #require(await reader.facts(uuid: "a"))

        #expect(facts.fileExtension == "jpg")
        #expect(facts.cameraMake == "Fujifilm")
        #expect(facts.cameraModel == "X-T5")
        let latitude = try #require(facts.latitude)
        let longitude = try #require(facts.longitude)
        #expect(abs(latitude + 33.5) < 0.001)
        #expect(abs(longitude - 151.25) < 0.001)
    }
}

// MARK: - Fixtures

/// Real JPEG bytes, encoded at test time so no image ships in the repository.
enum ImageFixture {
    struct EncodingFailed: Error {}

    static func jpeg(
        width: Int,
        height: Int,
        make: String? = nil,
        model: String? = nil,
        latitude: Double? = nil,
        longitude: Double? = nil,
        latitudeRef: String = "N",
        longitudeRef: String = "E"
    ) throws -> Data {
        guard let context = CGContext(
            data: nil,
            width: width,
            height: height,
            bitsPerComponent: 8,
            bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
        ) else { throw EncodingFailed() }
        context.setFillColor(red: 0.2, green: 0.5, blue: 0.8, alpha: 1)
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        guard let image = context.makeImage() else { throw EncodingFailed() }

        var properties: [CFString: Any] = [:]
        var tiff: [CFString: Any] = [:]
        if let make { tiff[kCGImagePropertyTIFFMake] = make }
        if let model { tiff[kCGImagePropertyTIFFModel] = model }
        if !tiff.isEmpty { properties[kCGImagePropertyTIFFDictionary] = tiff }
        if let latitude, let longitude {
            properties[kCGImagePropertyGPSDictionary] = [
                kCGImagePropertyGPSLatitude: latitude,
                kCGImagePropertyGPSLatitudeRef: latitudeRef,
                kCGImagePropertyGPSLongitude: longitude,
                kCGImagePropertyGPSLongitudeRef: longitudeRef,
            ] as [CFString: Any]
        }

        let output = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(
            output as CFMutableData,
            UTType.jpeg.identifier as CFString,
            1,
            nil
        ) else { throw EncodingFailed() }
        CGImageDestinationAddImage(destination, image, properties as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { throw EncodingFailed() }
        return output as Data
    }
}
