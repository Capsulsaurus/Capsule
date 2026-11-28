import CapsuleDomain
import CapsuleFoundation
import CapsuleNavigation
import CapsulePorts

// The values a ``CullingPass`` is built from and hands back: the verbs it
// accepts, the groups and steps it walks, and the verdicts a write must carry.

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
    var members: [CullAsset]
    /// Whether the stack's members are culled one by one rather than through
    /// the cover.
    var isExpanded = false

    /// A stack with more than one member — the only kind there is anything to
    /// expand.
    var isStack: Bool { stackID != nil && members.count > 1 }

    /// What the collapsed group shows.
    var cover: CullAsset { members[0] }

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

// MARK: - CullVerdict

/// One verdict the user gave, as the surface must write it.
///
/// The pass applies a verdict to what it shows at once; the write follows, and
/// its outcome comes back through ``CullingPass/settle(_:written:)`` or
/// ``CullingPass/revert(_:)`` carrying this value, so an outcome that arrives
/// after a later verdict on the same asset cannot undo that later one.
struct CullVerdict: Equatable, Sendable {
    /// The flag written.
    let flag: CullFlag
    /// The stack whose every member the write must reach — the whole stack,
    /// not only the members loaded into the pass — or `nil` for one asset.
    let stackID: StackID?
    /// The assets in the pass the verdict flagged.
    let targets: [AssetID]
    /// Orders verdicts within one pass.
    let serial: Int

    /// The assets the write reaches, given the stack's members as the stack
    /// port lists them: every one of those, then any target the port did not
    /// list. Ignores `stackMembers` for a one-asset verdict.
    func writeTargets(stackMembers: [AssetID]) -> [AssetID] {
        guard stackID != nil else { return targets }
        let listed = Set(stackMembers)
        return stackMembers + targets.filter { !listed.contains($0) }
    }
}
