package com.justin13888.capsule.core.model

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Nested
import org.junit.jupiter.api.Test
import java.time.Instant

/**
 * The closed predicate grammar and every rejection the design names for it.
 *
 * The grammar is a normative contract, not an internal convenience: a definition syncs to
 * every one of the owner's devices and must evaluate identically on all of them, so a client
 * that tolerated a term it did not understand would compute a different membership than its
 * peers. Each case below is one of the design's stated rejections.
 */
class SmartAlbumValidatorTest {
    @Nested
    inner class WellFormedDefinitions {
        @Test
        fun `a simple conjunction validates`() {
            assertInstanceOf(
                SmartAlbumValidation.Evaluable::class.java,
                validate(
                    Predicate.All(
                        listOf(
                            leaf(QueryField.Rating, Operator.Gte, Operand.Number(4)),
                            leaf(QueryField.MediaKind, Operator.Eq, Operand.EnumValue("image")),
                        ),
                    ),
                ),
            )
        }

        @Test
        fun `an empty conjunction is legal and means everything`() {
            // Empty All matches every asset and empty Any matches none; both are well formed,
            // so refusing them here would reject a definition the grammar explicitly allows.
            assertInstanceOf(SmartAlbumValidation.Evaluable::class.java, validate(Predicate.All(emptyList())))
            assertInstanceOf(SmartAlbumValidation.Evaluable::class.java, validate(Predicate.Any(emptyList())))
        }

        @Test
        fun `an AI term naming its model slot validates`() {
            assertInstanceOf(
                SmartAlbumValidation.Evaluable::class.java,
                validate(
                    Predicate.Leaf(
                        Term(
                            field = QueryField.TagsAi.asWire(),
                            op = Operator.Contains.asWire(),
                            operand = Operand.StringSet(listOf("beach")),
                            modelSlot = ModelSlot(ModelId("mobileclip-b"), ModelVersion("1")),
                        ),
                    ),
                ),
            )
        }
    }

    @Nested
    inner class StructuralRejections {
        @Test
        fun `a term naming an unknown field is refused, not ignored`() {
            val result =
                validate(
                    Predicate.Leaf(
                        Term(
                            field = Wire.Unknown("moon_phase"),
                            op = Operator.Eq.asWire(),
                            operand = Operand.Number(1),
                        ),
                    ),
                )
            assertEquals(listOf(PredicateViolation.UnknownField("moon_phase")), violations(result))
        }

        @Test
        fun `a term naming an unknown operator is refused`() {
            val result =
                validate(
                    Predicate.Leaf(
                        Term(
                            field = QueryField.Rating.asWire(),
                            op = Wire.Unknown("approximately"),
                            operand = Operand.Number(3),
                        ),
                    ),
                )
            assertEquals(listOf(PredicateViolation.UnknownOperator("approximately")), violations(result))
        }

        @Test
        fun `an operator outside the field's type class is refused`() {
            // `contains` is a set operator; `rating` is numeric. Real operator, wrong class.
            val result = validate(leaf(QueryField.Rating, Operator.Contains, Operand.StringSet(listOf("x"))))
            assertEquals(
                listOf(PredicateViolation.OperatorNotValidForField(QueryField.Rating, Operator.Contains)),
                violations(result),
            )
        }

        @Test
        fun `a temporal operator on a set field is refused`() {
            val result =
                validate(
                    leaf(QueryField.TagsUser, Operator.Before, Operand.TemporalPoint(Instant.EPOCH)),
                )
            assertEquals(
                listOf(PredicateViolation.OperatorNotValidForField(QueryField.TagsUser, Operator.Before)),
                violations(result),
            )
        }

        @Test
        fun `an operand mistyped for its field and operator is refused`() {
            val result = validate(leaf(QueryField.CaptureTimestamp, Operator.Before, Operand.Number(7)))
            val violation = violations(result).single()
            assertInstanceOf(PredicateViolation.OperandTypeMismatch::class.java, violation)
        }

        @Test
        fun `a temporal in_range demands a range, not a point`() {
            val result =
                validate(
                    leaf(QueryField.CaptureTimestamp, Operator.InRange, Operand.TemporalPoint(Instant.EPOCH)),
                )
            assertInstanceOf(PredicateViolation.OperandTypeMismatch::class.java, violations(result).single())
        }

        @Test
        fun `a numeric in_range demands a numeric range, not a temporal one`() {
            val result =
                validate(
                    leaf(
                        QueryField.Rating,
                        Operator.InRange,
                        Operand.TemporalRange(Instant.EPOCH, Instant.EPOCH.plusSeconds(1)),
                    ),
                )
            assertInstanceOf(PredicateViolation.OperandTypeMismatch::class.java, violations(result).single())
        }

        @Test
        fun `the is operator takes the queried field's own literal`() {
            // `cull` is trinary and `hidden` is boolean; swapping their literals is exactly
            // the "operand mistyped for its (field, op)" case.
            val wrongForCull = validate(leaf(QueryField.Cull, Operator.Is, Operand.BoolLiteral(true)))
            assertInstanceOf(PredicateViolation.OperandTypeMismatch::class.java, violations(wrongForCull).single())

            val wrongForHidden = validate(leaf(QueryField.Hidden, Operator.Is, Operand.CullLiteral(CullFlag.Pick)))
            assertInstanceOf(PredicateViolation.OperandTypeMismatch::class.java, violations(wrongForHidden).single())

            assertInstanceOf(
                SmartAlbumValidation.Evaluable::class.java,
                validate(leaf(QueryField.Cull, Operator.Is, Operand.CullLiteral(CullFlag.Reject))),
            )
        }

        @Test
        fun `an enum operand outside the field's own closed enum is refused`() {
            val result = validate(leaf(QueryField.ContentType, Operator.Eq, Operand.EnumValue("image/jpg")))
            assertEquals(
                listOf(PredicateViolation.OperandNotInClosedEnum(QueryField.ContentType, "image/jpg")),
                violations(result),
            )
        }

        @Test
        fun `every offending member of an enum set is reported`() {
            val result =
                validate(
                    leaf(QueryField.GpsDatum, Operator.In, Operand.EnumSet(listOf("wgs84", "bd09", "nad83"))),
                )
            assertEquals(
                listOf(
                    PredicateViolation.OperandNotInClosedEnum(QueryField.GpsDatum, "bd09"),
                    PredicateViolation.OperandNotInClosedEnum(QueryField.GpsDatum, "nad83"),
                ),
                violations(result),
            )
        }

        @Test
        fun `an AI term without its model slot is refused`() {
            // Evaluating it would mean comparing across model versions, which is forbidden
            // because the vector spaces differ and the comparison is meaningless.
            val result = validate(leaf(QueryField.TagsAi, Operator.Contains, Operand.StringSet(listOf("dog"))))
            assertEquals(listOf(PredicateViolation.MissingModelSlot(QueryField.TagsAi)), violations(result))
        }

        @Test
        fun `a people cluster term without its model slot is refused`() {
            val result =
                validate(
                    leaf(QueryField.PeopleCluster, Operator.ContainsAny, Operand.StringSet(listOf("c1"))),
                )
            assertEquals(listOf(PredicateViolation.MissingModelSlot(QueryField.PeopleCluster)), violations(result))
        }

        @Test
        fun `an inverted range is refused`() {
            val temporal =
                validate(
                    leaf(
                        QueryField.ImportTimestamp,
                        Operator.InRange,
                        Operand.TemporalRange(Instant.ofEpochSecond(100), Instant.ofEpochSecond(100)),
                    ),
                )
            assertEquals(listOf(PredicateViolation.RangeInverted(QueryField.ImportTimestamp)), violations(temporal))

            val numeric = validate(leaf(QueryField.Rating, Operator.InRange, Operand.NumberRange(5, 1)))
            assertEquals(listOf(PredicateViolation.RangeInverted(QueryField.Rating)), violations(numeric))
        }

        @Test
        fun `a stack_type set operand is bounds-checked but not enum-checked`() {
            // The grammar table puts `stack_type` in the set class, whose operand it defines
            // as "a string / id set" — unlike the enum class, which it defines as drawn from
            // the field's own closed enum. This validator follows the table rather than
            // tightening it unilaterally.
            assertInstanceOf(
                SmartAlbumValidation.Evaluable::class.java,
                validate(leaf(QueryField.StackType, Operator.Contains, Operand.StringSet(listOf("future_type")))),
            )
        }
    }

    @Nested
    inner class Bounds {
        @Test
        fun `a tree at the depth limit is accepted`() {
            assertInstanceOf(
                SmartAlbumValidation.Evaluable::class.java,
                validate(nest(Predicate.MAX_DEPTH)),
            )
        }

        @Test
        fun `a tree past the depth limit is refused`() {
            val result = validate(nest(Predicate.MAX_DEPTH + 1))
            assertEquals(
                listOf(PredicateViolation.DepthExceeded(Predicate.MAX_DEPTH + 1)),
                violations(result),
            )
        }

        @Test
        fun `depth counts a leaf as one`() {
            assertEquals(1, SmartAlbumValidator.depthOf(anyLeaf()))
            assertEquals(2, SmartAlbumValidator.depthOf(Predicate.All(listOf(anyLeaf()))))
            assertEquals(2, SmartAlbumValidator.depthOf(Predicate.Not(anyLeaf())))
        }

        @Test
        fun `an empty branch is depth one, so it cannot inflate the bound`() {
            assertEquals(1, SmartAlbumValidator.depthOf(Predicate.All(emptyList())))
            assertEquals(1, SmartAlbumValidator.depthOf(Predicate.Any(emptyList())))
        }

        @Test
        fun `a tree at the term limit is accepted`() {
            assertInstanceOf(
                SmartAlbumValidation.Evaluable::class.java,
                validate(Predicate.Any(List(Predicate.MAX_TERMS) { anyLeaf() })),
            )
        }

        @Test
        fun `a tree past the term limit is refused`() {
            val over = Predicate.MAX_TERMS + 1
            val result = validate(Predicate.Any(List(over) { anyLeaf() }))
            assertEquals(listOf(PredicateViolation.TermCountExceeded(over)), violations(result))
        }

        @Test
        fun `terms are counted across the whole tree, not per branch`() {
            val half = Predicate.MAX_TERMS / 2 + 1
            val tree =
                Predicate.All(
                    listOf(
                        Predicate.Any(List(half) { anyLeaf() }),
                        Predicate.Any(List(half) { anyLeaf() }),
                    ),
                )
            assertEquals(half * 2, SmartAlbumValidator.leaves(tree).size)
            assertEquals(listOf(PredicateViolation.TermCountExceeded(half * 2)), violations(validate(tree)))
        }

        @Test
        fun `a set operand at the member limit is accepted`() {
            assertInstanceOf(
                SmartAlbumValidation.Evaluable::class.java,
                validate(
                    leaf(
                        QueryField.TagsUser,
                        Operator.ContainsAny,
                        Operand.StringSet(List(Predicate.MAX_SET_MEMBERS) { "tag$it" }),
                    ),
                ),
            )
        }

        @Test
        fun `a set operand past the member limit is refused`() {
            val over = Predicate.MAX_SET_MEMBERS + 1
            val result =
                validate(
                    leaf(QueryField.TagsUser, Operator.ContainsAny, Operand.StringSet(List(over) { "tag$it" })),
                )
            assertEquals(listOf(PredicateViolation.SetOperandTooLarge(over)), violations(result))
        }

        @Test
        fun `a set member past the byte limit is refused`() {
            val long = "x".repeat(Predicate.MAX_SET_MEMBER_BYTES + 1)
            val result = validate(leaf(QueryField.AlbumId, Operator.Contains, Operand.StringSet(listOf(long))))
            assertEquals(
                listOf(PredicateViolation.SetMemberTooLong(long, Predicate.MAX_SET_MEMBER_BYTES + 1)),
                violations(result),
            )
        }

        @Test
        fun `the member limit is measured in bytes, not characters`() {
            // A multi-byte character would slip past a length check and blow the bound the
            // wire format actually enforces.
            val member = "é".repeat(Predicate.MAX_SET_MEMBER_BYTES / 2 + 1)
            assertTrue(member.length <= Predicate.MAX_SET_MEMBER_BYTES)
            val result = validate(leaf(QueryField.AlbumId, Operator.Contains, Operand.StringSet(listOf(member))))
            assertInstanceOf(PredicateViolation.SetMemberTooLong::class.java, violations(result).single())
        }

        @Test
        fun `a display name past the byte limit is refused`() {
            val name = "n".repeat(SmartAlbumDefinition.MAX_DISPLAY_NAME_BYTES + 1)
            val result = SmartAlbumValidator.validate(definition(anyLeaf(), displayName = name))
            assertEquals(
                listOf(PredicateViolation.DisplayNameTooLong(SmartAlbumDefinition.MAX_DISPLAY_NAME_BYTES + 1)),
                violations(result),
            )
        }
    }

    @Nested
    inner class ForwardSchema {
        @Test
        fun `a later grammar is preserved rather than evaluated`() {
            val result =
                SmartAlbumValidator.validate(
                    definition(anyLeaf(), schema = SmartAlbumDefinition.MAX_KNOWN_PREDICATE_SCHEMA + 1),
                )
            assertEquals(
                SmartAlbumValidation.PreservedUnevaluated(SmartAlbumDefinition.MAX_KNOWN_PREDICATE_SCHEMA + 1),
                result,
            )
        }

        @Test
        fun `a later grammar is not reported as malformed`() {
            // Its terms may legitimately name fields this build has never heard of. Checking
            // them against today's closed sets would report violations that are not
            // violations, and the definition would be "repaired" out of existence.
            val result =
                SmartAlbumValidator.validate(
                    definition(
                        Predicate.Leaf(
                            Term(
                                field = Wire.Unknown("shutter_speed"),
                                op = Wire.Unknown("faster_than"),
                                operand = Operand.Number(500),
                            ),
                        ),
                        schema = SmartAlbumDefinition.MAX_KNOWN_PREDICATE_SCHEMA + 3,
                    ),
                )
            assertInstanceOf(SmartAlbumValidation.PreservedUnevaluated::class.java, result)
        }

        @Test
        fun `the current grammar is evaluated`() {
            assertInstanceOf(
                SmartAlbumValidation.Evaluable::class.java,
                SmartAlbumValidator.validate(
                    definition(anyLeaf(), schema = SmartAlbumDefinition.MAX_KNOWN_PREDICATE_SCHEMA),
                ),
            )
        }
    }

    @Test
    fun `every violation is reported, not just the first`() {
        // A definition arriving over sync cannot be fixed interactively, so the user needs
        // the whole list to understand what the other device wrote.
        val result =
            validate(
                Predicate.All(
                    listOf(
                        Predicate.Leaf(Term(Wire.Unknown("moon_phase"), Operator.Eq.asWire(), Operand.Number(1))),
                        leaf(QueryField.ContentType, Operator.Eq, Operand.EnumValue("image/jpg")),
                        leaf(QueryField.TagsAi, Operator.Contains, Operand.StringSet(listOf("dog"))),
                    ),
                ),
            )
        assertEquals(3, violations(result).size, "expected one violation per offending term")
    }

    private fun leaf(
        field: QueryField,
        op: Operator,
        operand: Operand,
    ) = Predicate.Leaf(Term(field.asWire(), op.asWire(), operand))

    private fun anyLeaf() = leaf(QueryField.Rating, Operator.Gte, Operand.Number(1))

    private fun nest(depth: Int): Predicate {
        var node: Predicate = anyLeaf()
        repeat(depth - 1) { node = Predicate.Not(node) }
        return node
    }

    private fun definition(
        predicate: Predicate,
        schema: Int = SmartAlbumDefinition.MAX_KNOWN_PREDICATE_SCHEMA,
        displayName: String = "Favourites",
    ) = SmartAlbumDefinition(
        id = SmartAlbumId("sa-1"),
        predicateSchema = schema,
        displayName = Lww(displayName, LwwStamp(Instant.EPOCH, DeviceId("d1"))),
        predicate = predicate,
    )

    private fun validate(predicate: Predicate) = SmartAlbumValidator.validate(definition(predicate))

    private fun violations(result: SmartAlbumValidation): List<PredicateViolation> =
        assertInstanceOf(SmartAlbumValidation.Rejected::class.java, result).violations
}
