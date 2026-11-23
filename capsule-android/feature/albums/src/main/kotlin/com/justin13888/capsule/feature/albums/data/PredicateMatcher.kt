package com.justin13888.capsule.feature.albums.data

import com.justin13888.capsule.core.model.AiTag
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.ModelSlot
import com.justin13888.capsule.core.model.Operand
import com.justin13888.capsule.core.model.Operator
import com.justin13888.capsule.core.model.Predicate
import com.justin13888.capsule.core.model.QueryField
import com.justin13888.capsule.core.model.QueryFieldClass
import com.justin13888.capsule.core.model.Term
import com.justin13888.capsule.core.model.UserTag
import com.justin13888.capsule.core.model.knownOrNull
import java.time.Instant

/**
 * Decides whether one asset matches a validated predicate.
 *
 * Membership is computed, never stored, so somewhere on the client has to be able to answer
 * this question without a round trip — and the builder needs the answer *before* a
 * definition has been saved, which rules out asking the repository to evaluate a definition
 * it does not yet hold. That is why this lives here rather than behind the search contract:
 * it is the preview, not the truth, and the truth still comes from `SearchRepository`.
 *
 * A term this matcher cannot decide evaluates to `false`, never `true`. Failing closed keeps
 * an unknown field, an operator outside its field's type class, or a stale AI slot from
 * silently widening the result set — a smart album that quietly matched everything would be
 * far more surprising than one that matched nothing.
 */
object PredicateMatcher {
    /** Whether [asset] matches [predicate], given the tag sets it carries. */
    fun matches(
        predicate: Predicate,
        asset: Asset,
        tags: AssetTags = AssetTags(),
    ): Boolean =
        when (predicate) {
            // An empty `all` matches everything and an empty `any` matches nothing: the
            // identity elements the grammar specifies, not a special case.
            is Predicate.All -> predicate.operands.all { matches(it, asset, tags) }
            is Predicate.Any -> predicate.operands.any { matches(it, asset, tags) }
            is Predicate.Not -> !matches(predicate.operand, asset, tags)
            is Predicate.Leaf -> matchesTerm(predicate.term, asset, tags)
        }

    private fun matchesTerm(
        term: Term,
        asset: Asset,
        tags: AssetTags,
    ): Boolean {
        val field = term.field.knownOrNull ?: return false
        val op = term.op.knownOrNull ?: return false
        if (field.fieldClass !in op.appliesTo) return false
        return when (field.fieldClass) {
            QueryFieldClass.Temporal -> temporal(op, term.operand, temporalValue(field, asset))
            QueryFieldClass.Numeric -> numeric(op, term.operand, numericValue(field, asset))
            QueryFieldClass.Enumerated -> enumerated(op, term.operand, enumeratedValue(field, asset))
            QueryFieldClass.TrinaryOrBool -> flag(field, term.operand, asset)
            QueryFieldClass.SetValued -> setValued(op, term.operand, setValues(field, asset, tags, term.modelSlot))
            QueryFieldClass.Presence -> presence(field, term.operand, asset)
        }
    }
}

private fun temporalValue(
    field: QueryField,
    asset: Asset,
): Instant = if (field == QueryField.ImportTimestamp) asset.importTimestamp else asset.captureTimestamp

private fun numericValue(
    field: QueryField,
    asset: Asset,
): Long =
    when (field) {
        QueryField.Rating -> asset.rating.toLong()
        QueryField.DimensionsWidth -> (asset.dimensions?.width ?: 0).toLong()
        else -> (asset.dimensions?.height ?: 0).toLong()
    }

/** The raw enum token the field reads off an asset, or `null` when it holds none. */
private fun enumeratedValue(
    field: QueryField,
    asset: Asset,
): String? =
    when (field) {
        QueryField.ContentType -> asset.contentType?.raw
        QueryField.MediaKind ->
            asset.contentType
                ?.knownOrNull
                ?.mediaKind
                ?.wire
        else -> asset.gps?.datum?.raw
    }

private fun temporal(
    op: Operator,
    operand: Operand,
    value: Instant,
): Boolean =
    when {
        op == Operator.Before && operand is Operand.TemporalPoint -> value.isBefore(operand.at)
        op == Operator.After && operand is Operand.TemporalPoint -> value.isAfter(operand.at)
        // Half-open [start, end), exactly as the grammar fixes it for a temporal range.
        op == Operator.InRange && operand is Operand.TemporalRange ->
            !value.isBefore(operand.start) && value.isBefore(operand.endExclusive)

        else -> false
    }

private fun numeric(
    op: Operator,
    operand: Operand,
    value: Long,
): Boolean =
    when {
        op == Operator.Eq && operand is Operand.Number -> value == operand.value
        op == Operator.Gte && operand is Operand.Number -> value >= operand.value
        op == Operator.Lte && operand is Operand.Number -> value <= operand.value
        // Inclusive [lo, hi] for a numeric range, unlike the temporal one.
        op == Operator.InRange && operand is Operand.NumberRange -> value in operand.lo..operand.hi
        else -> false
    }

private fun enumerated(
    op: Operator,
    operand: Operand,
    value: String?,
): Boolean =
    when {
        value == null -> false
        op == Operator.Eq && operand is Operand.EnumValue -> value == operand.raw
        op == Operator.In && operand is Operand.EnumSet -> value in operand.raw
        else -> false
    }

private fun flag(
    field: QueryField,
    operand: Operand,
    asset: Asset,
): Boolean =
    when {
        field == QueryField.Cull && operand is Operand.CullLiteral ->
            (asset.cull.knownOrNull ?: CullFlag.Neutral) == operand.flag

        field == QueryField.Hidden && operand is Operand.BoolLiteral -> asset.isHidden == operand.value
        else -> false
    }

/**
 * The set a set-valued field reads off an asset.
 *
 * `tags_ai` and `people_cluster` are filtered to the term's own model slot: comparing across
 * model versions is meaningless because the vector spaces differ, so a term over a slot the
 * asset does not carry contributes nothing rather than matching loosely.
 */
private fun setValues(
    field: QueryField,
    asset: Asset,
    tags: AssetTags,
    slot: ModelSlot?,
): Set<String> =
    when (field) {
        QueryField.TagsUser -> tags.user.mapTo(mutableSetOf(), UserTag::tag)
        QueryField.TagsAi ->
            tags.freshAi
                .filter { slot == null || it.slot == slot }
                .mapTo(mutableSetOf(), AiTag::tag)

        QueryField.StackType -> setOfNotNull(asset.stack?.stackType?.raw)
        QueryField.AlbumId -> setOfNotNull(asset.albumId?.value)
        // Face-cluster membership is not projected onto the asset, so a term over it has
        // nothing to compare and fails closed rather than matching everything.
        else -> emptySet()
    }

private fun setValued(
    op: Operator,
    operand: Operand,
    values: Set<String>,
): Boolean {
    val members = (operand as? Operand.StringSet)?.members ?: return false
    return when (op) {
        Operator.Contains, Operator.ContainsAll -> members.isNotEmpty() && values.containsAll(members)
        Operator.ContainsAny -> members.any { it in values }
        else -> false
    }
}

private fun presence(
    field: QueryField,
    operand: Operand,
    asset: Asset,
): Boolean {
    val wanted = (operand as? Operand.Presence)?.mustExist ?: return false
    val present = if (field == QueryField.Gps) asset.gps != null else asset.camera != null
    return present == wanted
}
