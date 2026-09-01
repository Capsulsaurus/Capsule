import Foundation
import Testing

import CapsuleDomain

/// The closed predicate grammar's structural rejections and its documented
/// bounds (*Asset Organization — Validation*).
///
/// The predicate editor depends on every one of these: it calls the validator on
/// each edit to decide whether a term may be committed, so a bound that is wrong
/// here is a term the user can build and then fail to save.
@Suite("SmartAlbumPredicate validation enforces the closed grammar and its bounds")
struct PredicateValidationTests {
    // MARK: Bounds

    @Test("a predicate at the depth limit is accepted")
    func depthAtLimitAccepted() throws {
        try PredicateValidator.validate(Fixtures.nested(depth: PredicateValidator.maximumDepth))
    }

    @Test("a predicate one level past the depth limit is rejected")
    func depthOverLimitRejected() {
        let tooDeep = Fixtures.nested(depth: PredicateValidator.maximumDepth + 1)
        #expect(tooDeep.depth == PredicateValidator.maximumDepth + 1)
        #expect(throws: PredicateValidationError.depthExceeded(depth: 9, maximum: 8)) {
            try PredicateValidator.validate(tooDeep)
        }
    }

    @Test("64 terms are accepted and 65 are rejected")
    func termCountBound() throws {
        let atLimit = SmartAlbumPredicate.all(
            Array(repeating: SmartAlbumPredicate.term(Fixtures.ratingTerm), count: PredicateValidator.maximumTermCount)
        )
        try PredicateValidator.validate(atLimit)

        let overLimit = SmartAlbumPredicate.all(
            Array(repeating: SmartAlbumPredicate.term(Fixtures.ratingTerm), count: PredicateValidator.maximumTermCount + 1)
        )
        #expect(throws: PredicateValidationError.termCountExceeded(count: 65, maximum: 64)) {
            try PredicateValidator.validate(overLimit)
        }
    }

    @Test("a set operand over 64 members is rejected")
    func setMemberCountBound() {
        let members = (0 ... PredicateValidator.maximumSetMembers).map(String.init)
        let term = Fixtures.term(.tagsUser, .containsAny, .stringSet(members))
        #expect(throws: PredicateValidationError.setOperandTooLarge(count: 65, maximum: 64)) {
            try PredicateValidator.validate(term)
        }
    }

    @Test("a set member over 256 bytes is rejected, measured in UTF-8 bytes")
    func setMemberByteBound() {
        // Multi-byte characters must count as bytes, not as characters — a
        // character-based check would let a 256-emoji tag through at four times
        // the documented size.
        let member = String(repeating: "é", count: 129) // 258 UTF-8 bytes
        #expect(member.count == 129)
        #expect(member.utf8.count == 258)
        let term = Fixtures.term(.tagsUser, .contains, .stringSet([member]))
        #expect(throws: PredicateValidationError.setMemberTooLong(byteCount: 258, maximum: 256)) {
            try PredicateValidator.validate(term)
        }
    }

    @Test("a display name over 256 bytes is rejected")
    func displayNameBound() {
        let name = String(repeating: "a", count: 257)
        #expect(throws: PredicateValidationError.displayNameTooLong(byteCount: 257, maximum: 256)) {
            try PredicateValidator.validate(displayName: name)
        }
        #expect(throws: Never.self) {
            try PredicateValidator.validate(displayName: String(repeating: "a", count: 256))
        }
    }

    // MARK: Structural rejections

    @Test("a term naming an unknown field is a structural rejection, not a tolerated future")
    func unknownFieldRejected() {
        let term = Fixtures.term(QueryField(rawValue: "future_field"), .equalTo, .number(1))
        #expect(throws: PredicateValidationError.unknownField("future_field")) {
            try PredicateValidator.validate(term)
        }
    }

    @Test("a term naming an unknown operator is a structural rejection")
    func unknownOperatorRejected() {
        let term = Fixtures.term(.rating, PredicateOperator(rawValue: "approximately"), .number(1))
        #expect(throws: PredicateValidationError.unknownOperator("approximately")) {
            try PredicateValidator.validate(term)
        }
    }

    @Test("an operator invalid for the field's type class is rejected")
    func operatorNotValidForClass() {
        // `contains` is a set operator; `rating` is numeric.
        let term = Fixtures.term(.rating, .contains, .stringSet(["3"]))
        #expect(
            throws: PredicateValidationError.operatorNotValidForField(
                field: .rating,
                operatorKind: .contains
            )
        ) {
            try PredicateValidator.validate(term)
        }
    }

    @Test("an operand mistyped for its field and operator is rejected")
    func operandMistyped() {
        // `capture_timestamp before` needs an instant, not a number.
        let term = Fixtures.term(.captureTimestamp, .before, .number(5))
        #expect(
            throws: PredicateValidationError.operandTypeMismatch(
                field: .captureTimestamp,
                operatorKind: .before
            )
        ) {
            try PredicateValidator.validate(term)
        }
    }

    @Test("the trinary class distinguishes cull's flag literal from hidden's boolean")
    func trinaryOperandIsFieldSpecific() throws {
        // Both are trinary-class with the `is` operator, but they take
        // different literals — the one place the field, not the class, decides.
        try PredicateValidator.validate(Fixtures.term(.cull, .isValue, .cullFlag(.reject)))
        try PredicateValidator.validate(Fixtures.term(.hidden, .isValue, .boolean(true)))

        #expect(
            throws: PredicateValidationError.operandTypeMismatch(field: .cull, operatorKind: .isValue)
        ) {
            try PredicateValidator.validate(Fixtures.term(.cull, .isValue, .boolean(true)))
        }
        #expect(
            throws: PredicateValidationError.operandTypeMismatch(field: .hidden, operatorKind: .isValue)
        ) {
            try PredicateValidator.validate(Fixtures.term(.hidden, .isValue, .cullFlag(.pick)))
        }
    }

    @Test("an inverted or empty range is rejected before evaluation")
    func rangesMustBeOrdered() {
        let inverted = Fixtures.term(.rating, .inRange, .numberRange(lower: 5, upper: 1))
        #expect(throws: PredicateValidationError.invalidRange) {
            try PredicateValidator.validate(inverted)
        }

        // The temporal range is half-open, so an empty `[t, t)` matches nothing
        // and is always a mistake.
        let empty = Fixtures.term(
            .captureTimestamp,
            .inRange,
            .timestampRange(start: Fixtures.epoch, end: Fixtures.epoch)
        )
        #expect(throws: PredicateValidationError.invalidRange) {
            try PredicateValidator.validate(empty)
        }

        // An inclusive numeric range with equal bounds is a legal single value.
        #expect(throws: Never.self) {
            try PredicateValidator.validate(
                Fixtures.term(.rating, .inRange, .numberRange(lower: 3, upper: 3))
            )
        }
    }

    // MARK: Grammar table

    @Test("every known field has a type class and a non-empty operator list")
    func everyFieldIsGrammatical() {
        for field in QueryField.knownCases {
            #expect(field.typeClass != nil, "\(field.rawValue) has no type class")
            #expect(!field.validOperators.isEmpty, "\(field.rawValue) admits no operator")
        }
    }

    @Test("the operator picker and the validator agree for every field")
    func pickerMatchesValidator() throws {
        // The predicate editor renders its picker from `validOperators`. If the
        // validator rejected something the picker offered, the user would build
        // a term they cannot save.
        for field in QueryField.knownCases {
            for operatorKind in field.validOperators {
                let shape = QueryGrammar.requiredOperandShape(
                    field: field,
                    // swiftlint:disable:next force_unwrapping
                    fieldClass: field.typeClass!,
                    operatorKind: operatorKind
                )
                #expect(shape != nil, "\(field.rawValue) \(operatorKind.rawValue) has no operand shape")
            }
        }
    }

    // MARK: Empty-collection semantics

    @Test("empty all matches everything and empty any matches nothing")
    func emptyCollectionSemantics() throws {
        // Both are legal predicates and both validate; the asymmetry is in what
        // they *mean*, which is documented on the cases and asserted here so the
        // meaning cannot be quietly swapped.
        try PredicateValidator.validate(SmartAlbumPredicate.all([]))
        try PredicateValidator.validate(SmartAlbumPredicate.any([]))
        #expect(SmartAlbumPredicate.all([]).terms.isEmpty)
        #expect(SmartAlbumPredicate.any([]).terms.isEmpty)
        #expect(SmartAlbumPredicate.all([]).depth == 1)
    }

    // MARK: Forward schema

    @Test("a definition from a newer grammar is preserved and not evaluated")
    func forwardSchemaIsPreservedNotEvaluated() {
        let ahead = SmartAlbumDefinition(
            smartAlbumID: SmartAlbumID("11111111-1111-7111-8111-111111111111"),
            predicateSchema: SmartAlbumDefinition.currentPredicateSchema + 1,
            displayName: Lww(current: Fixtures.stamped("From the future", offsetSeconds: 0)),
            predicate: .term(Fixtures.ratingTerm)
        )
        #expect(!ahead.isEvaluable)

        let current = SmartAlbumDefinition(
            smartAlbumID: SmartAlbumID("22222222-2222-7222-8222-222222222222"),
            displayName: Lww(current: Fixtures.stamped("Now", offsetSeconds: 0)),
            predicate: .term(Fixtures.ratingTerm)
        )
        #expect(current.isEvaluable)
    }

    @Test("a definition with no sort falls back to the documented default")
    func defaultSort() {
        let definition = SmartAlbumDefinition(
            smartAlbumID: SmartAlbumID("33333333-3333-7333-8333-333333333333"),
            displayName: Lww(),
            predicate: .all([])
        )
        #expect(definition.effectiveSort == SortSpec(key: .captureTimestamp, direction: .descending))
    }
}
