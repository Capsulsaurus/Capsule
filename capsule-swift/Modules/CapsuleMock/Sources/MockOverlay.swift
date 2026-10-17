import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockFieldEdit

/// One field's edit state.
///
/// Three states, not two: "never written", "written to a value", and "written
/// back to absent" are genuinely different, and the domain's CRDT registers
/// depend on the difference — a wire-absent `cull` is not the same as one
/// stamped `neutral`. Collapsing them into a double optional would compile and
/// then read as noise at every call site.
public enum MockFieldEdit<Value: Sendable & Equatable>: Sendable, Equatable {
    case unchanged
    case set(Value)
    case cleared

    /// Apply this edit to a derived value.
    public func applied(to derived: Value?) -> Value? {
        switch self {
        case .unchanged: derived
        case let .set(value): value
        case .cleared: nil
        }
    }
}

// MARK: - MockAssetPatch

/// Everything a user can change about one asset.
///
/// The mock's writes are *real*: a rating set here is a rating every later read
/// returns, and every affected `changes()` stream fires. A mock that silently
/// dropped writes would make every screen a lie — a rating that springs back on
/// scroll is worse than no rating control at all, because it teaches the
/// reviewer to distrust what they see.
public struct MockAssetPatch: Sendable, Equatable {
    public var rating: UInt8?
    public var cull: CullFlag?
    public var isUserHidden: Bool?
    public var isDeleted: Bool?
    public var deletedAt: CapsuleTimestamp?
    public var retentionUntil: CapsuleTimestamp?
    public var isPurged: Bool
    public var caption: MockFieldEdit<String>
    /// Captions this asset's LWW register displaced, newest first.
    public var supersededCaptions: [Stamped<String>]
    public var geolocation: MockFieldEdit<Gps>
    public var albumID: AlbumID?
    /// Tags the user added, keyed by the add id that introduced them.
    public var addedUserTags: [AddID: String]
    /// Derived tags the user removed, named by the add id that introduced them.
    public var removedUserTagIDs: Set<AddID>
    /// AI tags the user dismissed.
    public var dismissedAITagIDs: Set<AddID>
    /// Stack membership edits — a stamped `nil` is leaving a stack, which is
    /// distinct from never having been in one.
    public var stackMembership: MockFieldEdit<StackMembership>
    public var isPinned: Bool
    /// Tiers released or fetched since the asset was derived.
    public var representations: LocalRepresentations?
    public var syncState: AssetSyncState?

    public init() {
        isPurged = false
        caption = .unchanged
        supersededCaptions = []
        geolocation = .unchanged
        addedUserTags = [:]
        removedUserTagIDs = []
        dismissedAITagIDs = []
        stackMembership = .unchanged
        isPinned = false
    }

    /// Whether this patch changes anything a query filter reads. Cheap enough
    /// to check inside the filtered-aggregate loop, which is the only reason it
    /// exists as a separate question.
    public var affectsFacets: Bool {
        rating != nil || cull != nil || isUserHidden != nil || isDeleted != nil
            || isPurged || albumID != nil
    }
}

// MARK: - MockOverlay

/// The mutable half of the library: every user edit, keyed by asset.
///
/// Kept as a value type so the query engine can take a snapshot and evaluate
/// without holding the store's actor for the duration of a 250 000-index scan.
/// It is small by construction — a user edits tens of assets, not hundreds of
/// thousands — so copying it is cheaper than the contention it avoids.
public struct MockOverlay: Sendable, Equatable {
    public private(set) var patches: [AssetID: MockAssetPatch] = [:]
    /// The next OR-set add counter this device will issue.
    ///
    /// Monotonic and never reset: reusing a counter would alias two distinct
    /// adds, so removing one would silently delete the other.
    public private(set) var nextAddCounter: UInt64 = 1000000

    public init() {}

    public func patch(for identifier: AssetID) -> MockAssetPatch? {
        patches[identifier]
    }

    /// Mutate one asset's patch, creating it if this is the first edit.
    public mutating func edit(_ identifier: AssetID, _ body: (inout MockAssetPatch) -> Void) {
        var patch = patches[identifier] ?? MockAssetPatch()
        body(&patch)
        patches[identifier] = patch
    }

    /// Issue a fresh add id for an OR-set insertion.
    public mutating func nextAddID(device: DeviceID) -> AddID {
        let counter = nextAddCounter
        nextAddCounter += 1
        return AddID(deviceID: device, counter: counter)
    }

    /// Every asset the user has explicitly trashed, so the trash view can list
    /// them alongside the derived ones.
    public var userTrashedIdentifiers: [AssetID] {
        patches.filter { $0.value.isDeleted == true && !$0.value.isPurged }.keys.sorted { lhs, rhs in
            lhs.sortKey < rhs.sortKey
        }
    }

    /// Every asset the user has explicitly hidden.
    public var userHiddenIdentifiers: [AssetID] {
        patches.filter { $0.value.isUserHidden == true && $0.value.isPurged == false }.keys
            .sorted { $0.sortKey < $1.sortKey }
    }
}

// MARK: - Sorting

public extension AssetID {
    /// A total, device-independent ordering key, matching
    /// ``LibraryAsset/stableSortKey`` so the overlay and the projection agree.
    var sortKey: String {
        switch self {
        case let .photoKit(localIdentifier): "photokit:\(localIdentifier)"
        case let .managed(uuid): "managed:\(uuid)"
        }
    }
}
