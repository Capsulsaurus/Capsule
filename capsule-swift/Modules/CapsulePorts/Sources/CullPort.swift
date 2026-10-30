import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - CullAsset

/// One asset as a cull pass needs it: its id, its flag, and where it sits in
/// its stack (*Organization — Culling*).
///
/// Deliberately narrower than ``LibraryAsset``: a pass groups, orders and
/// flags, and an adapter that reads only the workspace's cull verbs has no
/// capture time or format to report. The photo itself is drawn from ``id``.
public struct CullAsset: Sendable, Equatable, Hashable, Identifiable {
    /// The asset.
    public var id: AssetID
    /// Its current flag (``CullFlag/neutral`` when never flagged).
    public var cull: CullFlag
    /// Where it sits in its stack, or `nil` for an asset in no stack.
    public var stack: CullStackPlace?

    public init(id: AssetID, cull: CullFlag = .neutral, stack: CullStackPlace? = nil) {
        self.id = id
        self.cull = cull
        self.stack = stack
    }

    /// The pass's view of a library row.
    public init(_ asset: LibraryAsset) {
        self.init(id: asset.id, cull: asset.cull, stack: asset.stackMembership.map(CullStackPlace.init))
    }
}

/// One asset's place in its stack, as a cull pass orders it: cover first,
/// then by member index.
public struct CullStackPlace: Sendable, Equatable, Hashable {
    /// The stack the asset belongs to.
    public var stackID: StackID
    /// Whether it is the stack's cover: its primary member, the "best photo".
    public var isCover: Bool
    /// Its position within the stack, when one was recorded.
    public var memberIndex: UInt32?

    public init(stackID: StackID, isCover: Bool, memberIndex: UInt32? = nil) {
        self.stackID = stackID
        self.isCover = isCover
        self.memberIndex = memberIndex
    }

    /// The place a full stack membership records.
    public init(_ membership: StackMembership) {
        self.init(stackID: membership.stackID, isCover: membership.isStackCover, memberIndex: membership.memberIndex)
    }
}

// MARK: - CullPort

/// What the cull pass reads and writes: the assets of a sequence with their
/// flags and stack places, a flag on assets, a flag on a whole stack, and the
/// sweep of the rejects to the trash.
///
/// Every write but the sweep rewrites the `cull` register and never touches an
/// original's bytes. A stack has no stored flag: flagging one flags every
/// member it holds.
public protocol CullPort: Sendable {
    /// The pass over `query`, in batches of at most `batchSize` assets, in
    /// sequence order. Stack members arrive whether or not their stack is
    /// collapsed: the pass groups them itself.
    ///
    /// The stream finishes with ``CullPortRefusal/unsupportedSequence`` when
    /// the adapter cannot express `query`, and with the adapter's own error
    /// when a read fails.
    func pass(over query: TimelineQuery, batchSize: Int) -> AsyncThrowingStream<[CullAsset], any Error>

    /// Flag each asset.
    func setCull(_ flag: CullFlag, for assetIDs: [AssetID]) async throws

    /// Flag every member of a stack, and return the members flagged.
    func flagStack(_ stackID: StackID, as flag: CullFlag) async throws -> [AssetID]

    /// Move exactly these assets to the trash, each with the default signed
    /// retention window. Moving an asset already in the trash is not an error
    /// and does not extend its window, so a sweep that failed partway is
    /// retried whole.
    func moveToTrash(_ assetIDs: [AssetID]) async throws
}

/// Why a ``CullPort`` refused a request without reaching its store.
public enum CullPortRefusal: Error, Equatable, Sendable {
    /// The adapter cannot read the pass over this sequence: a filter it has no
    /// verb for.
    case unsupportedSequence
    /// The asset or stack is not one this adapter's store holds.
    case notFound
    /// The flag is one this build cannot name, so it is never written.
    case unknownFlag
}
