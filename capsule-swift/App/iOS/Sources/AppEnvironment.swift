import AssetKit
import CapsuleDiagnostics
import FeatureTimeline
import Foundation
import ImagePipeline
import ManagedStore

/// The app's composition root — constructs and holds the shared services that
/// feature screens depend on, injected by constructor from a single place.
///
/// The timeline is served by a ``CompositeAssetProvider`` that merges the
/// system Photos library and the Capsule-managed store; albums by a
/// ``CompositeAlbumProvider`` over system smart albums and Capsule user albums.
@MainActor
struct AppEnvironment {
    let assetProvider: any AssetProvider
    let albumProvider: any AlbumProvider
    let trashProvider: any TrashProvider
    let hiddenStore: HiddenStore
    let thumbnails: any ThumbnailProvider
    let mediaLoader: ViewerMediaLoader
    let importer: LibraryImporter

    /// Persisted diagnostics & telemetry consent (local-only by default).
    let consentStore: ConsentStore
    /// Wires MetricKit, breadcrumbs, the crash prompt, and bug-report export.
    let diagnostics: DiagnosticsCoordinator

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
        albumProvider = CompositeAlbumProvider(providers: [
            PhotoKitAlbumProvider(),
            ManagedAlbumProvider(library: library),
        ])
        trashProvider = managedProvider
        hiddenStore = HiddenStore()
        thumbnails = ImagePipeline()
        mediaLoader = ViewerMediaLoader()
        importer = LibraryImporter(importService: importService, managedProvider: managedProvider)

        let consent = ConsentStore()
        consentStore = consent
        diagnostics = DiagnosticsCoordinator(consent: consent)
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
