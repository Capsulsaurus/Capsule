import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockPredicateContext

/// Everything a predicate term can read about one asset.
///
/// Bundled rather than reaching back into the library per term, because a
/// predicate can carry sixty-four terms and re-deriving the geolocation for each
/// of them would make a smart album's cost quadratic in its own complexity.
public struct MockPredicateContext: Sendable {
    public var asset: LibraryAsset
    public var gps: Gps?
    public var hasCameraIdentity: Bool
    public var personIDs: Set<String>

    public init(asset: LibraryAsset, gps: Gps?, hasCameraIdentity: Bool, personIDs: Set<String>) {
        self.asset = asset
        self.gps = gps
        self.hasCameraIdentity = hasCameraIdentity
        self.personIDs = personIDs
    }
}

// MARK: - MockPredicateEvaluator

/// Evaluates the closed smart-album grammar.
///
/// Two rules from the grammar that are easy to get backwards and expensive to
/// get wrong: an empty `all` matches **every** asset and an empty `any` matches
/// **none**. Reversed, a half-configured smart album silently becomes either the
/// whole library or nothing.
///
/// A term over a model-scoped facet whose slot has changed evaluates as
/// **stale-excluded** rather than being compared across model versions, which is
/// why results can legitimately shrink after a model upgrade.
public enum MockPredicateEvaluator {
    public static func matches(_ predicate: SmartAlbumPredicate, context: MockPredicateContext) -> Bool {
        switch predicate {
        case let .all(children):
            children.allSatisfy { matches($0, context: context) }
        case let .any(children):
            children.contains { matches($0, context: context) }
        case let .not(child):
            !matches(child, context: context)
        case let .term(term):
            matches(term: term, context: context)
        }
    }

    /// Dispatch a term to the evaluator for its type class.
    ///
    /// Split three ways rather than written as one switch because the grammar
    /// has seventeen fields and one flat match is both over the complexity
    /// budget and impossible to read against the grammar table it mirrors.
    static func matches(term: Term, context: MockPredicateContext) -> Bool {
        if let result = matchesScalar(term, context: context) { return result }
        if let result = matchesFlag(term, context: context) { return result }
        return matchesSet(term, context: context)
    }

    /// Temporal, enumeration, and numeric fields. `nil` when the field is not
    /// one of them.
    private static func matchesScalar(_ term: Term, context: MockPredicateContext) -> Bool? {
        switch term.field {
        case .captureTimestamp:
            temporal(term, seconds: context.asset.effectiveCaptureTimestamp.epochSeconds)
        case .importTimestamp:
            temporal(term, seconds: context.asset.importTimestamp.epochSeconds)
        case .contentType:
            enumeration(term, value: context.asset.contentType.rawValue)
        case .mediaKind:
            enumeration(term, value: context.asset.contentType.mediaKind.rawValue)
        case .gpsDatum:
            context.gps.map { enumeration(term, value: $0.datum.rawValue) } ?? false
        case .rating:
            numeric(term, value: UInt32(context.asset.rating))
        case .dimensionsWidth:
            context.asset.dimensions.map { numeric(term, value: $0.width) } ?? false
        case .dimensionsHeight:
            context.asset.dimensions.map { numeric(term, value: $0.height) } ?? false
        default:
            nil
        }
    }

    /// The trinary and presence fields — the two classes where the **field**,
    /// not the class, fixes the operand type.
    private static func matchesFlag(_ term: Term, context: MockPredicateContext) -> Bool? {
        switch term.field {
        case .cull: trinaryCull(term, value: context.asset.cull)
        case .hidden: trinaryBoolean(term, value: context.asset.isUserHidden)
        case .gps: presence(term, isPresent: context.gps != nil)
        case .cameraID: presence(term, isPresent: context.hasCameraIdentity)
        default: nil
        }
    }

    /// The set fields.
    ///
    /// An unknown field lands in the default and matches nothing. It is a
    /// structural rejection at the validator and should never reach evaluation;
    /// if it does, matching nothing is the conservative answer, because a
    /// predicate that silently dropped a term would show a different album on
    /// different devices.
    private static func matchesSet(_ term: Term, context: MockPredicateContext) -> Bool {
        switch term.field {
        case .tagsUser:
            set(term, values: context.asset.tagsUser)
        case .tagsAI:
            set(term, values: currentSlotTags(context.asset.tagsAI))
        case .stackType:
            set(term, values: context.asset.stackMembership.map { [$0.stackType.rawValue] } ?? [])
        case .peopleCluster:
            set(term, values: context.personIDs)
        case .albumID:
            set(term, values: context.asset.albumID.map { [albumText($0)] } ?? [])
        default:
            false
        }
    }

    /// AI tags from a superseded slot are excluded rather than compared.
    private static func currentSlotTags(_ tags: Set<AiTag>) -> Set<String> {
        Set(tags.filter { $0.modelSlot == MockTables.sceneTaggingSlot }.map(\.tag))
    }

    // MARK: Operators

    private static func temporal(_ term: Term, seconds: Int64) -> Bool {
        switch (term.operatorKind, term.operand) {
        case let (.before, .timestamp(bound)): seconds < bound.epochSeconds
        case let (.after, .timestamp(bound)): seconds > bound.epochSeconds
        // Half-open `[start, end)`, so adjacent ranges tile without
        // double-counting the boundary instant.
        case let (.inRange, .timestampRange(start, end)):
            seconds >= start.epochSeconds && seconds < end.epochSeconds
        default: false
        }
    }

    private static func enumeration(_ term: Term, value: String) -> Bool {
        switch (term.operatorKind, term.operand) {
        case let (.equalTo, .enumerationValue(wanted)): value == wanted
        case let (.anyOf, .enumerationSet(wanted)): wanted.contains(value)
        default: false
        }
    }

    private static func numeric(_ term: Term, value: UInt32) -> Bool {
        switch (term.operatorKind, term.operand) {
        case let (.equalTo, .number(wanted)): value == wanted
        case let (.greaterThanOrEqual, .number(wanted)): value >= wanted
        case let (.lessThanOrEqual, .number(wanted)): value <= wanted
        // Inclusive `[lo, hi]`, so equal bounds are a legal single value.
        case let (.inRange, .numberRange(lower, upper)): value >= lower && value <= upper
        default: false
        }
    }

    private static func trinaryCull(_ term: Term, value: CullFlag) -> Bool {
        guard term.operatorKind == .isValue, case let .cullFlag(wanted) = term.operand else { return false }
        return value == wanted
    }

    private static func trinaryBoolean(_ term: Term, value: Bool) -> Bool {
        guard term.operatorKind == .isValue, case let .boolean(wanted) = term.operand else { return false }
        return value == wanted
    }

    private static func set(_ term: Term, values: Set<String>) -> Bool {
        guard case let .stringSet(wanted) = term.operand else { return false }
        switch term.operatorKind {
        case .contains, .containsAny: return !values.isDisjoint(with: wanted)
        case .containsAll: return Set(wanted).isSubset(of: values)
        default: return false
        }
    }

    private static func presence(_ term: Term, isPresent: Bool) -> Bool {
        guard term.operatorKind == .exists, case let .boolean(wanted) = term.operand else { return false }
        return isPresent == wanted
    }

    private static func albumText(_ identifier: AlbumID) -> String {
        switch identifier {
        case let .managed(uuid): uuid
        case let .smart(localIdentifier): localIdentifier
        }
    }
}
