import AssetKit
import ImagePipeline

/// The app's composition root — constructs and holds the shared services that
/// feature screens depend on, injected by constructor from a single place.
///
/// `@MainActor` because ``ViewerMediaLoader`` is main-actor-confined; Phase 4
/// swaps the bare `PhotoKitProvider` for a composite over PhotoKit and the
/// Capsule-managed store.
@MainActor
struct AppEnvironment {
    let assetProvider: any AssetProvider
    let thumbnails: any ThumbnailProvider
    let mediaLoader: ViewerMediaLoader

    init() {
        assetProvider = PhotoKitProvider()
        thumbnails = ImagePipeline()
        mediaLoader = ViewerMediaLoader()
    }
}
