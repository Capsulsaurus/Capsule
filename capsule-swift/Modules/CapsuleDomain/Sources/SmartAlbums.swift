import Foundation

// MARK: - Term

/// One leaf of a predicate: a field, an operator, and a typed literal.
public struct Term: Sendable, Equatable, Hashable {
    public var field: QueryField
    /// The comparison. Spelled `operatorKind` because `operator` is a Swift
    /// keyword.
    public var operatorKind: PredicateOperator
    public var operand: Operand

    public init(field: QueryField, operatorKind: PredicateOperator, operand: Operand) {
        self.field = field
        self.operatorKind = operatorKind
        self.operand = operand
    }
}

// MARK: - SmartAlbumPredicate

/// The closed, bounded predicate tree
/// (*Asset Organization — Smart-Album Definition Schema*).
///
/// The empty-collection semantics are **not** interchangeable and are taken
/// verbatim from the grammar: an empty ``all(_:)`` matches every asset, an empty
/// ``any(_:)`` matches none. Getting these backwards turns a half-configured
/// smart album into either the whole library or nothing.
public indirect enum SmartAlbumPredicate: Sendable, Equatable, Hashable {
    /// AND. **Empty matches every asset.**
    case all([SmartAlbumPredicate])
    /// OR. **Empty matches none.**
    case any([SmartAlbumPredicate])
    case not(SmartAlbumPredicate)
    case term(Term)

    /// The nesting depth, a bare term counting as 1. Bounded at
    /// ``PredicateValidator/maximumDepth``.
    public var depth: Int {
        switch self {
        case let .all(children), let .any(children):
            1 + (children.map(\.depth).max() ?? 0)
        case let .not(child):
            1 + child.depth
        case .term:
            1
        }
    }

    /// Every leaf term, in traversal order. Bounded at
    /// ``PredicateValidator/maximumTermCount``.
    public var terms: [Term] {
        switch self {
        case let .all(children), let .any(children):
            children.flatMap(\.terms)
        case let .not(child):
            child.terms
        case let .term(term):
            [term]
        }
    }
}

// MARK: - SortSpec

/// The closed sort key set. The default is `(capture_timestamp, desc)`.
public struct SortSpec: Sendable, Equatable, Hashable {
    /// The closed set of sortable keys.
    public enum Key: ClosedWireEnum {
        case captureTimestamp
        case importTimestamp
        case rating
        case unknown(String)

        public static let knownCases: [Key] = [.captureTimestamp, .importTimestamp, .rating]

        public init(rawValue: String) {
            self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
        }

        public var rawValue: String {
            switch self {
            case .captureTimestamp: "capture_timestamp"
            case .importTimestamp: "import_timestamp"
            case .rating: "rating"
            case let .unknown(raw): raw
            }
        }
    }

    /// Sort direction.
    public enum Direction: ClosedWireEnum {
        case ascending
        case descending
        case unknown(String)

        public static let knownCases: [Direction] = [.ascending, .descending]

        public init(rawValue: String) {
            self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
        }

        public var rawValue: String {
            switch self {
            case .ascending: "asc"
            case .descending: "desc"
            case let .unknown(raw): raw
            }
        }
    }

    public var key: Key
    public var direction: Direction

    public init(key: Key, direction: Direction) {
        self.key = key
        self.direction = direction
    }

    /// The documented default when a definition carries no sort.
    public static let `default` = SortSpec(key: .captureTimestamp, direction: .descending)
}

// MARK: - SmartAlbumDefinition

/// A user-defined smart album (*Asset Organization — Smart-Album Definition
/// Schema*).
///
/// The whole definition is the value of **one** LWW register keyed by
/// ``smartAlbumID`` in the library-settings document, so authoring or editing a
/// smart album is a single stamped write and there is never a partial-predicate
/// merge. Membership is **computed, never stored**: evaluation is a pure
/// function of `(definition, the assets the viewer can decrypt)` processed in
/// sorted asset-id order, so recomputation is idempotent and identical across
/// devices.
public struct SmartAlbumDefinition: Sendable, Equatable, Identifiable, Hashable {
    /// The predicate-grammar version this build can evaluate.
    public static let currentPredicateSchema: UInt16 = 1

    public var smartAlbumID: SmartAlbumID
    /// The closed-grammar version. A **later** value gates evaluation: a
    /// definition ahead of this build is preserved verbatim through sync
    /// round-trips and never evaluated or stripped, surfaced as "created by a
    /// newer app version".
    public var predicateSchema: UInt16
    /// The display name — an LWW register that converges like a caption. ≤ 256
    /// bytes.
    public var displayName: Lww<String>
    /// The predicate tree.
    public var predicate: SmartAlbumPredicate
    /// The sort, or `nil` for ``SortSpec/default``.
    public var sort: SortSpec?

    public var id: SmartAlbumID { smartAlbumID }

    public init(
        smartAlbumID: SmartAlbumID,
        predicateSchema: UInt16 = SmartAlbumDefinition.currentPredicateSchema,
        displayName: Lww<String>,
        predicate: SmartAlbumPredicate,
        sort: SortSpec? = nil
    ) {
        self.smartAlbumID = smartAlbumID
        self.predicateSchema = predicateSchema
        self.displayName = displayName
        self.predicate = predicate
        self.sort = sort
    }

    /// The effective sort.
    public var effectiveSort: SortSpec {
        sort ?? .default
    }

    /// Whether this build may evaluate the definition at all.
    ///
    /// A definition from a newer grammar must be **preserved and not
    /// evaluated** — evaluating it partially would produce a different album on
    /// this device than on the one that authored it.
    public var isEvaluable: Bool {
        predicateSchema <= Self.currentPredicateSchema
    }

    /// Validate the whole definition, predicate bounds included.
    ///
    /// - Throws: ``PredicateValidationError``.
    public func validate() throws {
        try PredicateValidator.validate(displayName: displayName.value)
        try PredicateValidator.validate(predicate)
    }
}

// MARK: - PredicateValidationError

/// Why a predicate or definition was structurally rejected.
///
/// Every case is a **rejection**, not a warning. The predicate editor renders
/// these directly as its inline constraint feedback, which is why each one
/// carries the offending values rather than a message: the copy lives in the
/// i18n catalog, keyed off the case.
public enum PredicateValidationError: Error, Sendable, Equatable, Hashable {
    /// The tree nests deeper than ``PredicateValidator/maximumDepth``.
    case depthExceeded(depth: Int, maximum: Int)
    /// The tree carries more than ``PredicateValidator/maximumTermCount`` terms.
    case termCountExceeded(count: Int, maximum: Int)
    /// The term named a field this build does not know.
    case unknownField(String)
    /// The term named an operator this build does not know.
    case unknownOperator(String)
    /// The operator is not valid for the field's type class.
    case operatorNotValidForField(field: QueryField, operatorKind: PredicateOperator)
    /// The operand's shape does not match the `(field, operator)` pair.
    case operandTypeMismatch(field: QueryField, operatorKind: PredicateOperator)
    /// A set operand carries more than ``PredicateValidator/maximumSetMembers``
    /// members.
    case setOperandTooLarge(count: Int, maximum: Int)
    /// A set member exceeds ``PredicateValidator/maximumSetMemberBytes``.
    case setMemberTooLong(byteCount: Int, maximum: Int)
    /// A range's bounds are inverted or empty.
    case invalidRange
    /// The display name exceeds ``PredicateValidator/maximumDisplayNameBytes``.
    case displayNameTooLong(byteCount: Int, maximum: Int)
}

// MARK: - PredicateValidator

/// The definition validator — the single place the documented bounds live.
///
/// A validating *function*, not a type the UI holds: the predicate editor calls
/// it on every edit to decide whether a term may be committed, and the same call
/// runs before a definition is written. One implementation, so the editor cannot
/// drift from what the writer accepts.
public enum PredicateValidator {
    /// Maximum nesting depth. A bare term is depth 1.
    public static let maximumDepth = 8
    /// Maximum leaf terms in one predicate.
    public static let maximumTermCount = 64
    /// Maximum members in a set operand.
    public static let maximumSetMembers = 64
    /// Maximum UTF-8 bytes per set member.
    public static let maximumSetMemberBytes = 256
    /// Maximum UTF-8 bytes of a display name.
    public static let maximumDisplayNameBytes = 256

    /// Validate a whole predicate: bounds first, then every term.
    ///
    /// Bounds are checked before terms so a pathologically large tree is
    /// rejected cheaply rather than walked.
    ///
    /// - Throws: ``PredicateValidationError``.
    public static func validate(_ predicate: SmartAlbumPredicate) throws {
        let depth = predicate.depth
        guard depth <= maximumDepth else {
            throw PredicateValidationError.depthExceeded(depth: depth, maximum: maximumDepth)
        }
        let terms = predicate.terms
        guard terms.count <= maximumTermCount else {
            throw PredicateValidationError.termCountExceeded(
                count: terms.count,
                maximum: maximumTermCount
            )
        }
        for term in terms {
            try validate(term)
        }
    }

    /// Validate one term against the grammar table: known field, known
    /// operator, operator legal for the field's class, operand shaped for the
    /// `(field, operator)` pair, and set bounds.
    ///
    /// - Throws: ``PredicateValidationError``.
    public static func validate(_ term: Term) throws {
        guard let fieldClass = term.field.typeClass else {
            throw PredicateValidationError.unknownField(term.field.rawValue)
        }
        guard term.operatorKind.isKnown else {
            throw PredicateValidationError.unknownOperator(term.operatorKind.rawValue)
        }
        let required = QueryGrammar.requiredOperandShape(
            field: term.field,
            fieldClass: fieldClass,
            operatorKind: term.operatorKind
        )
        guard let required else {
            throw PredicateValidationError.operatorNotValidForField(
                field: term.field,
                operatorKind: term.operatorKind
            )
        }
        guard term.operand.shape == required else {
            throw PredicateValidationError.operandTypeMismatch(
                field: term.field,
                operatorKind: term.operatorKind
            )
        }
        try validateBounds(of: term.operand)
    }

    /// Validate a display name's byte length. `nil` — a never-written register —
    /// is legal; the UI falls back to a catalog string.
    ///
    /// - Throws: ``PredicateValidationError/displayNameTooLong(byteCount:maximum:)``.
    public static func validate(displayName: String?) throws {
        guard let displayName else { return }
        let byteCount = displayName.utf8.count
        guard byteCount <= maximumDisplayNameBytes else {
            throw PredicateValidationError.displayNameTooLong(
                byteCount: byteCount,
                maximum: maximumDisplayNameBytes
            )
        }
    }

    /// The size and ordering bounds that apply to an operand's payload.
    private static func validateBounds(of operand: Operand) throws {
        switch operand {
        case let .stringSet(members), let .enumerationSet(members):
            try validateSetBounds(members)
        case let .timestampRange(start, end):
            // Half-open `[start, end)`: an empty range matches nothing and is
            // always a mistake, so it is rejected rather than silently stored.
            guard start < end else { throw PredicateValidationError.invalidRange }
        case let .numberRange(lower, upper):
            // Inclusive `[lo, hi]`, so equal bounds are a legal single value.
            guard lower <= upper else { throw PredicateValidationError.invalidRange }
        default:
            break
        }
    }

    /// Member count and per-member byte bounds for a set operand.
    private static func validateSetBounds(_ members: [String]) throws {
        guard members.count <= maximumSetMembers else {
            throw PredicateValidationError.setOperandTooLarge(
                count: members.count,
                maximum: maximumSetMembers
            )
        }
        for member in members {
            let byteCount = member.utf8.count
            guard byteCount <= maximumSetMemberBytes else {
                throw PredicateValidationError.setMemberTooLong(
                    byteCount: byteCount,
                    maximum: maximumSetMemberBytes
                )
            }
        }
    }
}
