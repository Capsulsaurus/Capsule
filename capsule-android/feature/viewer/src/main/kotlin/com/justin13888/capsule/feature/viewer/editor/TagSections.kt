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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AiTag
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.UserTag
import com.justin13888.capsule.feature.viewer.withArguments

/** Test handle for the tags the user wrote. */
const val USER_TAGS_TAG: String = "viewer.edit.tags-user"

/** Test handle for the tags a model suggested. */
const val AI_TAGS_TAG: String = "viewer.edit.tags-ai"

/**
 * Both tag namespaces, side by side and structurally apart.
 *
 * They are shown together because the promote and dismiss affordances only make sense in
 * contrast, and kept in separate sections for the same reason they are separate fields on the
 * wire: a model can never write into the user's set, so "can a suggestion overwrite what I
 * typed?" is a question that cannot arise.
 */
@Composable
internal fun TagSections(
    tags: AssetTags,
    isEditable: Boolean,
    actions: MetadataEditorActions,
    modifier: Modifier = Modifier,
) {
    UserTagSection(tags.user, isEditable, actions, modifier)
    AiTagSection(tags.ai, isEditable, actions)
}

/** The tags the user wrote, each removable by the add it targets. */
@Composable
private fun UserTagSection(
    tags: List<UserTag>,
    isEditable: Boolean,
    actions: MetadataEditorActions,
    modifier: Modifier,
) {
    var draft by remember { mutableStateOf("") }
    CapsuleSectionHeader(title = stringResource(R.string.viewer_edit_tags_user), modifier = modifier)
    if (tags.isEmpty()) {
        CapsuleListItem(
            headline = stringResource(R.string.viewer_edit_tags_user_empty),
            modifier = Modifier.fillMaxWidth().testTag(USER_TAGS_TAG),
        )
    }
    tags.forEach { tag ->
        CapsuleListItem(
            headline = tag.tag,
            modifier = Modifier.fillMaxWidth().testTag(USER_TAGS_TAG),
            trailing = {
                CapsuleButton(
                    onClick = { actions.onRemoveUserTag(tag.addId) },
                    label = stringResource(R.string.common_remove),
                    emphasis = CapsuleButtonEmphasis.Tertiary,
                    enabled = isEditable,
                    contentDescription =
                        stringResource(R.string.viewer_edit_tags_user_remove).withArguments("tag" to tag.tag),
                )
            },
        )
    }
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = SectionPadding),
        horizontalArrangement = Arrangement.spacedBy(SectionSpacing),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        OutlinedTextField(
            value = draft,
            onValueChange = { draft = it },
            modifier = Modifier.weight(1f),
            label = { Text(stringResource(R.string.viewer_edit_tags_user_field)) },
            enabled = isEditable,
            singleLine = true,
        )
        CapsuleButton(
            onClick = {
                actions.onAddUserTag(draft)
                draft = ""
            },
            label = stringResource(R.string.viewer_edit_tags_user_add),
            emphasis = CapsuleButtonEmphasis.Secondary,
            enabled = isEditable && draft.isNotBlank(),
        )
    }
}

/**
 * The model's suggestions, with keep and dismiss on each.
 *
 * A stale entry — one from a model version that has since been swapped — is shown and marked
 * rather than hidden. It is excluded from queries and queued for regeneration, not deleted:
 * the original is still there to regenerate from, and throwing the entry away would lose the
 * fact that something was suggested here at all.
 */
@Composable
private fun AiTagSection(
    tags: List<AiTag>,
    isEditable: Boolean,
    actions: MetadataEditorActions,
) {
    CapsuleSectionHeader(title = stringResource(R.string.viewer_edit_tags_ai))
    CapsuleListItem(headline = stringResource(R.string.viewer_edit_tags_ai_note), modifier = Modifier.fillMaxWidth())
    if (tags.isEmpty()) {
        CapsuleListItem(
            headline = stringResource(R.string.viewer_edit_tags_ai_empty),
            modifier = Modifier.fillMaxWidth().testTag(AI_TAGS_TAG),
        )
        return
    }
    tags.forEach { tag -> AiTagRow(tag, isEditable, actions) }
}

/** One suggestion: its text, the slot that produced it, and the two decisions. */
@Composable
private fun AiTagRow(
    tag: AiTag,
    isEditable: Boolean,
    actions: MetadataEditorActions,
) {
    val provenance =
        stringResource(R.string.viewer_edit_tags_ai_model).withArguments(
            "model" to tag.slot.modelId.value,
            "version" to tag.slot.modelVersion.value,
        )
    val staleNote = if (tag.isStale) stringResource(R.string.viewer_edit_tags_ai_stale) else null
    CapsuleListItem(
        headline = tag.tag,
        modifier = Modifier.fillMaxWidth().testTag(AI_TAGS_TAG),
        supporting = listOfNotNull(provenance, staleNote).joinToString(separator = "\n"),
        trailing = {
            Row(horizontalArrangement = Arrangement.spacedBy(SectionSpacing)) {
                CapsuleButton(
                    onClick = { actions.onPromoteAiTag(tag.addId) },
                    label = stringResource(R.string.viewer_edit_tags_ai_promote).withArguments("tag" to tag.tag),
                    emphasis = CapsuleButtonEmphasis.Tertiary,
                    enabled = isEditable,
                )
                CapsuleButton(
                    onClick = { actions.onDismissAiTag(tag.addId) },
                    label = stringResource(R.string.viewer_edit_tags_ai_dismiss).withArguments("tag" to tag.tag),
                    emphasis = CapsuleButtonEmphasis.Tertiary,
                    enabled = isEditable,
                )
            }
        },
    )
}

private val SectionPadding = 16.dp
private val SectionSpacing = 8.dp
