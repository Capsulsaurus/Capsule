package com.justin13888.capsule.feature.viewer.editor

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.filled.StarBorder
import androidx.compose.material.icons.filled.VisibilityOff
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleToggleButton
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.GpsSource
import com.justin13888.capsule.core.model.Wire
import com.justin13888.capsule.core.model.knownOrNull
import com.justin13888.capsule.feature.viewer.withArguments

/** Test handle for the star rating control. */
const val RATING_TAG: String = "viewer.edit.rating"

/** Test handle for the trinary cull control. */
const val CULL_TAG: String = "viewer.edit.cull"

/** Test handle for the hidden toggle. */
const val HIDDEN_TAG: String = "viewer.edit.hidden"

/** Test handle for the location source controls. */
const val LOCATION_TAG: String = "viewer.edit.location"

/** The highest star rating. */
private const val MAX_RATING = 5

/**
 * The star rating, `0..5`.
 *
 * Tapping the star already set clears the rating, which is the only way to get back to zero
 * without a separate control — and a separate "no rating" button beside five stars is one
 * more thing to aim at in a workflow whose whole point is speed.
 */
@Composable
internal fun RatingSection(
    rating: Int,
    isEditable: Boolean,
    onSetRating: (Int) -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleSectionHeader(title = stringResource(R.string.viewer_edit_rating), modifier = modifier)
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = SectionPadding).testTag(RATING_TAG),
        horizontalArrangement = Arrangement.spacedBy(SectionSpacing),
    ) {
        for (star in 1..MAX_RATING) {
            val label = stringResource(R.string.viewer_edit_rating_value).withArguments("rating" to star)
            CapsuleToggleButton(
                checked = star <= rating,
                onCheckedChange = { onSetRating(if (star == rating) 0 else star) },
                label = star.toString(),
                icon = if (star <= rating) Icons.Filled.Star else Icons.Filled.StarBorder,
                enabled = isEditable,
                contentDescription = label,
            )
        }
    }
    if (rating == 0) {
        CapsuleListItem(
            headline = stringResource(R.string.viewer_edit_rating_none),
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

/**
 * The trinary review flag.
 *
 * Three states rather than a "rejected" checkbox, because "not yet reviewed" and "reviewed
 * and kept" are different facts and a two-state control cannot tell them apart. Deliberately
 * orthogonal to the star rating: a reject can still be worth three stars, and a tool that
 * conflated the two would force a lossy workflow.
 */
@Composable
internal fun CullSection(
    cull: Wire<CullFlag>,
    isEditable: Boolean,
    onSetCull: (CullFlag) -> Unit,
) {
    CapsuleSectionHeader(title = stringResource(R.string.viewer_edit_cull))
    CapsuleListItem(headline = stringResource(R.string.viewer_edit_cull_note), modifier = Modifier.fillMaxWidth())
    val current = cull.knownOrNull
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = SectionPadding).testTag(CULL_TAG),
        horizontalArrangement = Arrangement.spacedBy(SectionSpacing),
    ) {
        CullFlag.entries.forEach { flag ->
            CapsuleToggleButton(
                checked = flag == current,
                onCheckedChange = { onSetCull(flag) },
                label = stringResource(flag.labelRes()),
                enabled = isEditable,
            )
        }
    }
    if (cull is Wire.Unknown) {
        CapsuleListItem(
            headline = stringResource(R.string.viewer_edit_cull_unknown).withArguments("token" to cull.raw),
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

/**
 * The hidden register, with what hiding actually does spelled out beside it.
 *
 * Worth the sentence: hiding is view-layer only — the asset stays in its album, keeps
 * syncing, and stays reachable from an existing share — and a user who mistook it for
 * deletion or for access control would be making a privacy decision on a false premise.
 */
@Composable
internal fun HiddenSection(
    isHidden: Boolean,
    isEditable: Boolean,
    onSetHidden: (Boolean) -> Unit,
) {
    CapsuleSectionHeader(title = stringResource(R.string.viewer_edit_hidden))
    CapsuleListItem(headline = stringResource(R.string.viewer_edit_hidden_body), modifier = Modifier.fillMaxWidth())
    CapsuleToggleButton(
        checked = isHidden,
        onCheckedChange = onSetHidden,
        label = stringResource(R.string.viewer_edit_hidden),
        modifier = Modifier.padding(horizontal = SectionPadding).testTag(HIDDEN_TAG),
        icon = Icons.Filled.VisibilityOff,
        enabled = isEditable,
    )
}

/**
 * The stored fix's source, and the one promotion the user can make.
 *
 * An inferred coordinate is a suggestion, not truth: it is written to the canonical field
 * only on explicit confirmation, exactly as a suggested tag becomes a user tag only when the
 * user says so. Without that step an automated guess could silently overwrite what the
 * camera recorded.
 */
@Composable
internal fun LocationSection(
    asset: Asset,
    isEditable: Boolean,
    actions: MetadataEditorActions,
) {
    val fix = asset.gps ?: return
    CapsuleSectionHeader(title = stringResource(R.string.viewer_edit_location))
    val needsConfirmation = fix.source.knownOrNull == GpsSource.Inferred
    if (needsConfirmation) {
        CapsuleListItem(
            headline = stringResource(R.string.viewer_edit_location_confirm_body),
            modifier = Modifier.fillMaxWidth(),
        )
    }
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = SectionPadding).testTag(LOCATION_TAG),
        horizontalArrangement = Arrangement.spacedBy(SectionSpacing),
    ) {
        CapsuleButton(
            onClick = actions.onConfirmLocation,
            label = stringResource(R.string.viewer_edit_location_confirm),
            emphasis = CapsuleButtonEmphasis.Secondary,
            enabled = isEditable && needsConfirmation,
        )
        CapsuleButton(
            onClick = actions.onClearLocation,
            label = stringResource(R.string.viewer_edit_location_remove),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            enabled = isEditable,
        )
    }
}

@StringRes
private fun CullFlag.labelRes(): Int =
    when (this) {
        CullFlag.Pick -> R.string.viewer_edit_cull_pick
        CullFlag.Neutral -> R.string.viewer_edit_cull_neutral
        CullFlag.Reject -> R.string.viewer_edit_cull_reject
    }

private val SectionPadding = 16.dp
private val SectionSpacing = 8.dp
