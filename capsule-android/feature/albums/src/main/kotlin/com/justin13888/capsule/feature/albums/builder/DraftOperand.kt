package com.justin13888.capsule.feature.albums.builder

import com.justin13888.capsule.core.model.ContentType
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.GpsDatum
import com.justin13888.capsule.core.model.MediaKind
import com.justin13888.capsule.core.model.Operand
import com.justin13888.capsule.core.model.Operator
import com.justin13888.capsule.core.model.Predicate
import com.justin13888.capsule.core.model.QueryField
import com.justin13888.capsule.core.model.QueryFieldClass
import com.justin13888.capsule.core.model.WireEnum
import java.time.DateTimeException
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneOffset

/**
 * The operand of one condition while it is still being typed.
 *
 * Distinct from [Operand], which is the finished literal, because an editor has a state the
 * domain type deliberately cannot express: a date field holding `2026-0`. Keeping the
 * half-typed text here rather than forcing it into the domain type is what lets the builder
 * refuse a malformed rule *structurally* — there is simply no way to build a
 * [com.justin13888.capsule.core.model.Term] out of text that does not parse — instead of
 * coercing it to something the user did not write.
 */
sealed interface DraftOperand {
    /** The finished literal, or `null` while the entry is incomplete. */
    fun compile(): Operand?

    /** A single instant, for `before` / `after`. */
    data class TemporalPoint(
        val text: String,
    ) : DraftOperand {
        override fun compile(): Operand? = parseInstant(text)?.let(Operand::TemporalPoint)
    }

    /** A half-open `[start, end)` window, for a temporal `in_range`. */
    data class TemporalRange(
        val fromText: String,
        val toText: String,
    ) : DraftOperand {
        override fun compile(): Operand? {
            val start = parseInstant(fromText) ?: return null
            val end = parseInstant(toText) ?: return null
            return Operand.TemporalRange(start, end)
        }
    }

    /** One value of the field's own closed enum, for `eq`. */
    data class EnumValue(
        val raw: String,
    ) : DraftOperand {
        override fun compile(): Operand? = raw.takeIf(String::isNotBlank)?.let(Operand::EnumValue)
    }

    /** A set of values of the field's own closed enum, for `in`. */
    data class EnumSet(
        val raw: List<String>,
    ) : DraftOperand {
        override fun compile(): Operand = Operand.EnumSet(raw)
    }

    /** An integer, for numeric `eq` / `gte` / `lte`. */
    data class Number(
        val text: String,
    ) : DraftOperand {
        override fun compile(): Operand? = text.trim().toLongOrNull()?.let(Operand::Number)
    }

    /** An inclusive `[lo, hi]` pair, for a numeric `in_range`. */
    data class NumberRange(
        val fromText: String,
        val toText: String,
    ) : DraftOperand {
        override fun compile(): Operand? {
            val lo = fromText.trim().toLongOrNull() ?: return null
            val hi = toText.trim().toLongOrNull() ?: return null
            return Operand.NumberRange(lo, hi)
        }
    }

    /** A culling flag, for `is` on `cull`. */
    data class Cull(
        val flag: CullFlag,
    ) : DraftOperand {
        override fun compile(): Operand = Operand.CullLiteral(flag)
    }

    /** A boolean, for `is` on `hidden`. */
    data class Bool(
        val value: Boolean,
    ) : DraftOperand {
        override fun compile(): Operand = Operand.BoolLiteral(value)
    }

    /** A bounded set of strings or ids, for the `contains` family. */
    data class Strings(
        val members: List<String>,
    ) : DraftOperand {
        override fun compile(): Operand = Operand.StringSet(members)
    }

    /** Whether the field must be present, for `exists`. */
    data class Presence(
        val mustExist: Boolean,
    ) : DraftOperand {
        override fun compile(): Operand = Operand.Presence(mustExist)
    }

    companion object {
        /**
         * The empty operand a freshly chosen `(field, op)` pair starts with.
         *
         * Derived from the pair rather than carried over from whatever the condition held
         * before, because an operand's shape is *fixed* by its pair: keeping a
         * [TemporalPoint] after the field moved to `rating` would produce a term the
         * validator refuses, which is a mistake the editor should not be able to make.
         */
        fun defaultFor(
            field: QueryField,
            op: Operator,
            now: Instant,
        ): DraftOperand {
            val today = formatDate(now)
            return when (op) {
                Operator.Before, Operator.After -> TemporalPoint(today)
                Operator.InRange ->
                    if (field.fieldClass == QueryFieldClass.Temporal) {
                        TemporalRange(today, today)
                    } else {
                        NumberRange("0", "0")
                    }

                Operator.Eq ->
                    if (field.fieldClass == QueryFieldClass.Enumerated) {
                        EnumValue(closedValues(field).firstOrNull().orEmpty())
                    } else {
                        Number("0")
                    }

                Operator.In -> EnumSet(emptyList())
                Operator.Gte, Operator.Lte -> Number("0")
                Operator.Is -> if (field == QueryField.Cull) Cull(CullFlag.Pick) else Bool(true)
                Operator.Contains, Operator.ContainsAny, Operator.ContainsAll -> Strings(emptyList())
                Operator.Exists -> Presence(true)
            }
        }

        /**
         * The closed value set an enumerated field draws from, as wire tokens.
         *
         * Offered to the picker so an out-of-set value is unreachable by construction rather
         * than caught after the fact — the grammar calls an operand outside a field's own
         * enum a structural rejection, and the cheapest way to honour that is never to show
         * one.
         */
        fun closedValues(field: QueryField): List<String> =
            when (field) {
                QueryField.ContentType -> ContentType.entries.map(WireEnum::wire)
                QueryField.MediaKind -> MediaKind.entries.map(WireEnum::wire)
                QueryField.GpsDatum -> GpsDatum.entries.map(WireEnum::wire)
                else -> emptyList()
            }

        /** How many members a set operand may hold before the grammar refuses it. */
        const val MAX_SET_MEMBERS = Predicate.MAX_SET_MEMBERS

        /** The encoded length limit on one set member. */
        const val MAX_SET_MEMBER_BYTES = Predicate.MAX_SET_MEMBER_BYTES

        /**
         * Reads a date or an instant out of what the user typed.
         *
         * Accepts a bare `YYYY-MM-DD`, taken at the start of that day in UTC, as well as a
         * full RFC 3339 instant. Anything else returns `null`, which the editor surfaces as
         * an incomplete condition rather than guessing at a date the user did not mean.
         */
        fun parseInstant(text: String): Instant? {
            val trimmed = text.trim()
            if (trimmed.isEmpty()) return null
            return try {
                Instant.parse(trimmed)
            } catch (_: DateTimeException) {
                runCatching { LocalDate.parse(trimmed).atStartOfDay(ZoneOffset.UTC).toInstant() }.getOrNull()
            }
        }

        /** The `YYYY-MM-DD` spelling of an instant, which is what the date fields show. */
        fun formatDate(at: Instant): String = LocalDate.ofInstant(at, ZoneOffset.UTC).toString()
    }
}
