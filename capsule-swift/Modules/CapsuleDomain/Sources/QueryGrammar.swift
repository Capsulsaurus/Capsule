import Foundation

// MARK: - QueryFieldClass

/// The type class of a queryable field — the thing that decides which operators
/// and which operand shapes are legal for it
/// (*Asset Organization — Smart-Album Definition Schema*).
///
/// Modelled explicitly rather than inferred per field so the predicate editor
/// can offer the right operator list *before* the user builds an invalid term,
/// instead of rejecting it afterwards.
public enum QueryFieldClass: Sendable, Equatable, Hashable, CaseIterable {
    /// RFC 3339 instants.
    case temporal
    /// A value drawn from the field's own closed enum.
    case enumeration
    /// `u32` numbers.
    case numeric
    /// A trinary flag or a boolean.
    case trinary
    /// A set of strings or ids.
    case set
    /// Presence or absence of an optional field.
    case presence
}

// MARK: - PredicateOperator

/// The closed operator set.
///
/// Named `PredicateOperator` rather than `Operator` because `operator` is a
/// Swift keyword and `Operator` is a term of art in the language — an
/// unqualified `Operator` in a module every feature imports would be a
/// permanent source of confusion.
public enum PredicateOperator: ClosedWireEnum {
    case before
    case after
    case inRange
    case equalTo
    case anyOf
    case greaterThanOrEqual
    case lessThanOrEqual
    case isValue
    case contains
    case containsAny
    case containsAll
    case exists
    case unknown(String)

    public static let knownCases: [PredicateOperator] = [
        .before, .after, .inRange, .equalTo, .anyOf,
        .greaterThanOrEqual, .lessThanOrEqual, .isValue,
        .contains, .containsAny, .containsAll, .exists,
    ]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .before: "before"
        case .after: "after"
        case .inRange: "in_range"
        case .equalTo: "eq"
        case .anyOf: "in"
        case .greaterThanOrEqual: "gte"
        case .lessThanOrEqual: "lte"
        case .isValue: "is"
        case .contains: "contains"
        case .containsAny: "contains_any"
        case .containsAll: "contains_all"
        case .exists: "exists"
        case let .unknown(raw): raw
        }
    }
}

// MARK: - QueryField

/// The closed set of queryable fields (*Asset Organization — Smart-Album
/// Definition Schema*).
///
/// Closed on purpose. A definition is stored data that syncs to every one of the
/// owner's devices, so it must evaluate identically everywhere and can carry no
/// code, no regex, and no unbounded input. A term naming an unknown field is a
/// **structural rejection at the definition validator** — never a "future to
/// ignore", because a predicate that silently dropped a term would show a
/// different album on different devices.
public enum QueryField: ClosedWireEnum {
    // temporal
    case captureTimestamp
    case importTimestamp
    // enumeration
    case contentType
    /// `image | video`, derived from `content_type` — never stored separately.
    case mediaKind
    case gpsDatum
    // numeric
    case rating
    case dimensionsWidth
    case dimensionsHeight
    // trinary / bool
    case cull
    case hidden
    // set
    case tagsUser
    case tagsAI
    case stackType
    case peopleCluster
    case albumID
    // presence
    case gps
    case cameraID

    case unknown(String)

    public static let knownCases: [QueryField] = [
        .captureTimestamp, .importTimestamp,
        .contentType, .mediaKind, .gpsDatum,
        .rating, .dimensionsWidth, .dimensionsHeight,
        .cull, .hidden,
        .tagsUser, .tagsAI, .stackType, .peopleCluster, .albumID,
        .gps, .cameraID,
    ]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .captureTimestamp: "capture_timestamp"
        case .importTimestamp: "import_timestamp"
        case .contentType: "content_type"
        case .mediaKind: "media_kind"
        case .gpsDatum: "gps.datum"
        case .rating: "rating"
        case .dimensionsWidth: "dimensions.width"
        case .dimensionsHeight: "dimensions.height"
        case .cull: "cull"
        case .hidden: "hidden"
        case .tagsUser: "tags_user"
        case .tagsAI: "tags_ai"
        case .stackType: "stack_type"
        case .peopleCluster: "people_cluster"
        case .albumID: "album_id"
        case .gps: "gps"
        case .cameraID: "camera_id"
        case let .unknown(raw): raw
        }
    }

    /// This field's type class, or `nil` for a field this build does not know —
    /// which is what makes an unknown field unvalidatable, and therefore a
    /// structural rejection rather than a tolerated future.
    public var typeClass: QueryFieldClass? {
        QueryGrammar.fieldClasses[self]
    }

    /// The operators legal for this field, in a stable order suitable for a
    /// picker. Empty for an unknown field.
    public var validOperators: [PredicateOperator] {
        typeClass.map { QueryGrammar.operators(for: $0) } ?? []
    }

    /// Whether querying this field is subject to the AI staleness rule: a term
    /// over a model slot whose canonical model changed evaluates as
    /// **stale-excluded** until regenerated, never compared across model
    /// versions (*AI — AI Output Containment*).
    public var isModelScoped: Bool {
        self == .tagsAI || self == .peopleCluster
    }
}

// MARK: - Operand

/// A typed literal, its shape fixed by the `(field, operator)` pair it
/// accompanies.
///
/// Typed rather than a stringly-typed blob so that most mistakes — a date
/// operand on a numeric field — are unrepresentable, and the rest are caught by
/// ``PredicateValidator``.
public enum Operand: Sendable, Equatable, Hashable {
    /// A single RFC 3339 instant — for `before` / `after`.
    case timestamp(CapsuleTimestamp)
    /// A **half-open** `[start, end)` pair — for temporal `in_range`. Half-open
    /// so adjacent ranges tile without double-counting the boundary instant.
    case timestampRange(start: CapsuleTimestamp, end: CapsuleTimestamp)
    /// One value from the field's own closed enum — for `eq`.
    case enumerationValue(String)
    /// A set of values from the field's own closed enum — for `in`.
    case enumerationSet([String])
    /// A single number — for `eq` / `gte` / `lte`.
    case number(UInt32)
    /// An inclusive `[lo, hi]` pair — for numeric `in_range`.
    case numberRange(lower: UInt32, upper: UInt32)
    /// A culling flag literal — for `cull is …`.
    case cullFlag(CullFlag)
    /// A boolean literal — for `hidden is …` and for `exists`.
    ///
    /// One case rather than two because the shapes are identical; which field it
    /// is legal on is decided by the grammar table, not by a second spelling of
    /// `Bool`.
    case boolean(Bool)
    /// A string or id set — for the set operators.
    case stringSet([String])

    /// The shape this operand presents to the grammar table.
    public var shape: OperandShape {
        switch self {
        case .timestamp: .timestamp
        case .timestampRange: .timestampRange
        case .enumerationValue: .enumerationValue
        case .enumerationSet: .enumerationSet
        case .number: .number
        case .numberRange: .numberRange
        case .cullFlag: .cullFlag
        case .boolean: .boolean
        case .stringSet: .stringSet
        }
    }
}

/// The shape half of an operand, without its payload — the value the grammar
/// table is keyed on.
public enum OperandShape: Sendable, Equatable, Hashable, CaseIterable {
    case timestamp
    case timestampRange
    case enumerationValue
    case enumerationSet
    case number
    case numberRange
    case cullFlag
    case boolean
    case stringSet
}

// MARK: - QueryGrammar

/// The grammar table: which operators a field class admits, and which operand
/// shape each `(class, operator)` pair requires.
///
/// A **table, not a switch**, for two reasons. It is directly testable — the
/// predicate editor renders its operator picker straight from
/// ``operators(for:)``, so the picker and the validator cannot disagree — and it
/// keeps the validation path flat instead of a deeply nested match that grows a
/// branch every time the grammar gains a row.
public enum QueryGrammar {
    /// Every known field's type class. The `unknown` case is deliberately
    /// absent: a field with no class cannot be validated, which is exactly the
    /// structural rejection the grammar requires.
    public static let fieldClasses: [QueryField: QueryFieldClass] = [
        .captureTimestamp: .temporal,
        .importTimestamp: .temporal,
        .contentType: .enumeration,
        .mediaKind: .enumeration,
        .gpsDatum: .enumeration,
        .rating: .numeric,
        .dimensionsWidth: .numeric,
        .dimensionsHeight: .numeric,
        .cull: .trinary,
        .hidden: .trinary,
        .tagsUser: .set,
        .tagsAI: .set,
        .stackType: .set,
        .peopleCluster: .set,
        .albumID: .set,
        .gps: .presence,
        .cameraID: .presence,
    ]

    /// The required operand shape for each legal `(class, operator)` pair.
    /// A pair absent from this table is not a legal term.
    public static let operandShapes: [Key: OperandShape] = [
        Key(.temporal, .before): .timestamp,
        Key(.temporal, .after): .timestamp,
        Key(.temporal, .inRange): .timestampRange,
        Key(.enumeration, .equalTo): .enumerationValue,
        Key(.enumeration, .anyOf): .enumerationSet,
        Key(.numeric, .equalTo): .number,
        Key(.numeric, .greaterThanOrEqual): .number,
        Key(.numeric, .lessThanOrEqual): .number,
        Key(.numeric, .inRange): .numberRange,
        Key(.set, .contains): .stringSet,
        Key(.set, .containsAny): .stringSet,
        Key(.set, .containsAll): .stringSet,
        Key(.presence, .exists): .boolean,
    ]

    /// The trinary class is the one place the **field**, not the class, fixes
    /// the operand: `cull is <flag>` and `hidden is <bool>` share an operator
    /// and a class but not a literal type.
    public static let trinaryOperandShapes: [QueryField: OperandShape] = [
        .cull: .cullFlag,
        .hidden: .boolean,
    ]

    /// The operators a class admits, in picker order.
    public static func operators(for fieldClass: QueryFieldClass) -> [PredicateOperator] {
        let fromTable = PredicateOperator.knownCases.filter {
            operandShapes[Key(fieldClass, $0)] != nil
        }
        return fieldClass == .trinary ? [.isValue] : fromTable
    }

    /// The operand shape a `(field, operator)` pair requires, or `nil` when the
    /// pair is not a legal term at all.
    public static func requiredOperandShape(
        field: QueryField,
        fieldClass: QueryFieldClass,
        operatorKind: PredicateOperator
    ) -> OperandShape? {
        if fieldClass == .trinary {
            return operatorKind == .isValue ? trinaryOperandShapes[field] : nil
        }
        return operandShapes[Key(fieldClass, operatorKind)]
    }

    /// A hashable `(class, operator)` pair — Swift has no tuple `Hashable`
    /// conformance to key a dictionary on.
    public struct Key: Sendable, Hashable {
        public var fieldClass: QueryFieldClass
        public var operatorKind: PredicateOperator

        public init(_ fieldClass: QueryFieldClass, _ operatorKind: PredicateOperator) {
            self.fieldClass = fieldClass
            self.operatorKind = operatorKind
        }
    }
}

public extension QueryFieldClass {
    /// The operators valid for this class, in a stable order suitable for a
    /// picker.
    var validOperators: [PredicateOperator] {
        QueryGrammar.operators(for: self)
    }
}
