package com.justin13888.capsule.feature.albums.builder

import com.justin13888.capsule.core.model.ModelSlot
import com.justin13888.capsule.core.model.Operator
import com.justin13888.capsule.core.model.Predicate
import com.justin13888.capsule.core.model.QueryField

/** A node's identity inside one draft, stable across every edit that does not remove it. */
@JvmInline
value class DraftNodeId(
    val value: Long,
)

/** Whether a group demands every child or merely one of them. */
enum class GroupKind {
    /** Conjunction. An empty one matches every asset, exactly as the grammar says. */
    All,

    /** Disjunction. An empty one matches nothing. */
    Any,
}

/**
 * One node of the rule the user is building.
 *
 * Negation is a flag on every node rather than a node of its own, and that is the one place
 * this shape departs from [Predicate]. A `Not` node in the domain tree holds exactly one
 * operand, so an editor that modelled it directly would have to police that arity on every
 * add and remove — and would still leave the user asking why "not" needs a wrapper before it
 * can be applied to a single condition. A flag compiles to the same tree and cannot be put
 * into an invalid shape.
 */
sealed interface DraftNode {
    /** Stable within the draft. */
    val id: DraftNodeId

    /** Whether the node's own result is inverted, i.e. wrapped in a `Not` on compile. */
    val negated: Boolean
}

/** A group of child nodes, combined with AND or OR. */
data class DraftGroup(
    override val id: DraftNodeId,
    override val negated: Boolean = false,
    val kind: GroupKind = GroupKind.All,
    val children: List<DraftNode> = emptyList(),
) : DraftNode

/**
 * One comparison: a field, an operator legal for that field's type class, and a literal
 * whose shape that pair fixes.
 *
 * [modelSlot] is required for the two AI-derived fields and forbidden elsewhere, because a
 * term that did not name its slot would have to compare across model versions to evaluate —
 * and those vector spaces differ, so the comparison would be meaningless.
 */
data class DraftCondition(
    override val id: DraftNodeId,
    override val negated: Boolean = false,
    val field: QueryField,
    val op: Operator,
    val operand: DraftOperand,
    val modelSlot: ModelSlot? = null,
) : DraftNode

/**
 * The whole rule under construction, plus the counter that mints node ids.
 *
 * Immutable, and every edit returns a new one. That is what makes the editor's bounds checks
 * honest: an edit is applied to a candidate draft and the candidate is measured, so "would
 * this exceed depth 8" is answered by the same code that answers "how deep is this rule",
 * rather than by a second implementation that can drift from it.
 */
data class PredicateDraft(
    val root: DraftGroup,
    val nextId: Long,
) {
    /** The node with [id], or `null` when it has been removed. */
    fun find(id: DraftNodeId): DraftNode? = findIn(root, id)

    /** The tree's nesting depth, counted the way the definition validator counts it. */
    val depth: Int get() = depthOf(root)

    /** How many leaf conditions the tree carries. */
    val termCount: Int get() = termCountOf(root)

    companion object {
        /** An empty rule: one `all` group, which matches every asset until it is narrowed. */
        fun empty(): PredicateDraft = PredicateDraft(root = DraftGroup(id = DraftNodeId(0)), nextId = 1)

        /** The maximum nesting depth the grammar allows. */
        const val MAX_DEPTH = Predicate.MAX_DEPTH

        /** The maximum number of conditions the grammar allows. */
        const val MAX_TERMS = Predicate.MAX_TERMS
    }
}

/**
 * A node's depth, matching `SmartAlbumValidator.depthOf` term for term.
 *
 * A negated node compiles to `Not(node)`, which is one level deeper than the node alone —
 * so the flag has to be counted here, or the editor would allow a rule the validator then
 * refuses.
 */
internal fun depthOf(node: DraftNode): Int {
    val own =
        when (node) {
            is DraftCondition -> 1
            is DraftGroup -> 1 + (node.children.maxOfOrNull(::depthOf) ?: 0)
        }
    return if (node.negated) own + 1 else own
}

/** How many leaf conditions sit under [node], including itself. */
internal fun termCountOf(node: DraftNode): Int =
    when (node) {
        is DraftCondition -> 1
        is DraftGroup -> node.children.sumOf(::termCountOf)
    }

private fun findIn(
    node: DraftNode,
    id: DraftNodeId,
): DraftNode? {
    if (node.id == id) return node
    if (node !is DraftGroup) return null
    return node.children.firstNotNullOfOrNull { findIn(it, id) }
}
