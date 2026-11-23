package com.justin13888.capsule.feature.albums.builder

import com.justin13888.capsule.core.model.Operator
import com.justin13888.capsule.core.model.QueryField
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.time.Instant

/**
 * The editor refuses invalid combinations structurally rather than coercing them.
 *
 * That distinction is the whole reason this layer exists. The definition validator would
 * catch every one of these at save time, but by then the user has built a rule they cannot
 * save and has to work out which part is wrong. Refusing the edit — with a reason, and
 * leaving the rule untouched — moves the failure to the moment it was caused.
 */
class PredicateEditingTest {
    private val now: Instant = Instant.parse("2026-06-01T00:00:00Z")

    private fun applied(edit: DraftEdit): PredicateDraft = assertInstanceOf(DraftEdit.Applied::class.java, edit).draft

    private fun refusal(edit: DraftEdit): EditRefusal = assertInstanceOf(DraftEdit.Refused::class.java, edit).reason

    @Test
    fun `a new condition starts on a legal field and operator pair`() {
        val draft = applied(PredicateEditing.addCondition(PredicateDraft.empty(), DraftNodeId(0), now))
        val condition = draft.root.children.single() as DraftCondition
        assertTrue(condition.field.fieldClass in condition.op.appliesTo)
        assertEquals(1, draft.termCount)
    }

    @Test
    fun `an operator outside the field's type class is refused, not applied`() {
        val draft = applied(PredicateEditing.addCondition(PredicateDraft.empty(), DraftNodeId(0), now))
        val id =
            draft.root.children
                .single()
                .id
        val edit = PredicateEditing.setOperator(draft, id, Operator.Contains, now)
        val expected = EditRefusal.OperatorNotValidForField(QueryField.CaptureTimestamp, Operator.Contains)
        assertEquals(expected, refusal(edit))
    }

    @Test
    fun `choosing a new field re-seeds the operator and the operand for its type class`() {
        val started = applied(PredicateEditing.addCondition(PredicateDraft.empty(), DraftNodeId(0), now))
        val id =
            started.root.children
                .single()
                .id
        val draft = applied(PredicateEditing.setField(started, id, QueryField.Rating, now))
        val condition = draft.find(id) as DraftCondition
        assertEquals(QueryField.Rating, condition.field)
        assertTrue(condition.field.fieldClass in condition.op.appliesTo)
        // The seeded operand is whatever the *new* pair fixes, never whatever the old one
        // held: a temporal point left behind on a numeric field is a term the validator
        // refuses outright.
        assertEquals(PredicateEditing.firstOperatorFor(QueryField.Rating), condition.op)
        assertInstanceOf(DraftOperand.NumberRange::class.java, condition.operand)
    }

    @Test
    fun `an AI-derived field gains the model slot it is required to name`() {
        val started = applied(PredicateEditing.addCondition(PredicateDraft.empty(), DraftNodeId(0), now))
        val id =
            started.root.children
                .single()
                .id
        val draft = applied(PredicateEditing.setField(started, id, QueryField.TagsAi, now))
        assertEquals(PredicateEditing.DEFAULT_SLOT, (draft.find(id) as DraftCondition).modelSlot)
    }

    @Test
    fun `nesting stops at the grammar's depth limit`() {
        var draft = PredicateDraft.empty()
        var parent = draft.root.id
        // The root is depth 1, so seven more groups reach the limit exactly.
        repeat(PredicateDraft.MAX_DEPTH - 1) {
            draft = applied(PredicateEditing.addGroup(draft, parent, GroupKind.All))
            parent = deepest(draft).id
        }
        assertEquals(PredicateDraft.MAX_DEPTH, draft.depth)
        assertEquals(EditRefusal.DepthLimit, refusal(PredicateEditing.addGroup(draft, parent, GroupKind.All)))
    }

    @Test
    fun `negating a node counts towards the depth limit, because it compiles to a wrapper`() {
        var draft = PredicateDraft.empty()
        var parent = draft.root.id
        repeat(PredicateDraft.MAX_DEPTH - 1) {
            draft = applied(PredicateEditing.addGroup(draft, parent, GroupKind.All))
            parent = deepest(draft).id
        }
        assertEquals(EditRefusal.DepthLimit, refusal(PredicateEditing.setNegated(draft, parent, negated = true)))
    }

    @Test
    fun `the term count stops at the grammar's limit`() {
        var draft = PredicateDraft.empty()
        repeat(PredicateDraft.MAX_TERMS) { draft = applied(PredicateEditing.addCondition(draft, draft.root.id, now)) }
        assertEquals(PredicateDraft.MAX_TERMS, draft.termCount)
        assertEquals(EditRefusal.TermLimit, refusal(PredicateEditing.addCondition(draft, draft.root.id, now)))
    }

    @Test
    fun `a set operand may not exceed sixty-four members`() {
        val draft = applied(PredicateEditing.addCondition(PredicateDraft.empty(), DraftNodeId(0), now))
        val id =
            draft.root.children
                .single()
                .id
        val tooMany = DraftOperand.Strings(List(DraftOperand.MAX_SET_MEMBERS + 1) { "tag$it" })
        assertEquals(EditRefusal.SetMemberLimit, refusal(PredicateEditing.setOperand(draft, id, tooMany)))
    }

    @Test
    fun `a set member may not exceed its byte limit`() {
        val draft = applied(PredicateEditing.addCondition(PredicateDraft.empty(), DraftNodeId(0), now))
        val id =
            draft.root.children
                .single()
                .id
        val long = "x".repeat(DraftOperand.MAX_SET_MEMBER_BYTES + 1)
        val refused = refusal(PredicateEditing.setOperand(draft, id, DraftOperand.Strings(listOf(long))))
        assertInstanceOf(EditRefusal.SetMemberTooLong::class.java, refused)
    }

    @Test
    fun `removing a group takes its children with it, and the root cannot be removed`() {
        val withGroup = applied(PredicateEditing.addGroup(PredicateDraft.empty(), DraftNodeId(0), GroupKind.Any))
        val groupId =
            withGroup.root.children
                .single()
                .id
        val withChild = applied(PredicateEditing.addCondition(withGroup, groupId, now))
        assertEquals(1, withChild.termCount)

        val pruned = applied(PredicateEditing.remove(withChild, groupId))
        assertEquals(0, pruned.termCount)
        assertTrue(pruned.root.children.isEmpty())

        val rootRemoved = applied(PredicateEditing.remove(pruned, pruned.root.id))
        assertEquals(pruned.root.id, rootRemoved.root.id)
    }

    @Test
    fun `only operators legal for a field are offered`() {
        assertEquals(
            listOf(Operator.Contains, Operator.ContainsAny, Operator.ContainsAll),
            PredicateEditing.operatorsFor(QueryField.TagsUser),
        )
        assertEquals(listOf(Operator.Exists), PredicateEditing.operatorsFor(QueryField.Gps))
    }

    private fun deepest(draft: PredicateDraft): DraftNode {
        var node: DraftNode = draft.root
        while (node is DraftGroup && node.children.isNotEmpty()) node = node.children.last()
        return node
    }
}
