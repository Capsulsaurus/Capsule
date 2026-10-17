import Foundation

// MARK: - CullFlag

/// The trinary culling flag — the review pass after a shoot
/// (*Asset Organization — Culling*).
///
/// Deliberately **orthogonal to the numeric star rating**: a reject can carry
/// three stars, and tools that conflate the two force lossy workflows. Flagging
/// touches no bytes and is fully reversible; only the batch-move-to-trash that
/// follows is destructive, and even that is soft-per-retention.
public enum CullFlag: ClosedWireEnum {
    /// A keeper.
    case pick
    /// Never flagged either way. The default, and **wire-absent**.
    case neutral
    /// Flagged for rejection — filtered out, a candidate for batch delete.
    case reject
    case unknown(String)

    public static let knownCases: [CullFlag] = [.pick, .neutral, .reject]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    /// Kebab-case, mirroring the Rust `#[serde(rename_all = "kebab-case")]`.
    public var rawValue: String {
        switch self {
        case .pick: "pick"
        case .neutral: "neutral"
        case .reject: "reject"
        case let .unknown(raw): raw
        }
    }

    /// Whether this value is omitted on the wire.
    public var isWireAbsent: Bool {
        self == .neutral
    }
}

// MARK: - GroupCullState

/// The culling state of a **group** — a stack or a burst.
///
/// A group has **no stored flag of its own**. Its state is derived from its
/// members, every time, because a stored group flag would be a second source of
/// truth that diverges the moment one member is re-flagged. Flagging a
/// collapsed stack instead applies the flag to each member, one
/// `metadata-update` per member, atomically staged.
public enum GroupCullState: Sendable, Equatable, Hashable {
    /// The group is empty — no members to derive from.
    case empty
    /// Every member is ``CullFlag/reject``. The batch-delete affordance applies
    /// to the whole group.
    case allRejected
    /// At least one member is ``CullFlag/pick``. A pick anywhere protects the
    /// group from a reject sweep.
    case anyPick
    /// Neither of the above — some mix of neutral and reject, no pick.
    case mixed

    /// Derive a group's state from its members' flags, in the documented
    /// precedence: all-rejected, then any-pick, else mixed.
    ///
    /// The order matters and is not arbitrary. An all-reject group cannot also
    /// contain a pick, so the two leading cases are disjoint; testing
    /// `allRejected` first means an empty-of-picks group reads as fully
    /// rejected rather than falling through to `mixed`.
    public init(members: [CullFlag]) {
        guard !members.isEmpty else {
            self = .empty
            return
        }
        if members.allSatisfy({ $0 == .reject }) {
            self = .allRejected
        } else if members.contains(.pick) {
            self = .anyPick
        } else {
            self = .mixed
        }
    }
}

// MARK: - StackType

/// The closed set of stack types (*Asset Organization — Stack Types*).
///
/// Closed **per `protocol_version`**: adding a type requires a new, later-dated
/// version, and an album pinned to an older version never sees the new value.
/// A sidecar naming `"future-stack-type"` is a structural rejection at the
/// validator, which is exactly why ``unknown(_:)`` here is read-only.
public enum StackType: ClosedWireEnum {
    // Photography & mobile

    /// The classic prosumer stack: an uncompressed RAW plus its processed JPEG,
    /// treated as one asset.
    case rawJpeg
    /// A high-speed sequence of stills, with a "best photo" in front.
    case burst
    /// A still paired with a 1.5–3 second clip, one interactive unit.
    case livePhoto
    /// An image paired with its depth map, so bokeh stays adjustable.
    case portrait
    /// AI grouping of visually similar images taken seconds apart.
    case smartSelection

    // Technical & creative

    /// Multiple exposures of one scene, to be merged into an HDR image.
    case hdrBracket
    /// Shifting focus points across a series — macro "infinite" depth of field.
    case focusStack
    /// Sensor-shifted captures merged for ultra-high resolution and colour.
    case pixelShift
    /// A sequence intended to be stitched into one wide-field image.
    case panorama

    // Video & audio

    /// A heavy master paired with a lightweight proxy for smooth editing.
    case proxy
    /// Action-camera chunks that are really one continuous recording.
    case chaptered
    /// Video grouped with externally-recorded high-quality audio.
    case dualAudio

    /// A user-formed grouping that fits none of the above.
    case custom

    case unknown(String)

    public static let knownCases: [StackType] = [
        .rawJpeg, .burst, .livePhoto, .portrait, .smartSelection,
        .hdrBracket, .focusStack, .pixelShift, .panorama,
        .proxy, .chaptered, .dualAudio, .custom,
    ]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    /// Snake_case, mirroring the Rust `#[serde(rename_all = "snake_case")]` on
    /// `capsule_core::domain::stack_type::StackType`.
    public var rawValue: String {
        switch self {
        case .rawJpeg: "raw_jpeg"
        case .burst: "burst"
        case .livePhoto: "live_photo"
        case .portrait: "portrait"
        case .smartSelection: "smart_selection"
        case .hdrBracket: "hdr_bracket"
        case .focusStack: "focus_stack"
        case .pixelShift: "pixel_shift"
        case .panorama: "panorama"
        case .proxy: "proxy"
        case .chaptered: "chaptered"
        case .dualAudio: "dual_audio"
        case .custom: "custom"
        case let .unknown(raw): raw
        }
    }
}

// MARK: - StackRole

/// An asset's role inside its stack.
///
/// A collapsed stack shows only its ``primary``, which is why companion files —
/// the JPEG half of a RAW+JPEG pair, a Live Photo's video — need no
/// `hidden` flag: their role already suppresses them from default views.
public enum StackRole: ClosedWireEnum {
    /// The stack's representative — the "best photo". A pointer in metadata,
    /// never a destructive choice.
    case primary
    /// An ordinary member.
    case member
    /// A proxy or optimized variant of the master.
    case proxy
    case unknown(String)

    public static let knownCases: [StackRole] = [.primary, .member, .proxy]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    /// Kebab-case, mirroring the Rust serde attribute.
    public var rawValue: String {
        switch self {
        case .primary: "primary"
        case .member: "member"
        case .proxy: "proxy"
        case let .unknown(raw): raw
        }
    }
}

// MARK: - StackMembership

/// One asset's membership in a stack — the value of the sidecar's
/// `stack_membership` LWW register.
///
/// **Stacking is metadata-only.** A stack edit modifies this field on each
/// member's sidecar and emits one `metadata-update` per affected asset. It never
/// deletes, rewrites, or merges the underlying bytes, so a buggy or malicious
/// stack edit cannot lose an original.
public struct StackMembership: Sendable, Equatable, Hashable {
    /// The stack this asset belongs to (UUIDv7).
    public var stackID: StackID
    /// The kind of stack.
    public var stackType: StackType
    /// This asset's role.
    public var role: StackRole
    /// Ordering within the stack — burst sequence, video chapter index.
    public var memberIndex: UInt32?

    public init(
        stackID: StackID,
        stackType: StackType,
        role: StackRole,
        memberIndex: UInt32? = nil
    ) {
        self.stackID = stackID
        self.stackType = stackType
        self.role = role
        self.memberIndex = memberIndex
    }

    /// Whether this asset is the one a collapsed stack renders.
    public var isStackCover: Bool {
        role == .primary
    }
}

// MARK: - Stack

/// A stack as the UI presents it: its identity, its type, and its members in
/// order.
///
/// Membership is **derived** from the members' sidecars, not stored separately —
/// the same no-second-source-of-truth rule the group cull state follows.
public struct Stack: Sendable, Equatable, Identifiable, Hashable {
    public var id: StackID
    public var stackType: StackType
    /// The primary member, rendered when the stack is collapsed.
    public var primaryAssetID: String
    /// Every member, in `member_index` order, primary included.
    public var memberAssetIDs: [String]
    /// The derived cull state of the group.
    public var cullState: GroupCullState

    public init(
        id: StackID,
        stackType: StackType,
        primaryAssetID: String,
        memberAssetIDs: [String],
        cullState: GroupCullState
    ) {
        self.id = id
        self.stackType = stackType
        self.primaryAssetID = primaryAssetID
        self.memberAssetIDs = memberAssetIDs
        self.cullState = cullState
    }

    /// How many members are hidden behind the cover in a collapsed stack.
    public var collapsedOverflowCount: Int {
        max(0, memberAssetIDs.count - 1)
    }
}

// MARK: - TrashEntry

/// An asset in the trash, with the retention window that protects it
/// (*Asset Organization — Recycling*).
///
/// The window is **signed into the `delete` manifest** as `retention_until`, not
/// configured server-side at purge time. That is the whole point: the server
/// can neither accelerate a purge (a hard purge before the signed deadline is
/// rejected by its own keyless worker) nor delay one past a user-issued
/// restore. The countdown a user sees is therefore a cryptographic floor, not a
/// promise.
public struct TrashEntry: Sendable, Equatable, Identifiable, Hashable {
    /// The default retention window, in days.
    public static let defaultRetentionDays = 30

    /// The soft-deleted asset.
    public var assetID: String
    /// When it was soft-deleted.
    public var deletedAt: CapsuleTimestamp
    /// The signed retention deadline. After this, the purge worker may proceed;
    /// before it, a purge is refused.
    public var retentionUntil: CapsuleTimestamp

    public var id: String { assetID }

    public init(assetID: String, deletedAt: CapsuleTimestamp, retentionUntil: CapsuleTimestamp) {
        self.assetID = assetID
        self.deletedAt = deletedAt
        self.retentionUntil = retentionUntil
    }

    /// Whether a `trash-restore` is still available at the given instant.
    ///
    /// A restore appends a new provenance record and rewinds local lifecycle
    /// state; the original `delete` record is **not** removed — the chain keeps
    /// "deleted on X, restored on Y".
    public func isRestorable(at now: CapsuleTimestamp) -> Bool {
        now < retentionUntil
    }

    /// Whole days remaining before the purge window opens, floored at zero.
    public func daysRemaining(at now: CapsuleTimestamp) -> Int {
        let seconds = retentionUntil.epochSeconds - now.epochSeconds
        return seconds <= 0 ? 0 : Int(seconds / 86400)
    }
}
