import AssetKit
import CapsuleFoundation
import Foundation

/// An in-memory ``AssetProvider`` for testing feature view models.
///
/// It holds a fixed array of assets, a settable authorization status, and one
/// change-stream observer that tests drive with ``emit(_:)`` / ``setAssets(_:)``.
public actor MockAssetProvider: AssetProvider {
    /// Errors the mock raises.
    public enum MockError: Error, Sendable {
        /// `loadTimeline()` was called while not authorized.
        case notAuthorized
    }

    private var assets: [Asset]
    private var status: AssetAuthorizationStatus
    private var continuation: AsyncStream<AssetChange>.Continuation?
    /// The number of times ``requestAuthorization()`` has been called.
    public private(set) var authorizationRequestCount = 0

    public init(assets: [Asset] = [], status: AssetAuthorizationStatus = .authorized) {
        self.assets = assets
        self.status = status
    }

    // MARK: AssetProvider

    public func authorizationStatus() -> AssetAuthorizationStatus {
        status
    }

    @discardableResult
    public func requestAuthorization() -> AssetAuthorizationStatus {
        authorizationRequestCount += 1
        if status == .notDetermined { status = .authorized }
        return status
    }

    public func loadTimeline() throws -> any AssetSnapshot {
        guard status.isUsable else { throw MockError.notAuthorized }
        return InMemoryAssetSnapshot(assets)
    }

    public func asset(for id: AssetID) -> Asset? {
        assets.first { $0.id == id }
    }

    public nonisolated func changes() -> AsyncStream<AssetChange> {
        AsyncStream { continuation in
            Task { await self.attach(continuation) }
        }
    }

    public func setFavorite(_ isFavorite: Bool, for id: AssetID) {
        guard let index = assets.firstIndex(where: { $0.id == id }) else { return }
        assets[index].isFavorite = isFavorite
        continuation?.yield(.reload(InMemoryAssetSnapshot(assets)))
    }

    public func delete(_ ids: [AssetID]) {
        let removed = Set(ids)
        assets.removeAll { removed.contains($0.id) }
        continuation?.yield(.reload(InMemoryAssetSnapshot(assets)))
    }

    // MARK: Test configuration

    /// Replace the authorization status returned by the mock.
    public func setAuthorizationStatus(_ status: AssetAuthorizationStatus) {
        self.status = status
    }

    /// Replace the asset set and emit a non-incremental reload to the observer.
    public func setAssets(_ assets: [Asset]) {
        self.assets = assets
        continuation?.yield(.reload(InMemoryAssetSnapshot(assets)))
    }

    /// Emit a change to the current observer, if any.
    public func emit(_ change: AssetChange) {
        continuation?.yield(change)
    }

    private func attach(_ continuation: AsyncStream<AssetChange>.Continuation) {
        self.continuation = continuation
    }
}
