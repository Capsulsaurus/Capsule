import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - MockLabelStore

/// Region, stack and album labels, and the regions themselves, in memory.
///
/// Refuses what the core's workspace refuses, in the same order: a label
/// write refuses a label the rule refuses before an unknown target, and a
/// region write refuses a refused first label, then a shape outside the
/// normalized-coordinate contract, then an unknown asset (every first label
/// is checked before the workspace's `add_region` writes the region) — and
/// writes nothing on any refusal. Region ids are minted deterministically from the seed, so two environments built
/// alike mint the same ids, and a deleted id is never minted again.
///
/// Whether an asset, stack or album exists is the library's answer, asked
/// through ``Existence``, so a label can only land on something the timeline
/// shows.
public actor MockLabelStore: LabelPort {
    /// Whether the library holds an asset, a stack, or a container album.
    public struct Existence: Sendable {
        public var asset: @Sendable (AssetID) async -> Bool
        public var stack: @Sendable (StackID) async -> Bool
        public var album: @Sendable (AlbumID) async -> Bool

        public init(
            asset: @escaping @Sendable (AssetID) async -> Bool,
            stack: @escaping @Sendable (StackID) async -> Bool,
            album: @escaping @Sendable (AlbumID) async -> Bool
        ) {
            self.asset = asset
            self.stack = stack
            self.album = album
        }

        /// Every id exists: for a test of the store's own rules.
        public static let everything = Existence(asset: { _ in true }, stack: { _ in true }, album: { _ in true })
    }

    private let existence: Existence
    private let seed: UInt64
    private var minted = 0
    private var regions: [AssetID: [AssetRegion]] = [:]
    private var stackLabels: [StackID: Set<String>] = [:]
    private var albumLabels: [AlbumID: Set<String>] = [:]

    public init(existence: Existence, seed: UInt64) {
        self.existence = existence
        self.seed = seed
    }

    // MARK: LabelPort

    public func labels(on target: LabelTarget) async throws -> [String] {
        try await requireExists(target)
        return current(target)
    }

    public func addLabel(_ label: String, to target: LabelTarget) async throws -> [String] {
        let normalized = try UserLabel.normalize(label)
        try await requireExists(target)
        update(target) { $0.insert(normalized) }
        return current(target)
    }

    public func removeLabel(_ label: String, from target: LabelTarget) async throws -> [String] {
        let normalized = try UserLabel.normalize(label)
        try await requireExists(target)
        update(target) { $0.remove(normalized) }
        return current(target)
    }

    public func regions(of assetID: AssetID) async throws -> [AssetRegion] {
        guard await existence.asset(assetID) else { throw LabelTargetRefusal.notFound }
        return regions[assetID] ?? []
    }

    public func addRegion(_ shape: RegionShape, labels: [String], to assetID: AssetID) async throws -> AssetRegion {
        let normalized = try labels.map { try UserLabel.normalize($0) }
        try shape.validate()
        guard await existence.asset(assetID) else { throw LabelTargetRefusal.notFound }
        let region = AssetRegion(
            id: MockIdentifiers.regionID(seed: seed, ordinal: minted),
            shape: shape,
            labels: Set(normalized).sorted()
        )
        minted += 1
        regions[assetID, default: []].append(region)
        return region
    }

    public func deleteRegion(_ regionID: RegionID, on assetID: AssetID) async throws {
        guard await existence.asset(assetID),
              let index = regions[assetID]?.firstIndex(where: { $0.id == regionID })
        else { throw LabelTargetRefusal.notFound }
        regions[assetID]?.remove(at: index)
    }

    // MARK: Storage

    private func requireExists(_ target: LabelTarget) async throws {
        let exists = switch target {
        case let .region(assetID, regionID):
            await existence.asset(assetID) && regions[assetID]?.contains { $0.id == regionID } == true
        case let .stack(stackID):
            await existence.stack(stackID)
        case let .album(albumID):
            await existence.album(albumID)
        }
        guard exists else { throw LabelTargetRefusal.notFound }
    }

    private func current(_ target: LabelTarget) -> [String] {
        switch target {
        case let .region(assetID, regionID):
            regions[assetID]?.first { $0.id == regionID }?.labels ?? []
        case let .stack(stackID):
            (stackLabels[stackID] ?? []).sorted()
        case let .album(albumID):
            (albumLabels[albumID] ?? []).sorted()
        }
    }

    private func update(_ target: LabelTarget, _ change: (inout Set<String>) -> Void) {
        switch target {
        case let .region(assetID, regionID):
            guard let index = regions[assetID]?.firstIndex(where: { $0.id == regionID }),
                  var region = regions[assetID]?[index]
            else { return }
            var labels = Set(region.labels)
            change(&labels)
            region.labels = labels.sorted()
            regions[assetID]?[index] = region
        case let .stack(stackID):
            change(&stackLabels[stackID, default: []])
        case let .album(albumID):
            change(&albumLabels[albumID, default: []])
        }
    }
}
