import CapsuleFoundation
import Foundation
import os
import Photos

/// The ``AssetProvider`` over the system Photos library (PhotoKit).
///
/// It owns the current `PHFetchResult`, observes the library for changes via
/// `PHPhotoLibraryChangeObserver`, and translates each `PHFetchResultChangeDetails`
/// into an ``AssetChange`` for every active ``changes()`` stream.
///
/// PhotoKit delivers change notifications on a background queue, so the small
/// mutable state (the fetch result and stream continuations) is guarded by an
/// `OSAllocatedUnfairLock`; the class is therefore `@unchecked Sendable`.
public final class PhotoKitProvider: NSObject, AssetProvider, PHPhotoLibraryChangeObserver,
    @unchecked Sendable {
    /// Errors surfaced by the provider.
    public enum ProviderError: Error, Sendable {
        /// The Photos library is not readable (denied or restricted access).
        case notAuthorized
    }

    private struct State {
        var fetchResult: PHFetchResult<PHAsset>?
        var observers: [UUID: AsyncStream<AssetChange>.Continuation] = [:]
        var isRegistered = false
    }

    private let state = OSAllocatedUnfairLock(initialState: State())
    private let library: PHPhotoLibrary

    public init(library: PHPhotoLibrary = .shared()) {
        self.library = library
        super.init()
    }

    deinit {
        if state.withLock(\.isRegistered) {
            library.unregisterChangeObserver(self)
        }
    }

    // MARK: AssetProvider

    public func authorizationStatus() async -> AssetAuthorizationStatus {
        AssetAuthorizationStatus(photoKit: PHPhotoLibrary.authorizationStatus(for: .readWrite))
    }

    @discardableResult
    public func requestAuthorization() async -> AssetAuthorizationStatus {
        let status = await withCheckedContinuation { continuation in
            PHPhotoLibrary.requestAuthorization(for: .readWrite) { status in
                continuation.resume(returning: status)
            }
        }
        let mapped = AssetAuthorizationStatus(photoKit: status)
        CapsuleLog.assetKit.info("photo library authorization: \(String(describing: mapped), privacy: .public)")
        return mapped
    }

    public func loadTimeline() async throws -> any AssetSnapshot {
        guard await authorizationStatus().isUsable else {
            throw ProviderError.notAuthorized
        }
        let options = PHFetchOptions()
        options.predicate = NSPredicate(
            format: "mediaType == %d OR mediaType == %d",
            PHAssetMediaType.image.rawValue,
            PHAssetMediaType.video.rawValue
        )
        options.sortDescriptors = [NSSortDescriptor(key: "creationDate", ascending: false)]
        let result = PHAsset.fetchAssets(with: options)
        state.withLock { $0.fetchResult = result }
        CapsuleLog.assetKit.info("loaded photo timeline: \(result.count) assets")
        return PhotoKitSnapshot(result)
    }

    public func asset(for id: AssetID) async throws -> Asset? {
        guard case let .photoKit(localIdentifier) = id else { return nil }
        let result = PHAsset.fetchAssets(withLocalIdentifiers: [localIdentifier], options: nil)
        return result.firstObject.map(Asset.init(photoKitAsset:))
    }

    public func changes() -> AsyncStream<AssetChange> {
        AsyncStream { continuation in
            let token = UUID()
            registerObserverIfNeeded()
            state.withLock { $0.observers[token] = continuation }
            continuation.onTermination = { [weak self] _ in
                self?.state.withLock { $0.observers[token] = nil }
            }
        }
    }

    public func setFavorite(_ isFavorite: Bool, for id: AssetID) async throws {
        guard case let .photoKit(localIdentifier) = id else { return }
        // The asset is re-fetched inside the change block so only the
        // (`Sendable`) identifier is captured across the boundary.
        try await library.performChanges {
            guard let phAsset = PHAsset.fetchAssets(
                withLocalIdentifiers: [localIdentifier], options: nil
            ).firstObject else { return }
            PHAssetChangeRequest(for: phAsset).isFavorite = isFavorite
        }
    }

    public func delete(_ ids: [AssetID]) async throws {
        let localIdentifiers = ids.compactMap { id -> String? in
            guard case let .photoKit(localIdentifier) = id else { return nil }
            return localIdentifier
        }
        guard !localIdentifiers.isEmpty else { return }
        try await library.performChanges {
            let phAssets = PHAsset.fetchAssets(withLocalIdentifiers: localIdentifiers, options: nil)
            PHAssetChangeRequest.deleteAssets(phAssets)
        }
    }

    public func locations(for ids: [AssetID]) async -> [AssetID: AssetCoordinate] {
        let localIdentifiers = ids.compactMap { id -> String? in
            guard case let .photoKit(localIdentifier) = id else { return nil }
            return localIdentifier
        }
        guard !localIdentifiers.isEmpty else { return [:] }
        let result = PHAsset.fetchAssets(withLocalIdentifiers: localIdentifiers, options: nil)
        var map: [AssetID: AssetCoordinate] = [:]
        result.enumerateObjects { asset, _, _ in
            guard let location = asset.location else { return }
            map[.photoKit(localIdentifier: asset.localIdentifier)] = AssetCoordinate(
                latitude: location.coordinate.latitude,
                longitude: location.coordinate.longitude
            )
        }
        return map
    }

    // MARK: PHPhotoLibraryChangeObserver

    public func photoLibraryDidChange(_ changeInstance: PHChange) {
        let (observers, change) = state.withLock { state -> ([AsyncStream<AssetChange>.Continuation], AssetChange?) in
            guard let current = state.fetchResult,
                  let details = changeInstance.changeDetails(for: current)
            else {
                return ([], nil)
            }
            let after = details.fetchResultAfterChanges
            state.fetchResult = after

            var moves: [AssetMove] = []
            details.enumerateMoves { fromIndex, toIndex in
                moves.append(AssetMove(origin: fromIndex, destination: toIndex))
            }
            let change = AssetChange(
                snapshot: PhotoKitSnapshot(after),
                removed: details.removedIndexes ?? [],
                inserted: details.insertedIndexes ?? [],
                changed: details.changedIndexes ?? [],
                moves: moves,
                isIncremental: details.hasIncrementalChanges
            )
            return (Array(state.observers.values), change)
        }

        guard let change else { return }
        CapsuleLog.assetKit.debug("photo library changed: \(change.snapshot.count) assets, incremental=\(change.isIncremental)")
        for observer in observers {
            observer.yield(change)
        }
    }

    // MARK: Private

    private func registerObserverIfNeeded() {
        let shouldRegister = state.withLock { state -> Bool in
            guard !state.isRegistered else { return false }
            state.isRegistered = true
            return true
        }
        if shouldRegister {
            library.register(self)
        }
    }
}
