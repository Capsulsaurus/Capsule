import AssetKit
import CapsuleFoundation
import Photos
import UIKit

/// Loads and prefetches the thumbnails the photo grid renders.
///
/// Feature views depend on this protocol, never on PhotoKit directly, so they
/// can be exercised against a mock.
public protocol ThumbnailProvider: Sendable {
    /// A thumbnail for `asset` decoded to fill `pixelSize`, or `nil` if it
    /// cannot be produced. `pixelSize` is in device pixels.
    func thumbnail(for asset: Asset, pixelSize: CGSize) async -> UIImage?

    /// Warm the cache for assets about to scroll on screen.
    func beginPrefetching(for assets: [Asset], pixelSize: CGSize) async

    /// Drop cache warming for assets that scrolled away unseen.
    func cancelPrefetching(for assets: [Asset], pixelSize: CGSize) async

    /// Drop every cached thumbnail — called by the app under memory pressure.
    func flushCaches() async
}

/// The production ``ThumbnailProvider``.
///
/// For PhotoKit assets it wraps a `PHCachingImageManager`, which decodes and
/// caches thumbnails off the main thread and serves prefetch requests. The
/// actor confines the (non-`Sendable`) image manager and the local-identifier→
/// `PHAsset` resolution cache.
///
/// ``flushCaches()`` lets the app drop both caches on a memory-pressure event,
/// so a long scrolling session cannot grow the cache without bound.
///
/// - Note: managed-store (file-backed) thumbnails via ImageIO downsampling are
///   added in Phase 4; until then ``thumbnail(for:pixelSize:)`` returns `nil`
///   for `.managed` assets.
public actor ImagePipeline: ThumbnailProvider {
    private let cachingManager = PHCachingImageManager()
    private var resolvedAssets: [String: PHAsset] = [:]

    public init() {
        cachingManager.allowsCachingHighQualityImages = false
    }

    public func thumbnail(for asset: Asset, pixelSize: CGSize) async -> UIImage? {
        let signposter = CapsuleSignpost.imagePipeline
        let interval = signposter.beginInterval("thumbnail")
        defer { signposter.endInterval("thumbnail", interval) }

        guard case let .photoKit(localIdentifier) = asset.id,
              let phAsset = phAsset(for: localIdentifier)
        else {
            return nil
        }
        return await requestImage(phAsset, pixelSize: pixelSize)
    }

    public func beginPrefetching(for assets: [Asset], pixelSize: CGSize) {
        let phAssets = photoKitAssets(in: assets)
        guard !phAssets.isEmpty else { return }
        cachingManager.startCachingImages(
            for: phAssets,
            targetSize: pixelSize,
            contentMode: .aspectFill,
            options: Self.makeRequestOptions()
        )
    }

    public func cancelPrefetching(for assets: [Asset], pixelSize: CGSize) {
        let phAssets = photoKitAssets(in: assets)
        guard !phAssets.isEmpty else { return }
        cachingManager.stopCachingImages(
            for: phAssets,
            targetSize: pixelSize,
            contentMode: .aspectFill,
            options: Self.makeRequestOptions()
        )
    }

    /// Drop all cached thumbnails — invoked by the app under memory pressure.
    public func flushCaches() {
        CapsuleLog.imagePipeline.notice("memory pressure — flushing thumbnail caches")
        cachingManager.stopCachingImagesForAllAssets()
        resolvedAssets.removeAll(keepingCapacity: true)
    }

    // MARK: Private

    private func requestImage(_ phAsset: PHAsset, pixelSize: CGSize) async -> UIImage? {
        await withCheckedContinuation { continuation in
            cachingManager.requestImage(
                for: phAsset,
                targetSize: pixelSize,
                contentMode: .aspectFill,
                options: Self.makeRequestOptions()
            ) { image, _ in
                continuation.resume(returning: image)
            }
        }
    }

    /// Resolve a single local identifier to its `PHAsset`, caching the result.
    private func phAsset(for localIdentifier: String) -> PHAsset? {
        if let cached = resolvedAssets[localIdentifier] { return cached }
        let result = PHAsset.fetchAssets(withLocalIdentifiers: [localIdentifier], options: nil)
        guard let phAsset = result.firstObject else { return nil }
        resolvedAssets[localIdentifier] = phAsset
        return phAsset
    }

    /// Resolve the PhotoKit assets among `assets`, batch-fetching any misses.
    private func photoKitAssets(in assets: [Asset]) -> [PHAsset] {
        let localIdentifiers = assets.compactMap { asset -> String? in
            guard case let .photoKit(localIdentifier) = asset.id else { return nil }
            return localIdentifier
        }
        let missing = localIdentifiers.filter { resolvedAssets[$0] == nil }
        if !missing.isEmpty {
            let fetched = PHAsset.fetchAssets(withLocalIdentifiers: missing, options: nil)
            for index in 0 ..< fetched.count {
                let phAsset = fetched.object(at: index)
                resolvedAssets[phAsset.localIdentifier] = phAsset
            }
        }
        return localIdentifiers.compactMap { resolvedAssets[$0] }
    }

    /// Fresh request options — a high-quality single-callback decode, sized
    /// fast, with iCloud download permitted.
    private static func makeRequestOptions() -> PHImageRequestOptions {
        let options = PHImageRequestOptions()
        options.deliveryMode = .highQualityFormat
        options.resizeMode = .fast
        options.isNetworkAccessAllowed = true
        return options
    }
}
