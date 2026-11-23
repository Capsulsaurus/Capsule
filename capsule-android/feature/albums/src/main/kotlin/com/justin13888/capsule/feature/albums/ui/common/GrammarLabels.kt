package com.justin13888.capsule.feature.albums.ui.common

import androidx.compose.runtime.Composable
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.Operator
import com.justin13888.capsule.core.model.PredicateViolation
import com.justin13888.capsule.core.model.QueryField
import com.justin13888.capsule.feature.albums.builder.EditRefusal

/**
 * The user-facing name of one queryable field.
 *
 * A `when` over the closed enum rather than a map keyed by the wire token, so adding a field
 * to the grammar is a compile error here instead of an untranslated token on screen.
 */
@Composable
@Suppress("CyclomaticComplexMethod")
internal fun fieldLabel(field: QueryField): String =
    stringResource(
        when (field) {
            QueryField.CaptureTimestamp -> R.string.albums_field_capture_timestamp
            QueryField.ImportTimestamp -> R.string.albums_field_import_timestamp
            QueryField.ContentType -> R.string.albums_field_content_type
            QueryField.MediaKind -> R.string.albums_field_media_kind
            QueryField.GpsDatum -> R.string.albums_field_gps_datum
            QueryField.Rating -> R.string.albums_field_rating
            QueryField.DimensionsWidth -> R.string.albums_field_dimensions_width
            QueryField.DimensionsHeight -> R.string.albums_field_dimensions_height
            QueryField.Cull -> R.string.albums_field_cull
            QueryField.Hidden -> R.string.albums_field_hidden
            QueryField.TagsUser -> R.string.albums_field_tags_user
            QueryField.TagsAi -> R.string.albums_field_tags_ai
            QueryField.StackType -> R.string.albums_field_stack_type
            QueryField.PeopleCluster -> R.string.albums_field_people_cluster
            QueryField.AlbumId -> R.string.albums_field_album_id
            QueryField.Gps -> R.string.albums_field_gps
            QueryField.CameraId -> R.string.albums_field_camera_id
        },
    )

/** The user-facing name of one comparison. */
@Composable
internal fun operatorLabel(op: Operator): String =
    stringResource(
        when (op) {
            Operator.Before -> R.string.albums_operator_before
            Operator.After -> R.string.albums_operator_after
            Operator.InRange -> R.string.albums_operator_in_range
            Operator.Eq -> R.string.albums_operator_eq
            Operator.In -> R.string.albums_operator_in
            Operator.Gte -> R.string.albums_operator_gte
            Operator.Lte -> R.string.albums_operator_lte
            Operator.Is -> R.string.albums_operator_is
            Operator.Contains -> R.string.albums_operator_contains
            Operator.ContainsAny -> R.string.albums_operator_contains_any
            Operator.ContainsAll -> R.string.albums_operator_contains_all
            Operator.Exists -> R.string.albums_operator_exists
        },
    )

/**
 * What one structural rejection says to the person who caused it.
 *
 * Every violation is reported, not just the first, because a definition arriving over sync
 * cannot be fixed interactively — the user needs the whole list to understand what the other
 * device wrote. So this renders one item of that list rather than a summary.
 */
@Composable
@Suppress("CyclomaticComplexMethod")
internal fun violationText(violation: PredicateViolation): String =
    when (violation) {
        is PredicateViolation.UnknownField ->
            stringResource(R.string.albums_violation_unknown_field).withArgs("raw" to violation.raw)

        is PredicateViolation.UnknownOperator ->
            stringResource(R.string.albums_violation_unknown_operator).withArgs("raw" to violation.raw)

        is PredicateViolation.OperatorNotValidForField ->
            stringResource(R.string.albums_violation_operator_not_valid)
                .withArgs("operator" to operatorLabel(violation.op), "field" to fieldLabel(violation.field))

        is PredicateViolation.OperandTypeMismatch ->
            stringResource(R.string.albums_violation_operand_mismatch)
                .withArgs("field" to fieldLabel(violation.field), "operator" to operatorLabel(violation.op))

        is PredicateViolation.OperandNotInClosedEnum ->
            stringResource(R.string.albums_violation_operand_not_in_enum)
                .withArgs("raw" to violation.raw, "field" to fieldLabel(violation.field))

        is PredicateViolation.DepthExceeded ->
            stringResource(R.string.albums_violation_depth_exceeded).withArgs("actual" to violation.actual)

        is PredicateViolation.TermCountExceeded ->
            stringResource(R.string.albums_violation_term_count_exceeded).withArgs("actual" to violation.actual)

        is PredicateViolation.SetOperandTooLarge ->
            stringResource(R.string.albums_violation_set_too_large).withArgs("actual" to violation.actual)

        is PredicateViolation.SetMemberTooLong ->
            stringResource(R.string.albums_violation_set_member_too_long).withArgs("actual" to violation.actualBytes)

        is PredicateViolation.DisplayNameTooLong ->
            stringResource(R.string.albums_violation_name_too_long).withArgs("actual" to violation.actualBytes)

        is PredicateViolation.MissingModelSlot ->
            stringResource(R.string.albums_violation_missing_model_slot)
                .withArgs("field" to fieldLabel(violation.field))

        is PredicateViolation.RangeInverted ->
            stringResource(R.string.albums_violation_range_inverted).withArgs("field" to fieldLabel(violation.field))
    }

/** Why the editor would not apply an edit at all. */
@Composable
internal fun refusalText(refusal: EditRefusal): String =
    when (refusal) {
        EditRefusal.DepthLimit -> stringResource(R.string.albums_builder_depth_limit)
        EditRefusal.TermLimit -> stringResource(R.string.albums_builder_term_limit)
        EditRefusal.SetMemberLimit -> stringResource(R.string.albums_builder_set_limit)
        is EditRefusal.SetMemberTooLong -> stringResource(R.string.albums_builder_set_member_limit)
        is EditRefusal.OperatorNotValidForField ->
            stringResource(R.string.albums_violation_operator_not_valid)
                .withArgs("operator" to operatorLabel(refusal.op), "field" to fieldLabel(refusal.field))
    }
