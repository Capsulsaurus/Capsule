import AssetKit
import FeatureTimeline
import Foundation
import ImagePipeline
import ManagedStore

/// The app's composition root — constructs and holds the shared services that
/// feature screens depend on, injected by constructor from a single place.
///
/// The timeline is served by a ``CompositeAssetProvider`` that merges the
/// system Photos library and the Capsule-managed store into one chronological
/// feed; the importer brings picked photos into the managed store.
@MainActor
struct AppEnvironment {
    let assetProvider: any AssetProvider
    let thumbnails: any ThumbnailProvider
    let mediaLoader: ViewerMediaLoader
    let importer: LibraryImporter

    init() {
        let layout = ManagedLibraryLayout(root: Self.libraryRoot())
        let library = ManagedLibrary(layout: layout)
        let photoKitProvider = PhotoKitProvider()
        let managedProvider = ManagedProvider(library: library)
        let importService = ImportService(
            library: library,
            fileStore: SystemFileStore(),
            hasher: CryptoKitHasher(),
            metadataExtractor: ImageIOMetadataExtractor()
        )

        assetProvider = CompositeAssetProvider(providers: [photoKitProvider, managedProvider])
        thumbnails = ImagePipeline()
        mediaLoader = ViewerMediaLoader()
        importer = LibraryImporter(importService: importService, managedProvider: managedProvider)
    }

    /// The managed library's root, falling back to the temporary directory if
    /// Application Support cannot be located.
    private static func libraryRoot() -> URL {
        if let root = try? ManagedLibraryLayout.defaultRoot() {
            return root
        }
        return URL.temporaryDirectory.appending(path: "CapsuleLibrary", directoryHint: .isDirectory)
    }
}
