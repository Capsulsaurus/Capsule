package com.justin13888.capsule.core.model

/**
 * Why a [SmartAlbumDefinition] was refused.
 *
 * Every variant is a **structural** rejection — the definition is malformed, not merely
 * unsupported. The distinction matters: a malformed definition is refused, while a
 * well-formed one from a newer `predicate_schema` is preserved and simply not evaluated
 * ([SmartAlbumValidation.PreservedUnevaluated]).
 */
sealed interface PredicateViolation {
    /** A term named a field outside the closed set. */
    data class UnknownField(
        val raw: String,
    ) : PredicateViolation

    /** A term named an operator outside the closed set. */
    data class UnknownOperator(
        val raw: String,
    ) : PredicateViolation

    /** The operator is real but not legal for this field's type class. */
    data class OperatorNotValidForField(
        val field: QueryField,
        val op: Operator,
    ) : PredicateViolation

    /** The operand's shape does not match the one fixed by `(field, op)`. */
    data class OperandTypeMismatch(
        /** The field the term queries. */
        val field: QueryField,
        /** The operator applied. */
        val op: Operator,
        /** The shape that arrived instead of the expected one. */
        val actual: Operand,
    ) : PredicateViolation

    /** An enumerated field's operand carried a value outside that field's own closed enum. */
    data class OperandNotInClosedEnum(
        val field: QueryField,
        val raw: String,
    ) : PredicateViolation

    /** The tree nests deeper than [Predicate.MAX_DEPTH]. */
    data class DepthExceeded(
        val actual: Int,
    ) : PredicateViolation

    /** The tree carries more than [Predicate.MAX_TERMS] leaves. */
    data class TermCountExceeded(
        val actual: Int,
    ) : PredicateViolation

    /** A set operand carried more than [Predicate.MAX_SET_MEMBERS] members. */
    data class SetOperandTooLarge(
        val actual: Int,
    ) : PredicateViolation

    /** A set member exceeded [Predicate.MAX_SET_MEMBER_BYTES]. */
    data class SetMemberTooLong(
        val member: String,
        val actualBytes: Int,
    ) : PredicateViolation

    /** The display name exceeded [SmartAlbumDefinition.MAX_DISPLAY_NAME_BYTES]. */
    data class DisplayNameTooLong(
        val actualBytes: Int,
    ) : PredicateViolation

    /** An AI-derived field was queried without naming the model slot it queries. */
    data class MissingModelSlot(
        val field: QueryField,
    ) : PredicateViolation

    /**
     * A range operand's bounds are inverted, so it can never match.
     *
     * Not named in the design table, which specifies the *shape* of a range but not its
     * ordering. Refusing it here is a deliberate reading of "an operand mistyped for its
     * `(field, op)` is a structural rejection": a half-open window that ends before it starts
     * is not a window, and admitting it would give the user a smart album that is silently
     * always empty.
     */
    data class RangeInverted(
        val field: QueryField,
    ) : PredicateViolation
}

/**
 * What a client may do with a [SmartAlbumDefinition] it has loaded.
 *
 * Three outcomes rather than two, because "cannot evaluate" and "must refuse" are different
 * obligations. A definition from a newer grammar must be kept intact and synced onward; a
 * malformed one must be refused at the validator. Collapsing them would either strip a
 * newer client's work or execute a definition nobody wrote.
 */
sealed interface SmartAlbumValidation {
    /** Well formed and within this build's grammar — evaluate it. */
    data object Evaluable : SmartAlbumValidation

    /**
     * Authored under a later `predicate_schema` than this build knows.
     *
     * Preserved verbatim through every sync round-trip, surfaced as "created by a newer app
     * version", never evaluated and **never stripped**.
     */
    data class PreservedUnevaluated(
        val predicateSchema: Int,
    ) : SmartAlbumValidation

    /** Structurally invalid. Every violation found, not just the first. */
    data class Rejected(
        val violations: List<PredicateViolation>,
    ) : SmartAlbumValidation
}

/**
 * The definition validator — the single gate a smart album passes before it is evaluated.
 *
 * Reports **every** violation rather than the first, because a definition arriving over sync
 * cannot be fixed interactively: the user needs the whole list to understand what the other
 * device wrote.
 */
object SmartAlbumValidator {
    /**
     * Validates [definition] against the closed grammar and its bounds.
     *
     * The forward-schema check runs first and short-circuits: a definition from a grammar
     * this build does not know may legitimately use fields and operators that do not exist
     * here, so validating it against this build's closed sets would report violations that
     * are not violations at all.
     */
    fun validate(
        definition: SmartAlbumDefinition,
        maxKnownSchema: Int = SmartAlbumDefinition.MAX_KNOWN_PREDICATE_SCHEMA,
    ): SmartAlbumValidation {
        if (definition.predicateSchema > maxKnownSchema) {
            return SmartAlbumValidation.PreservedUnevaluated(definition.predicateSchema)
        }
        val violations =
            buildList {
                addAll(displayNameViolations(definition))
                addAll(boundsViolations(definition.predicate))
                leaves(definition.predicate).forEach { addAll(termViolations(it)) }
            }
        return if (violations.isEmpty()) {
            SmartAlbumValidation.Evaluable
        } else {
            SmartAlbumValidation.Rejected(violations)
        }
    }

    /** The tree's nesting depth; a leaf is 1, and an empty [Predicate.All] or [Predicate.Any] is 1. */
    fun depthOf(predicate: Predicate): Int =
        when (predicate) {
            is Predicate.Leaf -> 1
            is Predicate.Not -> 1 + depthOf(predicate.operand)
            is Predicate.All -> 1 + (predicate.operands.maxOfOrNull(::depthOf) ?: 0)
            is Predicate.Any -> 1 + (predicate.operands.maxOfOrNull(::depthOf) ?: 0)
        }

    /** Every leaf term in the tree, in traversal order. */
    fun leaves(predicate: Predicate): List<Term> =
        when (predicate) {
            is Predicate.Leaf -> listOf(predicate.term)
            is Predicate.Not -> leaves(predicate.operand)
            is Predicate.All -> predicate.operands.flatMap(::leaves)
            is Predicate.Any -> predicate.operands.flatMap(::leaves)
        }
}

// ── Validator internals ──────────────────────────────────────────────────────
//
// Top-level and private rather than members of [SmartAlbumValidator]: they are pure
// functions over their arguments with no state to share, and hoisting them keeps the object's
// own surface down to the three things a caller actually calls.

private fun displayNameViolations(definition: SmartAlbumDefinition): List<PredicateViolation> {
    val bytes =
        definition.displayName.value
            ?.toByteArray()
            ?.size ?: 0
    return if (bytes > SmartAlbumDefinition.MAX_DISPLAY_NAME_BYTES) {
        listOf(PredicateViolation.DisplayNameTooLong(bytes))
    } else {
        emptyList()
    }
}

private fun boundsViolations(predicate: Predicate): List<PredicateViolation> =
    buildList {
        val depth = SmartAlbumValidator.depthOf(predicate)
        if (depth > Predicate.MAX_DEPTH) add(PredicateViolation.DepthExceeded(depth))
        val terms = SmartAlbumValidator.leaves(predicate).size
        if (terms > Predicate.MAX_TERMS) add(PredicateViolation.TermCountExceeded(terms))
    }

private fun termViolations(term: Term): List<PredicateViolation> {
    val field =
        term.field.knownOrNull
            ?: return listOf(PredicateViolation.UnknownField(term.field.raw))
    val op =
        term.op.knownOrNull
            ?: return listOf(PredicateViolation.UnknownOperator(term.op.raw))
    if (field.fieldClass !in op.appliesTo) {
        return listOf(PredicateViolation.OperatorNotValidForField(field, op))
    }
    return buildList {
        if (field.requiresModelSlot && term.modelSlot == null) {
            add(PredicateViolation.MissingModelSlot(field))
        }
        val mismatch = shapeViolation(field, op, term.operand)
        if (mismatch != null) {
            add(mismatch)
        } else {
            addAll(operandContentViolations(field, term.operand))
        }
    }
}

/** Whether the operand's shape is the one `(field, op)` fixes. */
private fun shapeViolation(
    field: QueryField,
    op: Operator,
    operand: Operand,
): PredicateViolation? {
    val matches =
        when (op) {
            Operator.Before, Operator.After -> operand is Operand.TemporalPoint
            Operator.InRange -> rangeShapeMatches(field, operand)
            Operator.Eq -> eqShapeMatches(field, operand)
            Operator.In -> operand is Operand.EnumSet
            Operator.Gte, Operator.Lte -> operand is Operand.Number
            Operator.Is -> isShapeMatches(field, operand)
            Operator.Contains, Operator.ContainsAny, Operator.ContainsAll -> operand is Operand.StringSet
            Operator.Exists -> operand is Operand.Presence
        }
    return if (matches) null else PredicateViolation.OperandTypeMismatch(field, op, operand)
}

private fun rangeShapeMatches(
    field: QueryField,
    operand: Operand,
): Boolean =
    when (field.fieldClass) {
        QueryFieldClass.Temporal -> operand is Operand.TemporalRange
        else -> operand is Operand.NumberRange
    }

private fun eqShapeMatches(
    field: QueryField,
    operand: Operand,
): Boolean =
    when (field.fieldClass) {
        QueryFieldClass.Enumerated -> operand is Operand.EnumValue
        else -> operand is Operand.Number
    }

/** `is` takes the queried field's *own* literal — a cull flag for `cull`, a boolean for `hidden`. */
private fun isShapeMatches(
    field: QueryField,
    operand: Operand,
): Boolean =
    when (field) {
        QueryField.Cull -> operand is Operand.CullLiteral
        else -> operand is Operand.BoolLiteral
    }

private fun operandContentViolations(
    field: QueryField,
    operand: Operand,
): List<PredicateViolation> =
    when (operand) {
        is Operand.EnumValue -> closedEnumViolations(field, listOf(operand.raw))
        is Operand.EnumSet -> setBoundViolations(operand.raw) + closedEnumViolations(field, operand.raw)
        is Operand.StringSet -> setBoundViolations(operand.members)
        is Operand.TemporalRange ->
            if (!operand.endExclusive.isAfter(operand.start)) {
                listOf(PredicateViolation.RangeInverted(field))
            } else {
                emptyList()
            }

        is Operand.NumberRange ->
            if (operand.hi < operand.lo) listOf(PredicateViolation.RangeInverted(field)) else emptyList()

        else -> emptyList()
    }

private fun setBoundViolations(members: List<String>): List<PredicateViolation> =
    buildList {
        if (members.size > Predicate.MAX_SET_MEMBERS) {
            add(PredicateViolation.SetOperandTooLarge(members.size))
        }
        members.forEach { member ->
            val bytes = member.toByteArray().size
            if (bytes > Predicate.MAX_SET_MEMBER_BYTES) {
                add(PredicateViolation.SetMemberTooLong(member, bytes))
            }
        }
    }

/**
 * Checks an enumerated field's operand against that field's *own* closed enum.
 *
 * Applied only to the `enum` type class, matching the grammar table exactly: it says an
 * enumerated operand is "a value / set drawn from the field's own closed enum", while a
 * set-valued operand is only "a string / id set". `stack_type` sits in the set class and
 * so is bounds-checked but not enum-checked here, even though its values do come from a
 * closed enum elsewhere — the table's split is deliberate and this validator follows it
 * rather than tightening it unilaterally.
 */
private fun closedEnumViolations(
    field: QueryField,
    values: List<String>,
): List<PredicateViolation> {
    val codec: WireCodec<out WireEnum> =
        when (field) {
            QueryField.ContentType -> ContentType
            QueryField.MediaKind -> MediaKind
            QueryField.GpsDatum -> GpsDatum
            else -> return emptyList()
        }
    return values
        .filter { codec.fromWireOrNull(it) == null }
        .map { PredicateViolation.OperandNotInClosedEnum(field, it) }
}
