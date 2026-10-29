import CapsuleFoundation
import CoreGraphics
import Foundation
import ImageIO

// MARK: - ManagedMediaFacts

/// What an imported file says about itself, read from its container.
///
/// Every field is optional because every one is genuinely absent for some real
/// file: a screenshot has no lens, a scan has no GPS fix. A file the decoder
/// cannot read at all still has a size and an extension, so those two are the
/// only facts a stored original always yields.
public struct ManagedMediaFacts: Sendable, Equatable {
    /// The extension the importer stored the original under, lowercased.
    public var fileExtension: String
    /// Size on disk, in bytes.
    public var byteCount: Int64
    public var cameraMake: String?
    public var cameraModel: String?
    public var lensModel: String?
    public var isoSpeed: Int?
    public var aperture: Double?
    public var shutterSpeed: Double?
    public var focalLength: Double?
    /// Signed decimal degrees: south of the equator is negative.
    public var latitude: Double?
    /// Signed decimal degrees: west of Greenwich is negative.
    public var longitude: Double?

    public init(fileExtension: String, byteCount: Int64) {
        self.fileExtension = fileExtension
        self.byteCount = byteCount
    }
}

// MARK: - ManagedMediaReader

/// Pixels and file facts for the assets in the on-disk managed store.
///
/// The import pipeline writes an original under `media/{YYYY}/{YYYY-MM}/{uuid}.{ext}`
/// and the catalog row that indexes it; this is the read side of that pair. It
/// resolves an asset's file through the catalog row's capture date (the
/// partition is derived from it), then decodes through ImageIO's thumbnail path,
/// which downsamples while decoding rather than inflating the full frame first.
///
/// ## Ownership is exact
///
/// Every read answers `nil` for an identifier the catalog has no row for. That
/// is what lets a composite put this reader *in front of* the mock renderer:
/// the mock decodes any identifier whose shape it recognises, so it cannot be
/// the one asked first.
///
/// ## Caching
///
/// Decoded images are held under a byte budget in an `NSCache`, keyed by asset
/// and edge, so a grid that scrolls back over a tile does not decode it twice.
/// A trashed asset keeps its row, and therefore its thumbnail, until it is
/// purged — which is what lets Recently Deleted show what it is offering to
/// destroy.
///
/// A `Sendable` class over thread-safe state rather than an actor, so the
/// decoded `CGImage` never crosses an isolation boundary on its way to the
/// grid.
public final class ManagedMediaReader: Sendable {
    /// The cache's byte ceiling — about forty full-screen frames, or a few
    /// thousand grid tiles.
    public static let defaultByteBudget = 64 * 1024 * 1024

    private let library: ManagedLibrary
    private let fileStore: any FileStore
    private let cache: DecodedImageCache

    /// - Parameter fileStore: where the bytes are read from — the same store the
    ///   importer wrote them through.
    public init(
        library: ManagedLibrary,
        fileStore: any FileStore = SystemFileStore(),
        byteBudget: Int = ManagedMediaReader.defaultByteBudget
    ) {
        self.library = library
        self.fileStore = fileStore
        cache = DecodedImageCache(byteBudget: byteBudget)
    }

    /// Whether the catalog indexes this asset, trashed or not.
    public func owns(uuid: String) async -> Bool {
        await catalogRow(uuid: uuid) != nil
    }

    /// The stored original's location, or `nil` when the asset is not this
    /// store's or its file is missing.
    public func mediaFile(uuid: String) async -> URL? {
        guard let captureDate = await catalogRow(uuid: uuid) else { return nil }
        return await mediaFile(uuid: uuid, captureDate: captureDate)
    }

    /// The asset decoded with its longest edge at most `maxPixelEdge`, with the
    /// file's orientation applied, or `nil` when it is not this store's or its
    /// bytes are not a format ImageIO decodes.
    ///
    /// The catalog is asked before the cache, so a purged asset's decoded
    /// pixels are never served from memory after its row is gone.
    ///
    /// An undecodable original is still a stored original: `nil` here is a
    /// placeholder tile, never a reason to doubt the import.
    public func image(uuid: String, maxPixelEdge: Int) async -> CGImage? {
        let edge = max(1, maxPixelEdge)
        let key = "\(uuid)@\(edge)"
        guard let captureDate = await catalogRow(uuid: uuid) else {
            cache.removeImage(forKey: key)
            return nil
        }
        if let cached = cache.image(forKey: key) { return cached }

        let signposter = CapsuleSignpost.imagePipeline
        let interval = signposter.beginInterval("managed-thumbnail")
        defer { signposter.endInterval("managed-thumbnail", interval) }

        guard let url = await mediaFile(uuid: uuid, captureDate: captureDate),
              let data = await read(url)
        else { return nil }
        guard let image = Self.downsample(data, maxPixelEdge: edge) else {
            CapsuleLog.managedStore.debug("no decodable image for \(uuid, privacy: .public)")
            return nil
        }
        cache.insert(image, forKey: key)
        CapsuleLog.managedStore.debug(
            "decoded \(uuid, privacy: .public) at \(image.width, privacy: .public)x\(image.height, privacy: .public)"
        )
        return image
    }

    /// The facts the stored original carries, or `nil` when the asset is not
    /// this store's or its file is missing.
    public func facts(uuid: String) async -> ManagedMediaFacts? {
        guard let url = await mediaFile(uuid: uuid), let data = await read(url) else { return nil }
        return Self.facts(of: data, fileExtension: url.pathExtension.lowercased())
    }

    /// Drop every decoded image — what a memory-pressure event triggers.
    public func evictAll() {
        CapsuleLog.managedStore.notice("evicting decoded managed images")
        cache.removeAll()
    }

    // MARK: Private

    private func mediaFile(uuid: String, captureDate: Date) async -> URL? {
        do {
            return try await library.mediaFile(uuid: uuid, captureDate: captureDate)
        } catch {
            CapsuleLog.managedStore.error(
                "media file lookup failed for \(uuid, privacy: .public): \(String(describing: error), privacy: .public)"
            )
            return nil
        }
    }

    /// The partition date of a catalogued asset, or `nil` when there is no row.
    private func catalogRow(uuid: String) async -> Date? {
        do {
            let catalog = try await library.catalog()
            guard let row = try await catalog.asset(id: uuid) else { return nil }
            return Date(timeIntervalSince1970: TimeInterval(row.captureTimestamp))
        } catch {
            CapsuleLog.managedStore.error(
                "catalog read failed for \(uuid, privacy: .public): \(String(describing: error), privacy: .public)"
            )
            return nil
        }
    }

    private func read(_ url: URL) async -> Data? {
        do {
            return try await fileStore.read(at: url)
        } catch {
            CapsuleLog.managedStore.error(
                "media read failed at \(url.lastPathComponent, privacy: .public): \(String(describing: error), privacy: .public)"
            )
            return nil
        }
    }

    /// Decode straight to the requested size through ImageIO's thumbnail path.
    ///
    /// `CreateThumbnailFromImageAlways` ignores any small preview embedded in
    /// the file, which would otherwise be upscaled into a blurry viewer frame;
    /// `WithTransform` applies the EXIF orientation so a portrait photo is not
    /// shown on its side.
    static func downsample(_ data: Data, maxPixelEdge: Int) -> CGImage? {
        let sourceOptions = [kCGImageSourceShouldCache: false] as CFDictionary
        guard let source = CGImageSourceCreateWithData(data as CFData, sourceOptions),
              CGImageSourceGetCount(source) > 0
        else { return nil }
        let options = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceThumbnailMaxPixelSize: maxPixelEdge,
        ] as CFDictionary
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options)
    }

    /// Container properties only — never a pixel decode. The field parse is
    /// ``ContainerExif``'s, shared with the viewer's PhotoKit loader.
    static func facts(of data: Data, fileExtension: String) -> ManagedMediaFacts {
        var facts = ManagedMediaFacts(fileExtension: fileExtension, byteCount: Int64(data.count))
        let exif = ContainerExif.parse(data)
        facts.cameraMake = exif.cameraMake
        facts.cameraModel = exif.cameraModel
        facts.lensModel = exif.lensModel
        facts.isoSpeed = exif.isoSpeed
        facts.aperture = exif.aperture
        facts.shutterSpeed = exif.shutterSpeed
        facts.focalLength = exif.focalLength
        facts.latitude = exif.latitude
        facts.longitude = exif.longitude
        return facts
    }
}

// MARK: - DecodedImageCache

/// A byte-budgeted cache of decoded images.
///
/// `@unchecked Sendable` because its only state is an `NSCache`, which is
/// documented thread-safe; nothing else is mutable.
private final class DecodedImageCache: @unchecked Sendable {
    private let storage = NSCache<NSString, Entry>()

    init(byteBudget: Int) {
        storage.totalCostLimit = byteBudget
    }

    func image(forKey key: String) -> CGImage? {
        storage.object(forKey: key as NSString)?.image
    }

    func insert(_ image: CGImage, forKey key: String) {
        storage.setObject(Entry(image), forKey: key as NSString, cost: image.bytesPerRow * image.height)
    }

    func removeImage(forKey key: String) {
        storage.removeObject(forKey: key as NSString)
    }

    func removeAll() {
        storage.removeAllObjects()
    }

    private final class Entry {
        let image: CGImage

        init(_ image: CGImage) {
            self.image = image
        }
    }
}
