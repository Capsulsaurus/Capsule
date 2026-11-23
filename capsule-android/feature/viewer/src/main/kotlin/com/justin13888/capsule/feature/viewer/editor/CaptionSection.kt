package com.justin13888.capsule.feature.viewer.editor

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.Caption
import com.justin13888.capsule.core.model.SupersededCaption
import com.justin13888.capsule.feature.viewer.ViewerFormat
import com.justin13888.capsule.feature.viewer.withArguments

/** Test handle for the caption field. */
const val CAPTION_FIELD_TAG: String = "viewer.edit.caption"

/** Test handle for the list of displaced captions. */
const val SUPERSEDED_TAG: String = "viewer.edit.superseded"

/**
 * The caption register and the values it displaced.
 *
 * The superseded list is the reason this section is not just a text field. A plain
 * last-writer-wins register drops one side of a tied edit silently, which is real data loss
 * when two people caption the same photo seconds apart; keeping the displaced values and
 * offering them back turns that into something recoverable. Restoring one is a *fresh write*,
 * not a rollback, so the restored text becomes the newest stamped value and wins on every
 * device without any special replay rule.
 */
@Composable
internal fun CaptionSection(
    caption: Caption,
    isEditable: Boolean,
    actions: MetadataEditorActions,
    modifier: Modifier = Modifier,
) {
    val current = caption.current?.value.orEmpty()
    var draft by remember(current) { mutableStateOf(current) }

    CapsuleSectionHeader(title = stringResource(R.string.viewer_edit_caption), modifier = modifier)
    OutlinedTextField(
        value = draft,
        onValueChange = { draft = it },
        modifier = Modifier.fillMaxWidth().padding(horizontal = SectionPadding).testTag(CAPTION_FIELD_TAG),
        label = { Text(stringResource(R.string.viewer_edit_caption)) },
        placeholder = { Text(stringResource(R.string.viewer_edit_caption_placeholder)) },
        enabled = isEditable,
        singleLine = false,
    )
    CaptionAttribution(caption)
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = SectionPadding),
        horizontalArrangement = Arrangement.spacedBy(SectionSpacing),
    ) {
        CapsuleButton(
            onClick = { actions.onSetCaption(draft) },
            label = stringResource(R.string.viewer_edit_caption_save),
            emphasis = CapsuleButtonEmphasis.Secondary,
            enabled = isEditable && draft != current,
        )
        CapsuleButton(
            onClick = actions.onClearCaption,
            label = stringResource(R.string.viewer_edit_caption_clear),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            enabled = isEditable && current.isNotEmpty(),
        )
    }
    SupersededCaptions(caption.superseded, isEditable, actions)
}

/** Who wrote the winning value and when, or that it was deliberately cleared. */
@Composable
private fun CaptionAttribution(caption: Caption) {
    val stamp = caption.current?.stamp ?: return
    val label =
        if (caption.current?.value == null) {
            stringResource(R.string.viewer_edit_caption_cleared)
        } else {
            stringResource(R.string.viewer_edit_caption_attribution).withArguments(
                "device" to ViewerFormat.shortHash(stamp.by.value),
                "time" to ViewerFormat.timestamp(stamp.writtenAt),
            )
        }
    CapsuleListItem(headline = label, modifier = Modifier.fillMaxWidth())
}

/** The displaced values, newest first, each restorable. */
@Composable
private fun SupersededCaptions(
    superseded: List<SupersededCaption>,
    isEditable: Boolean,
    actions: MetadataEditorActions,
) {
    if (superseded.isEmpty()) return
    CapsuleSectionHeader(title = stringResource(R.string.viewer_edit_caption_superseded))
    CapsuleListItem(
        headline = stringResource(R.string.viewer_edit_caption_superseded_body),
        modifier = Modifier.fillMaxWidth().testTag(SUPERSEDED_TAG),
    )
    superseded.forEach { displaced ->
        CapsuleListItem(
            headline = displaced.value,
            modifier = Modifier.fillMaxWidth(),
            supporting =
                stringResource(R.string.viewer_edit_caption_attribution).withArguments(
                    "device" to ViewerFormat.shortHash(displaced.writtenBy.value),
                    "time" to ViewerFormat.timestamp(displaced.writtenAt),
                ),
            trailing = {
                CapsuleButton(
                    onClick = { actions.onRestoreCaption(displaced) },
                    label = stringResource(R.string.viewer_edit_caption_restore),
                    emphasis = CapsuleButtonEmphasis.Tertiary,
                    enabled = isEditable,
                )
            },
        )
    }
}

private val SectionPadding = 16.dp
private val SectionSpacing = 8.dp
