import Foundation

/// An immutable, ordered view of a set of assets at a point in time.
///
/// A snapshot is what a provider hands the grid: it exposes a `count` and
/// O(1) indexed access, but never a materialised `[Asset]`. A PhotoKit-backed
/// snapshot wraps a lazy `PHFetchResult`; a managed snapshot wraps a catalog
/// row window. This keeps a 200k-photo timeline at near-zero resident memory —
/// the grid only ever realises the few `Asset` values currently on screen.
///
/// Conformers are `Sendable` and immutable: mutations arrive as a *new*
/// snapshot inside an ``AssetChange``.
public protocol AssetSnapshot: Sendable {
    /// The number of assets in the snapshot.
    var count: Int { get }

    /// The asset at `index`. O(1). `index` must be in `0..<count`.
    func asset(at index: Int) -> Asset
}

public extension AssetSnapshot {
    /// Whether the snapshot has no assets. (`count` is the protocol's only
    /// requirement, so this cannot be rewritten as an `isEmpty` check.)
    var isEmpty: Bool { count == 0 } // swiftlint:disable:this empty_count

    /// The asset at `index`, or `nil` if `index` is out of bounds.
    func assetIfPresent(at index: Int) -> Asset? {
        (0 ..< count).contains(index) ? asset(at: index) : nil
    }
}

/// A simple array-backed ``AssetSnapshot``.
///
/// Used for the managed store's bounded result windows, for SwiftUI previews,
/// and as the snapshot returned by test mocks. PhotoKit instead uses its own
/// lazy `PHFetchResult`-backed snapshot (Phase 2).
public struct InMemoryAssetSnapshot: AssetSnapshot {
    private let assets: [Asset]

    public init(_ assets: [Asset]) {
        self.assets = assets
    }

    public var count: Int { assets.count }

    public func asset(at index: Int) -> Asset { assets[index] }

    /// The backing assets as an array — for tests and diffing only.
    public var allAssets: [Asset] { assets }
}
