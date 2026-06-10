import AssetKit
import CapsuleCatalog
import CapsuleFoundation
import Foundation

/// Factory functions for test data, so test bodies state only what they care
/// about and inherit sensible defaults for everything else.
public enum Fixtures {
    /// A fixed reference instant — 2024-07-03 12:26:40 UTC.
    public static let referenceTimestamp: Int64 = 1_720_000_000

    /// A fixed reference `Date` at ``referenceTimestamp``.
    public static let referenceDate = Date(timeIntervalSince1970: TimeInterval(referenceTimestamp))

    // MARK: Catalog

    public static func catalogAsset(
        id: String = UUID().uuidString,
        assetType: String = "photo",
        captureTimestamp: Int64 = referenceTimestamp,
        hashSHA256: String? = nil,
        width: Int64? = 4032,
        height: Int64? = 3024,
        stackID: String? = nil,
        isStackHidden: Bool = false,
        albumID: String? = nil,
        rating: Int64 = 0,
        isDeleted: Bool = false,
        deletedAt: Int64? = nil
    ) -> CatalogAsset {
        CatalogAsset(
            id: id,
            assetType: assetType,
            captureTimestamp: captureTimestamp,
            importTimestamp: referenceTimestamp,
            hashSHA256: hashSHA256 ?? syntheticHash(id),
            captureUTC: captureTimestamp,
            width: width,
            height: height,
            stackID: stackID,
            isStackHidden: isStackHidden,
            albumID: albumID,
            rating: rating,
            isDeleted: isDeleted,
            deletedAt: deletedAt ?? (isDeleted ? captureTimestamp : nil)
        )
    }

    public static func catalogAlbum(
        id: String = UUID().uuidString,
        name: String = "Test Album",
        createdAt: Int64 = referenceTimestamp,
        coverAssetID: String? = nil
    ) -> CatalogAlbum {
        CatalogAlbum(
            id: id,
            name: name,
            createdAt: createdAt,
            modifiedAt: createdAt,
            coverAssetID: coverAssetID
        )
    }

    public static func sidecar(
        uuid: String = UUID().uuidString,
        assetType: String = "photo",
        originalFilename: String = "IMG_0001.HEIC",
        hashSHA256: String? = nil,
        fileSize: UInt64 = 2_400_000,
        stackHint: CatalogStackHint? = nil
    ) -> CatalogSidecar {
        CatalogSidecar(
            version: 1,
            uuid: uuid,
            assetType: assetType,
            originalFilename: originalFilename,
            importTimestamp: referenceTimestamp,
            modifiedTimestamp: referenceTimestamp,
            hashSHA256: hashSHA256 ?? syntheticHash(uuid),
            fileSize: fileSize,
            importerVersion: "capsule-ios/0.1.0",
            rawshiftVersion: "0.0.0",
            captureTimestamp: referenceTimestamp,
            captureUTC: referenceTimestamp,
            width: 4032,
            height: 3024,
            stackHint: stackHint
        )
    }

    // MARK: Domain

    public static func asset(
        id: AssetID? = nil,
        mediaType: MediaType = .photo,
        captureDate: Date = referenceDate,
        pixelWidth: Int = 4032,
        pixelHeight: Int = 3024,
        duration: TimeInterval = 0,
        isFavorite: Bool = false
    ) -> Asset {
        Asset(
            id: id ?? .managed(uuid: UUID().uuidString),
            mediaType: mediaType,
            captureDate: captureDate,
            pixelWidth: pixelWidth,
            pixelHeight: pixelHeight,
            duration: duration,
            isFavorite: isFavorite
        )
    }

    /// A run of `count` assets, one day apart, newest last.
    public static func assets(count: Int, mediaType: MediaType = .photo) -> [Asset] {
        (0 ..< count).map { index in
            asset(
                mediaType: mediaType,
                captureDate: referenceDate.addingTimeInterval(TimeInterval(index) * 86_400)
            )
        }
    }

    // MARK: Helpers

    /// A deterministic 64-character hex string derived from `seed` — a stand-in
    /// for a real SHA-256 that stays unique per distinct seed.
    public static func syntheticHash(_ seed: String) -> String {
        var value: UInt64 = 5381
        for byte in seed.utf8 {
            value = (value &* 33) &+ UInt64(byte)
        }
        let hex = String(value, radix: 16)
        return String((hex + String(repeating: "0", count: 64)).prefix(64))
    }
}
