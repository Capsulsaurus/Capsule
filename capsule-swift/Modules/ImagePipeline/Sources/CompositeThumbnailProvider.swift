import AssetKit
import CapsuleFoundation
import CoreGraphics

// MARK: - CompositeThumbnailProvider

/// One ``ThumbnailProvider`` over several sources: the first that renders an
/// asset answers for it.
///
/// The thumbnail-side counterpart of `CompositeAssetProvider`. A timeline that
/// interleaves two stores needs a renderer that can draw from both, and each
/// provider already answers `nil` for an asset it does not hold, so asking them
/// in order is the whole routing.
///
/// **Order is ownership.** Put a provider whose `nil` is exact (one that checks
/// its own index) ahead of one that renders anything shaped like its own ids,
/// or the second will draw a stand-in over an asset it does not own.
public struct CompositeThumbnailProvider: ThumbnailProvider {
    private let providers: [any ThumbnailProvider]

    public init(providers: [any ThumbnailProvider]) {
        self.providers = providers
    }

    public func thumbnail(for asset: Asset, pixelSize: CGSize) async -> PlatformImage? {
        for provider in providers {
            if let image = await provider.thumbnail(for: asset, pixelSize: pixelSize) {
                return image
            }
        }
        return nil
    }

    /// Every provider is handed the whole batch; each warms only what it holds.
    public func beginPrefetching(for assets: [Asset], pixelSize: CGSize) async {
        for provider in providers {
            await provider.beginPrefetching(for: assets, pixelSize: pixelSize)
        }
    }

    public func cancelPrefetching(for assets: [Asset], pixelSize: CGSize) async {
        for provider in providers {
            await provider.cancelPrefetching(for: assets, pixelSize: pixelSize)
        }
    }

    public func flushCaches() async {
        for provider in providers {
            await provider.flushCaches()
        }
    }
}

// MARK: - CompositeMetadataSource

/// One ``AssetMetadataSource`` over several: the first that describes an asset
/// answers for it.
///
/// Ordered by ownership for the same reason ``CompositeThumbnailProvider`` is: a
/// source that derives fixture metadata for any non-PhotoKit asset must come
/// after one that reads a real file, or an import would be shown a camera it
/// was never shot on.
public struct CompositeMetadataSource: AssetMetadataSource {
    private let sources: [any AssetMetadataSource]

    public init(sources: [any AssetMetadataSource]) {
        self.sources = sources
    }

    public func metadata(for asset: Asset) async -> AssetExifMetadata? {
        for source in sources {
            if let metadata = await source.metadata(for: asset) {
                return metadata
            }
        }
        return nil
    }
}
