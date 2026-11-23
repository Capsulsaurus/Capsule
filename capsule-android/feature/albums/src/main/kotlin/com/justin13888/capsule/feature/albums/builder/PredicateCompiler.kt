package com.justin13888.capsule.feature.albums.builder

import com.justin13888.capsule.core.model.Lww
import com.justin13888.capsule.core.model.LwwStamp
import com.justin13888.capsule.core.model.Operand
import com.justin13888.capsule.core.model.Predicate
import com.justin13888.capsule.core.model.SmartAlbumDefinition
import com.justin13888.capsule.core.model.SmartAlbumId
import com.justin13888.capsule.core.model.SortSpec
import com.justin13888.capsule.core.model.Term
import com.justin13888.capsule.core.model.asWire
import com.justin13888.capsule.core.model.knownOrNull

/** What came of turning the draft into a definition. */
sealed interface DraftCompilation {
    /** Every condition was complete; [predicate] is the tree the validator will see. */
    data class Compiled(
        val predicate: Predicate,
    ) : DraftCompilation

    /** These conditions are still half-typed, so no tree exists yet. */
    data class Incomplete(
        val nodes: List<DraftNodeId>,
    ) : DraftCompilation
}

/**
 * Moves a rule between the editor's shape and the stored one.
 *
 * Both directions are needed and neither is total, which is worth stating plainly. Compiling
 * fails while an operand is half-typed — that is the editor's normal state, not an error.
 * Decompiling fails when a definition names a field or operator this build does not know —
 * such a definition must be preserved and synced onward verbatim, so the right answer is to
 * refuse to open it in the editor rather than to open it and quietly drop the term.
 */
object PredicateCompiler {
    /** Turns the draft into a predicate, or names the conditions that are not ready. */
    fun compile(draft: PredicateDraft): DraftCompilation {
        val incomplete = incompleteNodes(draft.root)
        if (incomplete.isNotEmpty()) return DraftCompilation.Incomplete(incomplete)
        return DraftCompilation.Compiled(toPredicate(draft.root))
    }

    /**
     * Assembles the whole definition a save would write.
     *
     * One stamped write of the *whole* definition, never field-wise: that is what stops two
     * devices editing the same smart album from merging into a tree neither of them wrote.
     */
    @Suppress("LongParameterList")
    fun toDefinition(
        id: SmartAlbumId,
        displayName: String,
        predicate: Predicate,
        sort: SortSpec?,
        stamp: LwwStamp,
        predicateSchema: Int = SmartAlbumDefinition.MAX_KNOWN_PREDICATE_SCHEMA,
    ): SmartAlbumDefinition =
        SmartAlbumDefinition(
            id = id,
            predicateSchema = predicateSchema,
            displayName = Lww(value = displayName, stamp = stamp),
            predicate = predicate,
            sort = sort,
        )

    /**
     * Reads a stored definition back into the editor, or `null` when it cannot be edited.
     *
     * `null` is returned for a definition carrying a token outside this build's closed sets.
     * Opening it would mean either dropping the term — which loses another device's work —
     * or representing something the editor has no controls for.
     */
    fun toDraft(definition: SmartAlbumDefinition): PredicateDraft? {
        var counter = 0L
        val root = toNode(definition.predicate, negated = false) { counter++ } ?: return null
        val group = root as? DraftGroup ?: DraftGroup(id = DraftNodeId(counter++), children = listOf(root))
        return PredicateDraft(root = group, nextId = counter)
    }
}

/** The conditions whose operands do not yet parse, in traversal order. */
private fun incompleteNodes(node: DraftNode): List<DraftNodeId> =
    when (node) {
        is DraftCondition -> if (node.operand.compile() == null) listOf(node.id) else emptyList()
        is DraftGroup -> node.children.flatMap(::incompleteNodes)
    }

/** Compiles a node whose subtree is known to be complete. */
private fun toPredicate(node: DraftNode): Predicate {
    val bare =
        when (node) {
            is DraftCondition ->
                Predicate.Leaf(
                    Term(
                        field = node.field.asWire(),
                        op = node.op.asWire(),
                        operand = requireNotNull(node.operand.compile()) { "operand checked before compile" },
                        modelSlot = node.modelSlot,
                    ),
                )

            is DraftGroup -> {
                val operands = node.children.map(::toPredicate)
                if (node.kind == GroupKind.All) Predicate.All(operands) else Predicate.Any(operands)
            }
        }
    return if (node.negated) Predicate.Not(bare) else bare
}

/**
 * Reads one predicate node back into a draft node.
 *
 * A `Not` collapses into the negation flag on whatever it wraps; a `Not(Not(x))` therefore
 * needs a group to hang the second flag on, which is what the wrapper below is for.
 */
private fun toNode(
    predicate: Predicate,
    negated: Boolean,
    nextId: () -> Long,
): DraftNode? =
    when (predicate) {
        is Predicate.Not -> {
            val inner = toNode(predicate.operand, negated = false, nextId = nextId) ?: return null
            if (!inner.negated) {
                withNegation(inner, negated = true)
            } else {
                DraftGroup(id = DraftNodeId(nextId()), negated = true, children = listOf(inner))
            }
        }

        is Predicate.All, is Predicate.Any -> {
            val operands = operandsOf(predicate)
            val children = operands.map { toNode(it, negated = false, nextId = nextId) }
            if (children.any { it == null }) {
                null
            } else {
                DraftGroup(
                    id = DraftNodeId(nextId()),
                    negated = negated,
                    kind = if (predicate is Predicate.All) GroupKind.All else GroupKind.Any,
                    children = children.filterNotNull(),
                )
            }
        }

        is Predicate.Leaf -> toCondition(predicate.term, negated, nextId)
    }

private fun operandsOf(predicate: Predicate): List<Predicate> =
    when (predicate) {
        is Predicate.All -> predicate.operands
        is Predicate.Any -> predicate.operands
        else -> emptyList()
    }

private fun withNegation(
    node: DraftNode,
    negated: Boolean,
): DraftNode =
    when (node) {
        is DraftGroup -> node.copy(negated = negated)
        is DraftCondition -> node.copy(negated = negated)
    }

private fun toCondition(
    term: Term,
    negated: Boolean,
    nextId: () -> Long,
): DraftCondition? {
    val field = term.field.knownOrNull ?: return null
    val op = term.op.knownOrNull ?: return null
    val operand = toDraftOperand(term.operand) ?: return null
    return DraftCondition(
        id = DraftNodeId(nextId()),
        negated = negated,
        field = field,
        op = op,
        operand = operand,
        modelSlot = term.modelSlot,
    )
}

private fun toDraftOperand(operand: Operand): DraftOperand? =
    when (operand) {
        is Operand.TemporalPoint -> DraftOperand.TemporalPoint(DraftOperand.formatDate(operand.at))
        is Operand.TemporalRange ->
            DraftOperand.TemporalRange(
                DraftOperand.formatDate(operand.start),
                DraftOperand.formatDate(operand.endExclusive),
            )

        is Operand.EnumValue -> DraftOperand.EnumValue(operand.raw)
        is Operand.EnumSet -> DraftOperand.EnumSet(operand.raw)
        is Operand.Number -> DraftOperand.Number(operand.value.toString())
        is Operand.NumberRange -> DraftOperand.NumberRange(operand.lo.toString(), operand.hi.toString())
        is Operand.CullLiteral -> DraftOperand.Cull(operand.flag)
        is Operand.BoolLiteral -> DraftOperand.Bool(operand.value)
        is Operand.StringSet -> DraftOperand.Strings(operand.members)
        is Operand.Presence -> DraftOperand.Presence(operand.mustExist)
    }
