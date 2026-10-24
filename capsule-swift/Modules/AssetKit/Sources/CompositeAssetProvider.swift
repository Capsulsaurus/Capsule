import CapsuleFoundation
import Foundation

/// An ``AssetProvider`` that merges several providers into one chronological
/// timeline — the hybrid model's unification point.
///
/// The app sees a single timeline that interleaves the system Photos library
/// and the Capsule-managed store by capture date. A provider that fails to
/// load (e.g. PhotoKit when access is denied) is skipped, so the rest still
/// appear; when none of the rest has anything to show, the failure is thrown
/// rather than reported as an empty library. Per-asset operations fan out to every provider; only the one that
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

    /// Every provider's timeline, merged newest first.
    ///
    /// - Throws: the first provider's error when no provider produced a
    ///   non-empty timeline, so a source that failed is never reported as an
    ///   empty library.
    public func loadTimeline() async throws -> any AssetSnapshot {
        var results: [Result<any AssetSnapshot, any Error>] = []
        for provider in providers {
            do {
                let snapshot = try await provider.loadTimeline()
                results.append(.success(snapshot))
            } catch {
                results.append(.failure(error))
            }
        }
        return try Self.merge(results)
    }

    public func asset(for id: AssetID) async throws -> Asset? {
        for provider in providers {
            if let asset = try? await provider.asset(for: id) {
                return asset
            }
        }
        return nil
    }

    /// A merged reload whenever any provider publishes a change.
    ///
    /// The changing provider's own published snapshot is merged as-is rather
    /// than re-read: a provider's change already carries its new state, and the
    /// port-backed provider builds and warms a fresh paged snapshot for every
    /// change it publishes, so re-reading it would pay that twice. Every other
    /// provider contributes the last snapshot it published on this stream, or
    /// its `loadTimeline()` until it has published one. A reload that would
    /// have to report a failed source as an empty library is not published.
    public func changes() -> AsyncStream<AssetChange> {
        let providers = providers
        return AsyncStream { continuation in
            let latest = LatestTimelines(providers: providers)
            let task = Task {
                await withTaskGroup(of: Void.self) { group in
                    for (index, provider) in providers.enumerated() {
                        group.addTask {
                            for await change in provider.changes() {
                                guard !Task.isCancelled else { return }
                                guard let merged = try? await latest.merging(change.snapshot, from: index) else {
                                    continue
                                }
                                continuation.yield(.reload(merged))
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

    /// Merge per-provider load results into one timeline, newest first.
    ///
    /// A provider that failed is skipped while another provider has assets to
    /// show (PhotoKit denied beside a populated managed store). When none has,
    /// the first failure is rethrown: an empty timeline in its place would show
    /// a source that could not be read as a library with nothing in it.
    ///
    /// When exactly one provider has any assets, its snapshot is returned
    /// **unchanged** rather than copied, which keeps the provider's own snapshot
    /// type and skips an O(n) copy and sort the result would not need. Merging
    /// two non-empty sources copies every row of both into one array; a lazy
    /// snapshot such as the port-backed one answers each row it has not paged
    /// in yet with a provisional stand-in, and the copy keeps that stand-in.
    fileprivate static func merge(_ results: [Result<any AssetSnapshot, any Error>]) throws -> any AssetSnapshot {
        var snapshots: [any AssetSnapshot] = []
        var firstFailure: (any Error)?
        for result in results {
            switch result {
            case let .success(snapshot):
                if !snapshot.isEmpty { snapshots.append(snapshot) }
            case let .failure(error):
                CapsuleLog.assetKit.error(
                    "composite timeline source failed: \(String(describing: error), privacy: .public)"
                )
                firstFailure = firstFailure ?? error
            }
        }
        if snapshots.isEmpty, let firstFailure {
            throw firstFailure
        }
        if snapshots.count <= 1 {
            return snapshots.first ?? InMemoryAssetSnapshot([])
        }
        CapsuleLog.assetKit.debug("merging \(snapshots.count, privacy: .public) non-empty timelines")
        var assets: [Asset] = []
        for snapshot in snapshots {
            for index in 0 ..< snapshot.count {
                assets.append(snapshot.asset(at: index))
            }
        }
        assets.sort { $0.captureDate > $1.captureDate }
        return InMemoryAssetSnapshot(assets)
    }
}

// MARK: - LatestTimelines

/// The last snapshot each provider published on one composite change stream.
///
/// One per ``CompositeAssetProvider/changes()`` stream, so a published change
/// is merged with what the other providers last said instead of re-reading
/// every provider. A provider that has published nothing yet is read once
/// through `loadTimeline()`; a read that fails is retried on the next change.
private actor LatestTimelines {
    private let providers: [any AssetProvider]
    private var snapshots: [(any AssetSnapshot)?]

    init(providers: [any AssetProvider]) {
        self.providers = providers
        snapshots = Array(repeating: nil, count: providers.count)
    }

    /// Record `snapshot` as provider `index`'s state and merge it with the rest.
    func merging(_ snapshot: any AssetSnapshot, from index: Int) async throws -> any AssetSnapshot {
        snapshots[index] = snapshot
        var failures: [Int: any Error] = [:]
        for position in providers.indices where snapshots[position] == nil {
            do {
                let loaded = try await providers[position].loadTimeline()
                // A change published while this read was suspended is newer.
                if snapshots[position] == nil { snapshots[position] = loaded }
            } catch {
                failures[position] = error
            }
        }
        let results: [Result<any AssetSnapshot, any Error>] = providers.indices.map { position in
            if let known = snapshots[position] { return .success(known) }
            return .failure(failures[position] ?? CancellationError())
        }
        return try CompositeAssetProvider.merge(results)
    }
}
