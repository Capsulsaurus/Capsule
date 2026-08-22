package com.justin13888.capsule.core.mock.repo

import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.Operand
import com.justin13888.capsule.core.model.Operator
import com.justin13888.capsule.core.model.Predicate
import com.justin13888.capsule.core.model.QueryField
import com.justin13888.capsule.core.model.QueryFieldClass
import com.justin13888.capsule.core.model.Term
import com.justin13888.capsule.core.model.UserTag
import com.justin13888.capsule.core.model.knownOrNull

/**
 * Evaluates a validated smart-album predicate against one asset.
 *
 * Written out rather than approximated, because a smart album whose membership is "roughly
 * right" is worse than one that is empty: the whole feature is a promise that the same
 * definition produces the same set on every device, and a mock that quietly matched everything
 * would make a broken predicate editor look like it worked.
 *
 * A term this evaluator cannot decide — an unknown field, an operator outside the field's type
 * class, an AI-derived field whose model slot is stale — evaluates to `false` rather than to
 * `true`. Failing closed keeps an undecidable term from silently widening the result set.
 */
object PredicateEvaluator {
    /** Whether [asset] matches [predicate]. */
    fun matches(
        predicate: Predicate,
        asset: Asset,
        tags: AssetTags,
    ): Boolean =
        when (predicate) {
            // An empty `all` matches every asset and an empty `any` matches none, exactly as
            // the grammar specifies — the identity elements of the two operators.
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
        return when (field.fieldClass) {
            QueryFieldClass.Temporal, QueryFieldClass.Numeric -> scalar(field, op, term.operand, asset)
            QueryFieldClass.Enumerated -> enumerated(op, term.operand, enumeratedValue(field, asset))
            QueryFieldClass.TrinaryOrBool -> flag(field, op, term.operand, asset)
            QueryFieldClass.SetValued -> setValued(op, term.operand, setValues(field, asset, tags))
            QueryFieldClass.Presence -> presence(field, term.operand, asset)
        }
    }
}

/** The comparable number or instant [field] reads off an asset. */
private fun scalar(
    field: QueryField,
    op: Operator,
    operand: Operand,
    asset: Asset,
): Boolean =
    when (field) {
        QueryField.CaptureTimestamp -> temporal(op, operand, asset.captureTimestamp)
        QueryField.ImportTimestamp -> temporal(op, operand, asset.importTimestamp)
        QueryField.Rating -> numeric(op, operand, asset.rating.toLong())
        QueryField.DimensionsWidth -> numeric(op, operand, (asset.dimensions?.width ?: 0).toLong())
        else -> numeric(op, operand, (asset.dimensions?.height ?: 0).toLong())
    }

/** The raw enum token [field] reads off an asset, or `null` when it has none. */
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

/** The set [field] reads off an asset. */
private fun setValues(
    field: QueryField,
    asset: Asset,
    tags: AssetTags,
): Set<String> =
    when (field) {
        QueryField.TagsUser -> tags.user.map(UserTag::tag).toSet()
        // Stale AI values are excluded from queries rather than compared across model
        // versions: the vector spaces differ, so the comparison would be meaningless.
        QueryField.TagsAi -> tags.freshAi.map { it.tag }.toSet()
        QueryField.StackType -> setOfNotNull(asset.stack?.stackType?.raw)
        QueryField.AlbumId -> setOfNotNull(asset.albumId?.value)
        // Face membership needs the embeddings behind a model slot, which the mock does
        // not carry per asset; failing closed keeps the term from widening the result.
        else -> emptySet()
    }

/** The trinary or boolean flag [field] reads off an asset. */
private fun flag(
    field: QueryField,
    op: Operator,
    operand: Operand,
    asset: Asset,
): Boolean =
    if (field == QueryField.Cull) {
        cull(op, operand, asset)
    } else {
        op == Operator.Is && (operand as? Operand.BoolLiteral)?.value == asset.isHidden
    }

/** Whether the field [field] names is present on an asset at all. */
private fun presence(
    field: QueryField,
    operand: Operand,
    asset: Asset,
): Boolean {
    val mustExist = (operand as? Operand.Presence)?.mustExist ?: return false
    val present = if (field == QueryField.Gps) asset.gps != null else asset.camera != null
    return mustExist == present
}

private fun temporal(
    op: Operator,
    operand: Operand,
    value: java.time.Instant,
): Boolean =
    when {
        op == Operator.Before && operand is Operand.TemporalPoint -> value.isBefore(operand.at)
        op == Operator.After && operand is Operand.TemporalPoint -> value.isAfter(operand.at)
        // Half-open `[start, endExclusive)` for temporal ranges, as the grammar fixes.
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
        // Inclusive `[lo, hi]` for numeric ranges — deliberately not the temporal rule.
        op == Operator.InRange && operand is Operand.NumberRange -> value in operand.lo..operand.hi
        else -> false
    }

private fun enumerated(
    op: Operator,
    operand: Operand,
    raw: String?,
): Boolean =
    when {
        raw == null -> false
        op == Operator.Eq && operand is Operand.EnumValue -> raw == operand.raw
        op == Operator.In && operand is Operand.EnumSet -> raw in operand.raw
        else -> false
    }

private fun setValued(
    op: Operator,
    operand: Operand,
    values: Set<String>,
): Boolean {
    val members = (operand as? Operand.StringSet)?.members ?: return false
    return when (op) {
        Operator.Contains, Operator.ContainsAll -> values.containsAll(members)
        Operator.ContainsAny -> members.any { it in values }
        else -> false
    }
}

private fun cull(
    op: Operator,
    operand: Operand,
    asset: Asset,
): Boolean {
    if (op != Operator.Is) return false
    val wanted = (operand as? Operand.CullLiteral)?.flag ?: return false
    // A never-flagged asset is wire-absent and reads as neutral: never reviewed and
    // explicitly marked neutral are the same statement, because neither is a decision.
    return (asset.cull.knownOrNull ?: CullFlag.Neutral) == wanted
}
