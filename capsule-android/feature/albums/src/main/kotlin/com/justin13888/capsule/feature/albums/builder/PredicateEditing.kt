package com.justin13888.capsule.feature.albums.builder

import com.justin13888.capsule.core.model.ModelId
import com.justin13888.capsule.core.model.ModelSlot
import com.justin13888.capsule.core.model.ModelVersion
import com.justin13888.capsule.core.model.Operator
import com.justin13888.capsule.core.model.QueryField
import java.time.Instant

/** Why an edit to the rule was refused outright rather than applied and flagged. */
sealed interface EditRefusal {
    /** The rule would nest deeper than the grammar's limit. */
    data object DepthLimit : EditRefusal

    /** The rule would carry more conditions than the grammar's limit. */
    data object TermLimit : EditRefusal

    /** A value list would hold more members than the grammar's limit. */
    data object SetMemberLimit : EditRefusal

    /** One value exceeded the grammar's per-member length limit. */
    data class SetMemberTooLong(
        val member: String,
        val bytes: Int,
    ) : EditRefusal

    /** The operator is real, but not legal for this field's type class. */
    data class OperatorNotValidForField(
        val field: QueryField,
        val op: Operator,
    ) : EditRefusal
}

/** The outcome of one edit: a new draft, or the reason nothing moved. */
sealed interface DraftEdit {
    /** The edit was legal; [draft] is the rule as it now stands. */
    data class Applied(
        val draft: PredicateDraft,
    ) : DraftEdit

    /** Nothing changed, and this is why. */
    data class Refused(
        val reason: EditRefusal,
    ) : DraftEdit
}

/**
 * Every structural edit the smart-album builder can make, as pure functions.
 *
 * The design calls an invalid combination a **structural rejection** rather than something
 * to tolerate, and this is where that is honoured: an illegal edit is refused here, with a
 * reason, and never applied-then-corrected. Nothing in this object silently coerces —
 * choosing a new field resets the operator and the operand because their shapes are fixed by
 * the field's type class, which is a *consequence* of the user's edit rather than a
 * correction of it, and the reset is visible on screen the moment it happens.
 *
 * Bounds are checked by applying the edit to a candidate and measuring the candidate with
 * the same functions that report the rule's size. One implementation, so the editor and the
 * validator cannot disagree about what depth 8 means.
 *
 * There is one function per edit the grammar admits, which is why the count is what it is:
 * folding two of them together would mean a caller had to say *which* edit it meant through a
 * parameter, and the compiler would stop checking that the operand matched the operation.
 */
@Suppress("TooManyFunctions")
object PredicateEditing {
    /** Appends a condition under [parentId], seeded with the first legal `(field, op)` pair. */
    fun addCondition(
        draft: PredicateDraft,
        parentId: DraftNodeId,
        now: Instant,
    ): DraftEdit {
        val field = QueryField.CaptureTimestamp
        val op = firstOperatorFor(field)
        val condition =
            DraftCondition(
                id = DraftNodeId(draft.nextId),
                field = field,
                op = op,
                operand = DraftOperand.defaultFor(field, op, now),
            )
        return bounded(draft.copy(root = insert(draft.root, parentId, condition), nextId = draft.nextId + 1))
    }

    /** Appends an empty group under [parentId]. */
    fun addGroup(
        draft: PredicateDraft,
        parentId: DraftNodeId,
        kind: GroupKind,
    ): DraftEdit {
        val group = DraftGroup(id = DraftNodeId(draft.nextId), kind = kind)
        return bounded(draft.copy(root = insert(draft.root, parentId, group), nextId = draft.nextId + 1))
    }

    /** Removes [id] and everything under it. The root is never removable. */
    fun remove(
        draft: PredicateDraft,
        id: DraftNodeId,
    ): DraftEdit {
        if (id == draft.root.id) return DraftEdit.Applied(draft)
        return DraftEdit.Applied(draft.copy(root = prune(draft.root, id)))
    }

    /** Inverts, or un-inverts, one node — the grammar's `not`, applied where the user is. */
    fun setNegated(
        draft: PredicateDraft,
        id: DraftNodeId,
        negated: Boolean,
    ): DraftEdit =
        bounded(
            draft.copy(
                root =
                    transform(draft.root, id) { node ->
                        when (node) {
                            is DraftGroup -> node.copy(negated = negated)
                            is DraftCondition -> node.copy(negated = negated)
                        }
                    },
            ),
        )

    /** Switches a group between AND and OR. */
    fun setGroupKind(
        draft: PredicateDraft,
        id: DraftNodeId,
        kind: GroupKind,
    ): DraftEdit =
        DraftEdit.Applied(
            draft.copy(
                root = transform(draft.root, id) { node -> if (node is DraftGroup) node.copy(kind = kind) else node },
            ),
        )

    /**
     * Points a condition at a different field.
     *
     * The operator and operand are re-seeded from the new field's type class. They have to
     * be: `rating before 2026-01-01` is not a rule with a fixable operand, it is a rule the
     * validator refuses outright, and carrying the old operator forward would let the user
     * build one.
     */
    fun setField(
        draft: PredicateDraft,
        id: DraftNodeId,
        field: QueryField,
        now: Instant,
    ): DraftEdit {
        val op = firstOperatorFor(field)
        return DraftEdit.Applied(
            draft.copy(
                root =
                    transform(draft.root, id) { node ->
                        if (node !is DraftCondition) {
                            node
                        } else {
                            node.copy(
                                field = field,
                                op = op,
                                operand = DraftOperand.defaultFor(field, op, now),
                                modelSlot = if (field.requiresModelSlot) node.modelSlot ?: DEFAULT_SLOT else null,
                            )
                        }
                    },
            ),
        )
    }

    /** Changes a condition's operator, refusing one that is illegal for its field. */
    fun setOperator(
        draft: PredicateDraft,
        id: DraftNodeId,
        op: Operator,
        now: Instant,
    ): DraftEdit {
        val condition = draft.find(id) as? DraftCondition ?: return DraftEdit.Applied(draft)
        if (condition.field.fieldClass !in op.appliesTo) {
            return DraftEdit.Refused(EditRefusal.OperatorNotValidForField(condition.field, op))
        }
        return DraftEdit.Applied(
            draft.copy(
                root =
                    transform(draft.root, id) { node ->
                        if (node !is DraftCondition) {
                            node
                        } else {
                            node.copy(op = op, operand = DraftOperand.defaultFor(node.field, op, now))
                        }
                    },
            ),
        )
    }

    /** Writes a condition's operand, enforcing the grammar's bounds on set operands. */
    fun setOperand(
        draft: PredicateDraft,
        id: DraftNodeId,
        operand: DraftOperand,
    ): DraftEdit {
        setOperandRefusal(operand)?.let { return DraftEdit.Refused(it) }
        return DraftEdit.Applied(
            draft.copy(
                root =
                    transform(draft.root, id) { node ->
                        if (node is DraftCondition) node.copy(operand = operand) else node
                    },
            ),
        )
    }

    /** Names the `(model_id, model_version)` slot an AI-derived condition queries. */
    fun setModelSlot(
        draft: PredicateDraft,
        id: DraftNodeId,
        slot: ModelSlot,
    ): DraftEdit =
        DraftEdit.Applied(
            draft.copy(
                root =
                    transform(draft.root, id) { node ->
                        if (node is DraftCondition && node.field.requiresModelSlot) {
                            node.copy(modelSlot = slot)
                        } else {
                            node
                        }
                    },
            ),
        )

    /** The operators legal for [field], in the order the picker offers them. */
    fun operatorsFor(field: QueryField): List<Operator> = Operator.entries.filter { field.fieldClass in it.appliesTo }

    /** The first legal operator for [field]; every type class has at least one. */
    fun firstOperatorFor(field: QueryField): Operator = operatorsFor(field).first()

    /** The slot a freshly created AI-derived condition starts on. */
    val DEFAULT_SLOT: ModelSlot =
        ModelSlot(
            modelId = ModelId("mobileclip-b"),
            modelVersion = ModelVersion("1"),
        )
}

/** The grammar's bound a set operand would break, or `null` when it breaks none. */
private fun setOperandRefusal(operand: DraftOperand): EditRefusal? {
    val members =
        when (operand) {
            is DraftOperand.Strings -> operand.members
            is DraftOperand.EnumSet -> operand.raw
            else -> return null
        }
    if (members.size > DraftOperand.MAX_SET_MEMBERS) return EditRefusal.SetMemberLimit
    members.forEach { member ->
        val bytes = member.toByteArray().size
        if (bytes > DraftOperand.MAX_SET_MEMBER_BYTES) {
            return EditRefusal.SetMemberTooLong(member, bytes)
        }
    }
    return null
}

/**
 * Accepts a candidate only if it is still inside the grammar's depth and term bounds.
 *
 * Measured with the same functions that report the rule's size, so the editor and the
 * definition validator cannot end up disagreeing about what depth eight means.
 */
private fun bounded(candidate: PredicateDraft): DraftEdit =
    when {
        candidate.depth > PredicateDraft.MAX_DEPTH -> DraftEdit.Refused(EditRefusal.DepthLimit)
        candidate.termCount > PredicateDraft.MAX_TERMS -> DraftEdit.Refused(EditRefusal.TermLimit)
        else -> DraftEdit.Applied(candidate)
    }

/** Returns [root] with [child] appended under the group identified by [parentId]. */
private fun insert(
    root: DraftGroup,
    parentId: DraftNodeId,
    child: DraftNode,
): DraftGroup =
    transform(root, parentId) { node ->
        if (node is DraftGroup) node.copy(children = node.children + child) else node
    }

/** Returns [root] without the node identified by [id]. */
private fun prune(
    root: DraftGroup,
    id: DraftNodeId,
): DraftGroup =
    root.copy(
        children =
            root.children
                .filterNot { it.id == id }
                .map { child -> if (child is DraftGroup) prune(child, id) else child },
    )

/** Returns [root] with [change] applied to the node identified by [id]. */
private fun transform(
    root: DraftGroup,
    id: DraftNodeId,
    change: (DraftNode) -> DraftNode,
): DraftGroup {
    val changed = if (root.id == id) change(root) else root
    val group = changed as? DraftGroup ?: root
    return group.copy(children = group.children.map { child -> transformNode(child, id, change) })
}

private fun transformNode(
    node: DraftNode,
    id: DraftNodeId,
    change: (DraftNode) -> DraftNode,
): DraftNode {
    val changed = if (node.id == id) change(node) else node
    return if (changed is DraftGroup) {
        changed.copy(children = changed.children.map { transformNode(it, id, change) })
    } else {
        changed
    }
}
