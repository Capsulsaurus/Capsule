package com.justin13888.capsule.feature.people

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Face
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import kotlinx.collections.immutable.persistentListOf

/** Test handle for the model-suggestion section, which is the AI/user boundary made visible. */
const val PERSON_SUGGESTIONS_TAG: String = "person.suggestions"

/** Test handle for the in-place message shown when an action is refused. */
const val PERSON_ACTION_FAILURE_TAG: String = "person.actionFailure"

private val ScreenPadding = 16.dp
private val ScreenSpacing = 12.dp

/** Everything one person's screen can ask for, in one object. */
@Suppress("LongParameterList")
@Immutable
class PersonDetailActions(
    /** Back to wherever the screen was opened from. */
    val onBack: () -> Unit,
    /** A photograph was opened. */
    val onOpenAsset: (String) -> Unit,
    /** The rename sheet was opened or dismissed. */
    val onRenameRequested: (Boolean) -> Unit,
    /** A new name was typed. */
    val onRenameSubmit: (String) -> Unit,
    /** A model suggestion was kept as one of the user's own tags. */
    val onPromote: (AiSuggestion) -> Unit,
    /** A model suggestion was rejected. */
    val onDismissSuggestion: (AiSuggestion) -> Unit,
    /** The in-place failure message was acknowledged. */
    val onDismissActionFailure: () -> Unit,
    /** The remediation on the error state. */
    val onRetry: () -> Unit,
) {
    companion object {
        /** A set that does nothing, for previews and for state-only tests. */
        val Inert: PersonDetailActions = PersonDetailActions({}, {}, {}, {}, {}, {}, {}, {})
    }
}

/**
 * One person: their name, their photographs, and what the models think is in them.
 *
 * The suggestion strip is the point of this screen beyond the grid. AI-derived values live in
 * their own namespace and can never overwrite what the user wrote, so a suggestion becoming a
 * tag is an action the user takes, one at a time, right here — never something that happens
 * because a model ran.
 */
@Composable
fun PersonDetailScreen(
    state: PersonDetailUiState,
    actions: PersonDetailActions,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize()) {
        CapsuleTopAppBar(
            title = state.displayName.ifEmpty { stringResource(R.string.android_destination_person_detail) },
            subtitle = photoCountLabel(state.photos.size),
            navigationIcon = {
                IconButton(onClick = actions.onBack) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                        contentDescription = stringResource(R.string.common_back),
                    )
                }
            },
            actions = {
                CapsuleButton(
                    onClick = { actions.onRenameRequested(true) },
                    label = stringResource(R.string.people_action_rename),
                    emphasis = CapsuleButtonEmphasis.Tertiary,
                )
            },
        )
        state.actionFailure?.let { failure ->
            CapsuleBanner(
                title = failureTitle(failure),
                modifier =
                    Modifier
                        .fillMaxWidth()
                        .padding(horizontal = ScreenPadding)
                        .testTag(PERSON_ACTION_FAILURE_TAG),
                body = failureBody(failure),
                severity = CapsuleBannerSeverity.Error,
                action = {
                    CapsuleButton(
                        onClick = actions.onDismissActionFailure,
                        label = stringResource(R.string.common_close),
                        emphasis = CapsuleButtonEmphasis.Tertiary,
                    )
                },
            )
        }
        Suggestions(state = state, actions = actions)
        Box(modifier = Modifier.weight(1f).fillMaxWidth()) {
            PersonBody(state = state, actions = actions)
        }
    }
    if (state.renaming) {
        PeopleNameSheet(
            target = NameTarget(personId = null, clusterId = null, initialName = state.displayName),
            people = state.emptyPeople(),
            onSubmit = actions.onRenameSubmit,
            onAttach = {},
            onDismiss = { actions.onRenameRequested(false) },
        )
    }
}

/** Whichever of the four states the person's own screen is in. */
@Composable
private fun PersonBody(
    state: PersonDetailUiState,
    actions: PersonDetailActions,
) {
    when (state.phase) {
        PeoplePhase.Loading ->
            CapsuleLoadingState(
                modifier = Modifier.fillMaxSize(),
                title = stringResource(R.string.people_loading),
                contentDescription = stringResource(R.string.people_loading),
            )

        PeoplePhase.Failed ->
            CapsuleErrorState(
                title = failureTitle(state.failure ?: PeopleFailure.Unknown),
                modifier = Modifier.fillMaxSize(),
                body = failureBody(state.failure ?: PeopleFailure.Unknown),
                action = {
                    CapsuleButton(
                        onClick = actions.onRetry,
                        label = stringResource(R.string.people_error_retry),
                        emphasis = CapsuleButtonEmphasis.Secondary,
                    )
                },
            )

        PeoplePhase.Empty ->
            CapsuleEmptyState(
                title = stringResource(R.string.people_person_empty),
                modifier = Modifier.fillMaxSize(),
                icon = Icons.Filled.Face,
            )

        PeoplePhase.Content -> PersonPhotos(state = state, actions = actions)
    }
}

/** The photographs, in the shared gallery. */
@Composable
private fun PersonPhotos(
    state: PersonDetailUiState,
    actions: PersonDetailActions,
) {
    val template = stringResource(R.string.people_a11y_photo)
    val total = state.photos.size
    PeopleGrid(
        photos = state.photos,
        contentDescription =
            stringResource(R.string.people_a11y_grid).withArguments("name" to state.displayName),
        cellDescription = { index -> template.withArguments("index" to index + 1, "count" to total) },
        onOpenAsset = actions.onOpenAsset,
        modifier = Modifier.fillMaxSize(),
    )
}

/**
 * What the models suggested, and the two things the user may do about it.
 *
 * Kept visibly separate from anything the user wrote. The separation is structural — the two
 * tag sets are different fields, so a model cannot reach into user intent — and this section
 * is what makes that visible: "keep as my tag" is a copy into the user's own set under a
 * fresh add id, never a move, and never automatic.
 */
@Composable
private fun Suggestions(
    state: PersonDetailUiState,
    actions: PersonDetailActions,
) {
    if (state.phase != PeoplePhase.Content) return
    CapsuleSectionHeader(title = stringResource(R.string.people_ai_tags_title))
    Text(
        text = stringResource(R.string.people_ai_tags_body),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = ScreenPadding),
    )
    if (state.suggestions.isEmpty()) {
        Text(
            text = stringResource(R.string.people_ai_tags_empty),
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(ScreenPadding),
        )
        return
    }
    Row(
        modifier =
            Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState())
                .padding(horizontal = ScreenPadding)
                .testTag(PERSON_SUGGESTIONS_TAG),
        horizontalArrangement = Arrangement.spacedBy(ScreenSpacing / 2),
    ) {
        state.suggestions.forEach { suggestion ->
            CapsuleCard {
                Text(
                    text = suggestion.tag,
                    style = MaterialTheme.typography.titleSmall,
                    modifier = Modifier.padding(horizontal = ScreenPadding, vertical = ScreenSpacing / 2),
                )
                Row(horizontalArrangement = Arrangement.spacedBy(ScreenSpacing / 4)) {
                    CapsuleButton(
                        onClick = { actions.onPromote(suggestion) },
                        label = stringResource(R.string.people_ai_tags_promote),
                        emphasis = CapsuleButtonEmphasis.Secondary,
                    )
                    CapsuleButton(
                        onClick = { actions.onDismissSuggestion(suggestion) },
                        label = stringResource(R.string.people_ai_tags_dismiss),
                        emphasis = CapsuleButtonEmphasis.Tertiary,
                    )
                }
            }
        }
    }
}

/** The rename sheet has nobody to attach to, so it is handed an empty roster. */
private fun PersonDetailUiState.emptyPeople() = persistentListOf<PersonCard>()
