import CapsuleFoundation
import Foundation

/// Whether the app may read a given asset source.
///
/// The managed store needs no system permission and is always `.authorized`;
/// PhotoKit maps the system `PHAuthorizationStatus`, where `.limited` means
/// the user granted access to a hand-picked subset of their library.
public enum AssetAuthorizationStatus: Sendable, Equatable {
    case notDetermined
    case restricted
    case denied
    case authorized
    case limited

    /// Whether the source can be read at all (full or limited access).
    public var isUsable: Bool {
        self == .authorized || self == .limited
    }
}

/// A single asset that moved position between two snapshots.
public struct AssetMove: Sendable, Equatable {
    /// The asset's index in the previous snapshot.
    public var origin: Int
    /// The asset's index in the new snapshot.
    public var destination: Int

    public init(origin: Int, destination: Int) {
        self.origin = origin
        self.destination = destination
    }
}

/// An update to a provider's timeline.
///
/// ``snapshot`` is always the new, authoritative state. When ``isIncremental``
/// is `true` the index sets describe the delta against the *previous*
/// snapshot, so a `UICollectionViewDiffableDataSource` can animate the change;
/// when `false` the consumer should reload wholesale.
public struct AssetChange: Sendable {
    /// The new timeline state.
    public var snapshot: any AssetSnapshot
    /// Indices removed from the previous snapshot.
    public var removed: IndexSet
    /// Indices inserted into the new snapshot.
    public var inserted: IndexSet
    /// Indices whose asset changed in place.
    public var changed: IndexSet
    /// Assets that moved position.
    public var moves: [AssetMove]
    /// Whether the index sets form a usable delta (else: reload wholesale).
    public var isIncremental: Bool

    public init(
        snapshot: any AssetSnapshot,
        removed: IndexSet = [],
        inserted: IndexSet = [],
        changed: IndexSet = [],
        moves: [AssetMove] = [],
        isIncremental: Bool = false
    ) {
        self.snapshot = snapshot
        self.removed = removed
        self.inserted = inserted
        self.changed = changed
        self.moves = moves
        self.isIncremental = isIncremental
    }

    /// A non-incremental change — the consumer reloads from `snapshot`.
    public static func reload(_ snapshot: any AssetSnapshot) -> AssetChange {
        AssetChange(snapshot: snapshot, isIncremental: false)
    }
}

/// A source of timeline assets.
///
/// The app sees its photos through this one abstraction, whatever the backing
/// source: `PhotoKitProvider` over the system library, `ManagedProvider` over
/// the Capsule store, and `CompositeAssetProvider` merging both into a single
/// chronological timeline. Feature view models depend only on this protocol,
/// and are tested against `MockAssetProvider`.
public protocol AssetProvider: Sendable {
    /// The current authorization status, without prompting.
    func authorizationStatus() async -> AssetAuthorizationStatus

    /// Request access, prompting the user if the status is undetermined.
    /// Returns the resulting status.
    @discardableResult
    func requestAuthorization() async -> AssetAuthorizationStatus

    /// Load the current timeline as an ordered snapshot, newest first.
    ///
    /// - Throws: if the source is not authorized or cannot be read.
    func loadTimeline() async throws -> any AssetSnapshot

    /// Resolve a single asset by identifier, or `nil` if it no longer exists.
    func asset(for id: AssetID) async throws -> Asset?

    /// A stream of timeline updates for as long as the returned stream is held.
    func changes() -> AsyncStream<AssetChange>

    /// Set an asset's favourite flag in its backing source.
    func setFavorite(_ isFavorite: Bool, for id: AssetID) async throws

    /// Delete assets from their backing source.
    ///
    /// For the system Photos library this presents the standard deletion
    /// confirmation; the deletion completes only if the user confirms.
    func delete(_ ids: [AssetID]) async throws
}
