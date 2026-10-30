import CapsuleDomain
import CapsuleFoundation
import CapsuleNavigation
import Foundation

// MARK: - CullVerb

/// Everything a cull pass can be asked to do, named once so the keyboard, the
/// on-screen buttons and the accessibility actions all reach the same verb.
enum CullVerb: CaseIterable, Sendable {
    case pick
    case neutral
    case reject
    case next
    case previous
    case expandStack
    case collapseStack
    case sweep

    /// The flag a verdict writes, or `nil` for a verb that is not a verdict.
    var flag: CullFlag? {
        switch self {
        case .pick: .pick
        case .neutral: .neutral
        case .reject: .reject
        default: nil
        }
    }

    /// The verb a menu or keyboard action asks the culling surface for, or
    /// `nil` for an action that is not the surface's.
    init?(action: NavigationAction) {
        switch action {
        case .cullPick: self = .pick
        case .cullNeutral: self = .neutral
        case .cullReject: self = .reject
        case .nextAsset: self = .next
        case .previousAsset: self = .previous
        case .expandStack: self = .expandStack
        case .collapseStack: self = .collapseStack
        case .rejectSweep: self = .sweep
        default: return nil
        }
    }
}

// MARK: - CullGroup

/// One unit of the pass: a lone asset, or a stack shown by its cover.
///
/// A stack has no flag of its own (*Organization — Culling*): its state is
/// derived from the members every time it is read, so it cannot diverge from
/// them.
struct CullGroup: Equatable, Sendable {
    /// The stack the members share, or `nil` for a lone asset.
    let stackID: StackID?
    /// Cover first, then by `member_index`, then in the order they arrived.
    var members: [LibraryAsset]
    /// Whether the stack's members are culled one by one rather than through
    /// the cover.
    var isExpanded = false

    /// A stack with more than one member — the only kind there is anything to
    /// expand.
    var isStack: Bool { stackID != nil && members.count > 1 }

    /// What the collapsed group shows.
    var cover: LibraryAsset { members[0] }

    /// The derived state: all-rejected, then any-pick, else mixed.
    var cullState: GroupCullState { GroupCullState(members: members.map(\.cull)) }
}

// MARK: - CullStep

/// One position in the pass: a whole group, or one member of an expanded stack.
struct CullStep: Equatable, Sendable {
    let group: Int
    /// `nil` when the step is the whole group — a lone asset, or a collapsed
    /// stack, which a verdict flags member by member.
    let member: Int?
}

/// The flag state a step shows: an asset's own flag, or a collapsed stack's
/// derived state and size.
enum CullStepFlag: Equatable, Sendable {
    case asset(CullFlag)
    case stack(GroupCullState, memberCount: Int)
}

// MARK: - CullingPass

/// The cull pass over one sequence of assets, as a value: where the user is,
/// which stacks are open, and the flags as the user last set them.
///
/// Pure on purpose. Writing a flag, moving to the trash and announcing the
/// result are the surface's business; this decides *what* each verb does to
/// the pass and hands back the asset ids a write must reach, so every rule
/// below is a unit test rather than a tap.
///
/// - A verdict on a collapsed stack flags every member, and any verdict
///   advances to the next step (the last step stays put).
/// - Expanding a stack replaces its one step with a step per member, starting
///   on the cover; collapsing folds them back and lands on the stack.
/// - The sweep candidates are the rejects in the pass, and nothing outside it.
struct CullingPass: Equatable, Sendable {
    /// The library query a pass over `context` reads, or `nil` for a sequence
    /// no library query can express yet.
    ///
    /// Stack members are always included, collapsed or not: the pass groups
    /// them itself, and a verdict on a collapsed stack has to reach every one.
    /// Only the timeline and an album resolve, for the reason the viewer's route
    /// gives: a person, place, smart album or search sequence each needs its own
    /// collection query, and those keep the scaffold until one exists.
    static func query(for context: ViewerContext) -> TimelineQuery? {
        switch context {
        case var .timeline(query):
            query.includeStackHidden = true
            return query
        case let .album(albumID):
            return TimelineQuery(albumID: albumID, includeStackHidden: true)
        default:
            return nil
        }
    }

    private(set) var groups: [CullGroup] = []
    private(set) var steps: [CullStep] = []
    /// The current step. Always a valid index into ``steps`` unless the pass is
    /// empty.
    private(set) var index = 0
    /// Where each stack's group sits, so a page that brings another member of
    /// an already-seen stack joins it in constant time.
    private var stackGroups: [StackID: Int] = [:]

    init(assets: [LibraryAsset] = []) {
        append(assets)
    }

    // MARK: Reading

    var isEmpty: Bool { steps.isEmpty }

    /// The current step, or `nil` for an empty pass.
    var current: CullStep? { steps.indices.contains(index) ? steps[index] : nil }

    /// The asset the current step shows.
    var currentAsset: LibraryAsset? { current.map(asset(at:)) }

    /// The current step's flag state.
    var currentFlag: CullStepFlag? {
        guard let step = current else { return nil }
        let group = groups[step.group]
        if step.member == nil, group.isStack {
            return .stack(group.cullState, memberCount: group.members.count)
        }
        return .asset(asset(at: step).cull)
    }

    /// 1-based position of the current step, and the number of steps.
    var position: (index: Int, count: Int) { (isEmpty ? 0 : index + 1, steps.count) }

    /// Every rejected asset in the pass, in pass order.
    var sweepCandidates: [AssetID] {
        groups.flatMap { $0.members.filter { $0.cull == .reject }.map(\.id) }
    }

    /// Whether `verb` would do anything from here — what a menu item and a
    /// button ask before they offer it.
    func accepts(_ verb: CullVerb) -> Bool {
        guard let step = current else { return false }
        switch verb {
        case .pick, .neutral, .reject: return true
        case .next: return index < steps.count - 1
        case .previous: return index > 0
        case .expandStack: return step.member == nil && groups[step.group].isStack
        case .collapseStack: return step.member != nil
        case .sweep: return groups.contains { $0.members.contains { $0.cull == .reject } }
        }
    }

    /// The current flags of `ids`, for undoing a verdict whose write failed.
    func flags(of ids: [AssetID]) -> [AssetID: CullFlag] {
        let wanted = Set(ids)
        var found: [AssetID: CullFlag] = [:]
        for member in groups.lazy.flatMap(\.members) where wanted.contains(member.id) {
            found[member.id] = member.cull
        }
        return found
    }

    // MARK: Building

    /// Add assets in sequence order. A member of a stack already in the pass
    /// joins it wherever it arrives, so a stack split across two pages is still
    /// one step.
    mutating func append(_ assets: [LibraryAsset]) {
        let anchor = currentAsset?.id
        for asset in assets {
            guard let stackID = asset.stackMembership?.stackID else {
                groups.append(CullGroup(stackID: nil, members: [asset]))
                continue
            }
            if let at = stackGroups[stackID] {
                Self.insert(asset, into: &groups[at].members)
            } else {
                stackGroups[stackID] = groups.count
                groups.append(CullGroup(stackID: stackID, members: [asset]))
            }
        }
        rebuild(anchoredOn: anchor)
    }

    /// Drop assets that left the pass — swept to the trash. A stack left with
    /// no members leaves with them.
    mutating func remove(_ ids: Set<AssetID>) {
        guard !ids.isEmpty else { return }
        let anchor = currentAsset?.id
        for at in groups.indices {
            groups[at].members.removeAll { ids.contains($0.id) }
        }
        groups.removeAll { $0.members.isEmpty }
        stackGroups = [:]
        for (at, group) in groups.enumerated() {
            if let stackID = group.stackID { stackGroups[stackID] = at }
        }
        rebuild(anchoredOn: anchor)
    }

    // MARK: Verbs

    /// Apply a verdict to the current step and advance.
    ///
    /// - Returns: the assets whose flag the surface must write — every member of
    ///   a collapsed stack, else the one asset. Empty for an empty pass.
    mutating func verdict(_ flag: CullFlag) -> [AssetID] {
        guard let step = current else { return [] }
        let targets = verdictTargets
        let reached = Set(targets)
        for member in groups[step.group].members.indices where reached.contains(groups[step.group].members[member].id) {
            groups[step.group].members[member].cull = flag
        }
        _ = next()
        return targets
    }

    /// The assets a verdict on the current step writes: every member of a
    /// collapsed stack, else the one asset. Read before a verdict, so a write
    /// that fails can put back exactly what it overwrote.
    var verdictTargets: [AssetID] {
        guard let step = current else { return [] }
        let members = groups[step.group].members
        return step.member.map { [members[$0].id] } ?? members.map(\.id)
    }

    /// Put flags back after their write failed. Position is left alone: the
    /// user is told, and is already looking at the next asset.
    mutating func restore(_ flags: [AssetID: CullFlag]) {
        for group in groups.indices {
            for member in groups[group].members.indices {
                if let flag = flags[groups[group].members[member].id] {
                    groups[group].members[member].cull = flag
                }
            }
        }
    }

    /// Move to the next step. `false` at the end of the pass.
    mutating func next() -> Bool {
        guard index < steps.count - 1 else { return false }
        index += 1
        return true
    }

    /// Move to the previous step. `false` at the start of the pass.
    mutating func previous() -> Bool {
        guard index > 0 else { return false }
        index -= 1
        return true
    }

    /// Open the collapsed stack under review, landing on its cover.
    mutating func expandStack() -> Bool {
        guard accepts(.expandStack), let step = current else { return false }
        groups[step.group].isExpanded = true
        rebuild(anchoredOn: nil)
        index = steps.firstIndex(of: CullStep(group: step.group, member: 0)) ?? index
        return true
    }

    /// Fold the expanded stack the current member belongs to, landing on it.
    mutating func collapseStack() -> Bool {
        guard accepts(.collapseStack), let step = current else { return false }
        groups[step.group].isExpanded = false
        rebuild(anchoredOn: nil)
        index = steps.firstIndex(of: CullStep(group: step.group, member: nil)) ?? index
        return true
    }

    // MARK: Internals

    func asset(at step: CullStep) -> LibraryAsset {
        let group = groups[step.group]
        return step.member.map { group.members[$0] } ?? group.cover
    }

    /// Recompute the steps, keeping the user on the asset they were looking at
    /// when it is still in the pass, and otherwise on the step that took its
    /// place.
    private mutating func rebuild(anchoredOn anchor: AssetID?) {
        steps = groups.indices.flatMap { at -> [CullStep] in
            let group = groups[at]
            guard group.isStack, group.isExpanded else { return [CullStep(group: at, member: nil)] }
            return group.members.indices.map { CullStep(group: at, member: $0) }
        }
        if let anchor, let found = steps.firstIndex(where: { asset(at: $0).id == anchor }) {
            index = found
        } else {
            index = min(index, max(steps.count - 1, 0))
        }
    }

    /// Insert a stack member in cover-first, `member_index` order, after any
    /// member that sorts equal, so arrival order breaks ties.
    private static func insert(_ asset: LibraryAsset, into members: inout [LibraryAsset]) {
        let key = order(of: asset)
        let at = members.firstIndex { order(of: $0) > key } ?? members.endIndex
        members.insert(asset, at: at)
    }

    private static func order(of asset: LibraryAsset) -> Order {
        let membership = asset.stackMembership
        return Order(
            isCover: membership?.isStackCover ?? false,
            memberIndex: membership?.memberIndex ?? .max
        )
    }

    private struct Order: Comparable {
        let isCover: Bool
        let memberIndex: UInt32

        static func < (lhs: Order, rhs: Order) -> Bool {
            if lhs.isCover != rhs.isCover { return lhs.isCover }
            return lhs.memberIndex < rhs.memberIndex
        }
    }
}
