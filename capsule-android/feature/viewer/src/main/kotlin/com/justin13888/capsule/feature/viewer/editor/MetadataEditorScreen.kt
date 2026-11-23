package com.justin13888.capsule.feature.viewer.editor

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.PhotoLibrary
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.feature.viewer.localizedMessage

/** Test handle for the editor. */
const val METADATA_EDITOR_TAG: String = "viewer.editor"

/** Test handle for the banner explaining why every control is disabled. */
const val EDITING_DISABLED_TAG: String = "viewer.editor.disabled"

/** Test handle for the banner raised when a write was refused. */
const val EDITOR_REFUSAL_TAG: String = "viewer.editor.refusal"

/**
 * Everything on an asset the user owns and may change.
 *
 * When the asset came from a newer peer every control is *disabled rather than removed*, with
 * a banner saying why. That is the whole shape of the forward-schema rule: this build cannot
 * re-sign a sidecar whose tokens it does not understand without either dropping a field —
 * which invalidates the peer's signature — or guessing. Hiding the controls would read as a
 * missing feature; disabling them beside an explanation reads as the deliberate refusal it is.
 */
@Composable
fun MetadataEditorScreen(
    state: MetadataEditorUiState,
    actions: MetadataEditorActions,
    modifier: Modifier = Modifier,
) {
    when (state) {
        MetadataEditorUiState.Loading ->
            CapsuleLoadingState(
                modifier = modifier.fillMaxSize().testTag(METADATA_EDITOR_TAG),
                contentDescription = stringResource(R.string.common_loading),
            )

        MetadataEditorUiState.Missing ->
            CapsuleEmptyState(
                title = stringResource(R.string.viewer_empty_title),
                modifier = modifier.fillMaxSize().testTag(METADATA_EDITOR_TAG),
                body = stringResource(R.string.viewer_empty_body),
                icon = Icons.Filled.PhotoLibrary,
            )

        is MetadataEditorUiState.Failed ->
            CapsuleErrorState(
                title = stringResource(R.string.viewer_error_title),
                modifier = modifier.fillMaxSize().testTag(METADATA_EDITOR_TAG),
                body = state.failure.localizedMessage(),
                action = {
                    CapsuleButton(onClick = actions.onRetry, label = stringResource(R.string.viewer_action_retry))
                },
            )

        is MetadataEditorUiState.Ready -> EditorSections(state.editor, actions, modifier)
    }
}

/** The editable surfaces, in the order a review pass touches them. */
@Composable
private fun EditorSections(
    editor: MetadataEditorState,
    actions: MetadataEditorActions,
    modifier: Modifier,
) {
    Column(
        modifier =
            modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .testTag(METADATA_EDITOR_TAG),
    ) {
        if (!editor.isEditable) {
            CapsuleBanner(
                title = stringResource(R.string.viewer_edit_disabled),
                modifier = Modifier.fillMaxWidth().padding(EditorPadding).testTag(EDITING_DISABLED_TAG),
                body = stringResource(R.string.viewer_degraded_forward_schema_body),
                severity = CapsuleBannerSeverity.Warning,
            )
        }
        RefusalBanner(editor, actions)
        CaptionSection(editor.caption, editor.isEditable, actions)
        RatingSection(editor.asset.rating, editor.isEditable, actions.onSetRating)
        CullSection(editor.asset.cull, editor.isEditable, actions.onSetCull)
        TagSections(editor.tags, editor.isEditable, actions)
        LocationSection(editor.asset, editor.isEditable, actions)
        HiddenSection(editor.asset.isHidden, editor.isEditable, actions.onSetHidden)
    }
}

/**
 * The last refused write, localized from its code.
 *
 * Kept visible until acknowledged rather than shown as a snackbar: a quota past its grace
 * window is a condition, not an event, and a message that scrolls away after four seconds is
 * one the user will hit again without understanding why.
 */
@Composable
private fun RefusalBanner(
    editor: MetadataEditorState,
    actions: MetadataEditorActions,
) {
    val refusal = editor.refusal ?: return
    CapsuleBanner(
        title = stringResource(R.string.viewer_error_title),
        modifier = Modifier.fillMaxWidth().padding(EditorPadding).testTag(EDITOR_REFUSAL_TAG),
        body = refusal.localizedMessage(),
        severity = CapsuleBannerSeverity.Error,
        action = {
            CapsuleButton(
                onClick = actions.onDismissRefusal,
                label = stringResource(R.string.common_close),
                emphasis = CapsuleButtonEmphasis.Tertiary,
            )
        },
    )
}

private val EditorPadding = 16.dp
