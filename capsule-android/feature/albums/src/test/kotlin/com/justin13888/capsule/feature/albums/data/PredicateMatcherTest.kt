package com.justin13888.capsule.feature.albums.data

import com.justin13888.capsule.core.model.AddId
import com.justin13888.capsule.core.model.AiTag
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.GpsFix
import com.justin13888.capsule.core.model.GpsSource
import com.justin13888.capsule.core.model.ModelId
import com.justin13888.capsule.core.model.ModelSlot
import com.justin13888.capsule.core.model.ModelVersion
import com.justin13888.capsule.core.model.Operand
import com.justin13888.capsule.core.model.Operator
import com.justin13888.capsule.core.model.Predicate
import com.justin13888.capsule.core.model.QueryField
import com.justin13888.capsule.core.model.Term
import com.justin13888.capsule.core.model.UserTag
import com.justin13888.capsule.core.model.Wire
import com.justin13888.capsule.core.model.asWire
import com.justin13888.capsule.feature.albums.AlbumsFixtures
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.time.Instant

/**
 * The preview evaluator, held to the grammar rather than to intuition.
 *
 * Two asymmetries the design fixes are checked directly, because both are the sort of thing a
 * plausible implementation gets backwards: a temporal range is half-open `[start, end)` while
 * a numeric one is inclusive `[lo, hi]`, and an undecidable term fails **closed**. A matcher
 * that failed open would quietly widen every rule containing a term it did not understand.
 */
class PredicateMatcherTest {
    private val capturedAt: Instant = Instant.parse("2026-06-01T12:00:00Z")
    private val asset =
        AlbumsFixtures
            .asset(id = "a", capturedAt = capturedAt, width = 4000, height = 3000, rating = 4)
            .copy(
                albumId = AlbumId("album-1"),
                cull = CullFlag.Pick.asWire(),
                gps = GpsFix(49.0, -123.0, GpsSource.Exif.asWire()),
            )

    private fun leaf(
        field: QueryField,
        op: Operator,
        operand: Operand,
        slot: ModelSlot? = null,
    ) = Predicate.Leaf(Term(field.asWire(), op.asWire(), operand, slot))

    private fun matches(
        predicate: Predicate,
        tags: AssetTags = AssetTags(),
    ) = PredicateMatcher.matches(predicate, asset, tags)

    @Test
    fun `an empty all matches everything and an empty any matches nothing`() {
        assertTrue(matches(Predicate.All(emptyList())))
        assertFalse(matches(Predicate.Any(emptyList())))
    }

    @Test
    fun `a temporal range is half-open`() {
        val startsHere =
            leaf(
                QueryField.CaptureTimestamp,
                Operator.InRange,
                Operand.TemporalRange(capturedAt, capturedAt.plusSeconds(1)),
            )
        val endsHere =
            leaf(
                QueryField.CaptureTimestamp,
                Operator.InRange,
                Operand.TemporalRange(capturedAt.minusSeconds(1), capturedAt),
            )
        assertTrue(matches(startsHere))
        assertFalse(matches(endsHere))
    }

    @Test
    fun `a numeric range is inclusive at both ends`() {
        assertTrue(matches(leaf(QueryField.Rating, Operator.InRange, Operand.NumberRange(4, 4))))
    }

    @Test
    fun `before and after are strict`() {
        assertFalse(matches(leaf(QueryField.CaptureTimestamp, Operator.Before, Operand.TemporalPoint(capturedAt))))
        assertFalse(matches(leaf(QueryField.CaptureTimestamp, Operator.After, Operand.TemporalPoint(capturedAt))))
    }

    @Test
    fun `an enumerated field compares against its own closed enum`() {
        assertTrue(matches(leaf(QueryField.MediaKind, Operator.Eq, Operand.EnumValue("image"))))
        assertTrue(matches(leaf(QueryField.MediaKind, Operator.In, Operand.EnumSet(listOf("video", "image")))))
        assertFalse(matches(leaf(QueryField.MediaKind, Operator.Eq, Operand.EnumValue("video"))))
    }

    @Test
    fun `the trinary cull flag and the boolean hidden flag each take their own literal`() {
        assertTrue(matches(leaf(QueryField.Cull, Operator.Is, Operand.CullLiteral(CullFlag.Pick))))
        assertFalse(matches(leaf(QueryField.Cull, Operator.Is, Operand.CullLiteral(CullFlag.Reject))))
        assertFalse(matches(leaf(QueryField.Hidden, Operator.Is, Operand.BoolLiteral(true))))
    }

    @Test
    fun `the contains family reads the field's own set`() {
        val tags =
            AssetTags(
                user =
                    listOf(
                        UserTag("beach", AddId(DeviceId("d"), 1)),
                        UserTag("sunset", AddId(DeviceId("d"), 2)),
                    ),
            )
        assertTrue(matches(leaf(QueryField.TagsUser, Operator.Contains, Operand.StringSet(listOf("beach"))), tags))
        assertTrue(
            matches(
                leaf(QueryField.TagsUser, Operator.ContainsAll, Operand.StringSet(listOf("beach", "sunset"))),
                tags,
            ),
        )
        assertTrue(
            matches(leaf(QueryField.TagsUser, Operator.ContainsAny, Operand.StringSet(listOf("rain", "beach"))), tags),
        )
        assertFalse(
            matches(
                leaf(QueryField.TagsUser, Operator.ContainsAll, Operand.StringSet(listOf("beach", "rain"))),
                tags,
            ),
        )
    }

    @Test
    fun `an AI term only sees values from the slot it names`() {
        val slot = ModelSlot(ModelId("mobileclip-b"), ModelVersion("1"))
        val other = ModelSlot(ModelId("mobileclip-b"), ModelVersion("2"))
        val tags = AssetTags(ai = listOf(AiTag("dog", AddId(DeviceId("d"), 3), slot)))
        assertTrue(matches(leaf(QueryField.TagsAi, Operator.Contains, Operand.StringSet(listOf("dog")), slot), tags))
        assertFalse(matches(leaf(QueryField.TagsAi, Operator.Contains, Operand.StringSet(listOf("dog")), other), tags))
    }

    @Test
    fun `a stale AI value is excluded rather than compared across versions`() {
        val slot = ModelSlot(ModelId("mobileclip-b"), ModelVersion("1"))
        val tags = AssetTags(ai = listOf(AiTag("dog", AddId(DeviceId("d"), 3), slot, isStale = true)))
        assertFalse(matches(leaf(QueryField.TagsAi, Operator.Contains, Operand.StringSet(listOf("dog")), slot), tags))
    }

    @Test
    fun `presence reads whether the field is stored at all`() {
        assertTrue(matches(leaf(QueryField.Gps, Operator.Exists, Operand.Presence(mustExist = true))))
        assertTrue(matches(leaf(QueryField.CameraId, Operator.Exists, Operand.Presence(mustExist = false))))
    }

    @Test
    fun `an unknown field fails closed rather than widening the result`() {
        val unknown =
            Predicate.Leaf(Term(Wire.Unknown("colour_temperature"), Operator.Eq.asWire(), Operand.Number(1)))
        assertFalse(matches(unknown))
        assertTrue(matches(Predicate.Not(unknown)))
    }

    @Test
    fun `an operator outside the field's type class fails closed`() {
        assertFalse(matches(leaf(QueryField.Rating, Operator.Contains, Operand.StringSet(listOf("4")))))
    }
}
