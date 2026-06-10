import CapsuleFoundation
import Foundation

/// An ``AssetProvider`` that merges several providers into one chronological
/// timeline — the hybrid model's unification point.
///
/// The app sees a single timeline that interleaves the system Photos library
/// and the Capsule-managed store by capture date. A provider that fails to
/// load (e.g. PhotoKit when access is denied) is skipped, so the rest still
/// appear. Per-asset operations fan out to every provider; only the one that
/// owns the asset's identifier acts.
public final class CompositeAssetProvider: AssetProvider {
    private let providers: [any AssetProvider]

    public init(providers: [any AssetProvider]) {
        self.providers = providers
    }

    public func authorizationStatus() async -> AssetAuthorizationStatus {
        for provider in providers where await provider.authorizationStatus().isUsable {
            return .authorized
        }
        return .denied
    }

    @discardableResult
    public func requestAuthorization() async -> AssetAuthorizationStatus {
        var anyUsable = false
        for provider in providers where await provider.requestAuthorization().isUsable {
            anyUsable = true
        }
        return anyUsable ? .authorized : .denied
    }

    public func loadTimeline() async throws -> any AssetSnapshot {
        await Self.mergedSnapshot(of: providers)
    }

    public func asset(for id: AssetID) async throws -> Asset? {
        for provider in providers {
            if let asset = try? await provider.asset(for: id) {
                return asset
            }
        }
        return nil
    }

    public func changes() -> AsyncStream<AssetChange> {
        let providers = providers
        return AsyncStream { continuation in
            let task = Task {
                await withTaskGroup(of: Void.self) { group in
                    for provider in providers {
                        group.addTask {
                            for await _ in provider.changes() {
                                let snapshot = await Self.mergedSnapshot(of: providers)
                                continuation.yield(.reload(snapshot))
                            }
                        }
                    }
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    public func setFavorite(_ isFavorite: Bool, for id: AssetID) async throws {
        for provider in providers {
            try await provider.setFavorite(isFavorite, for: id)
        }
    }

    public func delete(_ ids: [AssetID]) async throws {
        for provider in providers {
            try await provider.delete(ids)
        }
    }

    public func locations(for ids: [AssetID]) async -> [AssetID: AssetCoordinate] {
        var merged: [AssetID: AssetCoordinate] = [:]
        for provider in providers {
            let partial = await provider.locations(for: ids)
            merged.merge(partial) { _, new in new }
        }
        return merged
    }

    /// Load every provider's timeline and merge it, newest first. A provider
    /// that throws is skipped.
    private static func mergedSnapshot(of providers: [any AssetProvider]) async -> InMemoryAssetSnapshot {
        var assets: [Asset] = []
        for provider in providers {
            guard let snapshot = try? await provider.loadTimeline() else { continue }
            for index in 0 ..< snapshot.count {
                assets.append(snapshot.asset(at: index))
            }
        }
        assets.sort { $0.captureDate > $1.captureDate }
        return InMemoryAssetSnapshot(assets)
    }
}
