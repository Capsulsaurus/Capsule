import CapsuleFoundation
import Foundation
import Photos

/// The ``AlbumProvider`` over the system Photos library's smart albums.
///
/// Smart albums (Favorites, Recents, Videos, Selfies, …) are curated by the
/// system and read-only — `createUserAlbum` and `addAsset` therefore throw
/// ``AlbumError/readOnly``.
///
/// Not every subtype exists on every platform: macOS has no Selfies or
/// Screenshots smart album, for instance. That needs no branch, because a
/// fetch for an absent subtype simply returns no collections and the
/// zero-count filter in ``loadAlbums()`` drops it — the Mac's albums screen
/// just shows a shorter list.
public struct PhotoKitAlbumProvider: AlbumProvider {
    /// The smart-album subtypes surfaced in the albums screen, in display
    /// order. Subtypes the running platform does not curate fetch empty and
    /// fall out of the list; see the type's note.
    private static let subtypes: [PHAssetCollectionSubtype] = [
        .smartAlbumUserLibrary,
        .smartAlbumFavorites,
        .smartAlbumRecentlyAdded,
        .smartAlbumVideos,
        .smartAlbumSelfPortraits,
        .smartAlbumScreenshots,
        .smartAlbumLivePhotos,
        .smartAlbumPanoramas,
        .smartAlbumBursts,
        .smartAlbumSlomoVideos,
    ]

    public init() {}

    public func loadAlbums() async -> [AlbumSummary] {
        var summaries: [AlbumSummary] = []
        for subtype in Self.subtypes {
            let collections = PHAssetCollection.fetchAssetCollections(
                with: .smartAlbum, subtype: subtype, options: nil
            )
            for index in 0 ..< collections.count {
                let collection = collections.object(at: index)
                let assetCount = PHAsset.fetchAssets(
                    in: collection, options: Self.fetchOptions()
                ).count
                guard assetCount > 0 else { continue }
                summaries.append(AlbumSummary(
                    id: .smart(localIdentifier: collection.localIdentifier),
                    title: collection.localizedTitle ?? String(localized: "app.albums.untitled"),
                    count: assetCount
                ))
            }
        }
        return summaries
    }

    public func assets(in albumID: AlbumID) async throws -> [Asset] {
        guard case let .smart(localIdentifier) = albumID else { return [] }
        let collections = PHAssetCollection.fetchAssetCollections(
            withLocalIdentifiers: [localIdentifier], options: nil
        )
        guard let collection = collections.firstObject else { throw AlbumError.notFound }
        let result = PHAsset.fetchAssets(in: collection, options: Self.fetchOptions())
        return (0 ..< result.count).map { Asset(photoKitAsset: result.object(at: $0)) }
    }

    public func createUserAlbum(named _: String) async throws {
        throw AlbumError.readOnly
    }

    public func addAsset(_: AssetID, to _: AlbumID) async throws {
        throw AlbumError.readOnly
    }

    public func changes() -> AsyncStream<Void> {
        // Smart-album membership is treated as static for this prototype.
        AsyncStream { $0.finish() }
    }

    private static func fetchOptions() -> PHFetchOptions {
        let options = PHFetchOptions()
        options.predicate = NSPredicate(
            format: "mediaType == %d OR mediaType == %d",
            PHAssetMediaType.image.rawValue,
            PHAssetMediaType.video.rawValue
        )
        options.sortDescriptors = [NSSortDescriptor(key: "creationDate", ascending: false)]
        return options
    }
}
