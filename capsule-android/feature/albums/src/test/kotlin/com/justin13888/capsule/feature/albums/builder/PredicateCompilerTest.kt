package com.justin13888.capsule.feature.albums.builder

import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.Lww
import com.justin13888.capsule.core.model.LwwStamp
import com.justin13888.capsule.core.model.Operand
import com.justin13888.capsule.core.model.Operator
import com.justin13888.capsule.core.model.Predicate
import com.justin13888.capsule.core.model.QueryField
import com.justin13888.capsule.core.model.SmartAlbumDefinition
import com.justin13888.capsule.core.model.SmartAlbumId
import com.justin13888.capsule.core.model.Term
import com.justin13888.capsule.core.model.Wire
import com.justin13888.capsule.core.model.asWire
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test
import java.time.Instant

/**
 * Moving a rule between the editor's shape and the stored one.
 *
 * Neither direction is total, and both partial cases are load-bearing. A half-typed operand
 * has no tree at all, which is the editor's normal state rather than an error. A stored
 * definition naming a token this build does not know must *not* open in the editor, because
 * the only thing the editor could show is that definition with pieces missing — and saving
 * from there would overwrite another device's work with the truncation.
 */
class PredicateCompilerTest {
    private val stamp = LwwStamp(Instant.parse("2026-06-01T00:00:00Z"), DeviceId("device"))
    private val now: Instant = Instant.parse("2026-06-01T00:00:00Z")

    @Test
    fun `a complete rule compiles to the tree the validator will see`() {
        val draft = editedDraft()
        val compiled = assertInstanceOf(DraftCompilation.Compiled::class.java, PredicateCompiler.compile(draft))
        val all = assertInstanceOf(Predicate.All::class.java, compiled.predicate)
        val leaf = assertInstanceOf(Predicate.Leaf::class.java, all.operands.single())
        assertEquals(QueryField.Rating.asWire(), leaf.term.field)
    }

    @Test
    fun `a half-typed operand names the condition rather than compiling`() {
        val started = applied(PredicateEditing.addCondition(PredicateDraft.empty(), DraftNodeId(0), now))
        val id =
            started.root.children
                .single()
                .id
        val blank = applied(PredicateEditing.setOperand(started, id, DraftOperand.TemporalPoint("")))
        val incomplete = assertInstanceOf(DraftCompilation.Incomplete::class.java, PredicateCompiler.compile(blank))
        assertEquals(listOf(id), incomplete.nodes)
    }

    @Test
    fun `a negated node compiles to a not wrapper`() {
        val started = applied(PredicateEditing.addCondition(PredicateDraft.empty(), DraftNodeId(0), now))
        val id =
            started.root.children
                .single()
                .id
        val negated = applied(PredicateEditing.setNegated(started, id, negated = true))
        val compiled = assertInstanceOf(DraftCompilation.Compiled::class.java, PredicateCompiler.compile(negated))
        val all = assertInstanceOf(Predicate.All::class.java, compiled.predicate)
        assertInstanceOf(Predicate.Not::class.java, all.operands.single())
    }

    @Test
    fun `a stored definition round trips back into the editor`() {
        val predicate =
            Predicate.Any(
                listOf(
                    Predicate.Leaf(
                        Term(
                            field = QueryField.Rating.asWire(),
                            op = Operator.Gte.asWire(),
                            operand = Operand.Number(4),
                        ),
                    ),
                    Predicate.Not(
                        Predicate.Leaf(
                            Term(
                                field = QueryField.Hidden.asWire(),
                                op = Operator.Is.asWire(),
                                operand = Operand.BoolLiteral(true),
                            ),
                        ),
                    ),
                ),
            )
        val definition = definition(predicate)
        val draft = requireNotNull(PredicateCompiler.toDraft(definition))
        val recompiled = assertInstanceOf(DraftCompilation.Compiled::class.java, PredicateCompiler.compile(draft))
        assertEquals(predicate, recompiled.predicate)
    }

    @Test
    fun `a definition naming an unknown field refuses to open in the editor`() {
        val predicate =
            Predicate.All(
                listOf(
                    Predicate.Leaf(
                        Term(
                            field = Wire.Unknown("colour_temperature"),
                            op = Operator.Eq.asWire(),
                            operand = Operand.Number(1),
                        ),
                    ),
                ),
            )
        assertNull(PredicateCompiler.toDraft(definition(predicate)))
    }

    private fun definition(predicate: Predicate) =
        SmartAlbumDefinition(
            id = SmartAlbumId("018f"),
            predicateSchema = 1,
            displayName = Lww("Keepers", stamp),
            predicate = predicate,
        )

    private fun editedDraft(): PredicateDraft {
        val started = applied(PredicateEditing.addCondition(PredicateDraft.empty(), DraftNodeId(0), now))
        val id =
            started.root.children
                .single()
                .id
        return applied(PredicateEditing.setField(started, id, QueryField.Rating, now))
    }

    private fun applied(edit: DraftEdit): PredicateDraft = assertInstanceOf(DraftEdit.Applied::class.java, edit).draft
}
