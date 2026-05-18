import AssetKit
import ImagePipeline

/// The app's composition root — constructs and holds the shared services that
/// feature screens depend on, injected by constructor from a single place.
///
/// Phase 4 swaps the bare `PhotoKitProvider` for a composite provider over
/// PhotoKit and the Capsule-managed store.
struct AppEnvironment: Sendable {
    let assetProvider: any AssetProvider
    let thumbnails: any ThumbnailProvider

    init() {
        assetProvider = PhotoKitProvider()
        thumbnails = ImagePipeline()
    }
}
