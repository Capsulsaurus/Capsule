package com.justin13888.capsule.feature.people

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.i18n.R
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableList
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * One person's screen.
 *
 * The suggestion strip carries the load here. AI-derived values live in their own namespace
 * so a model can never overwrite user intent, and this screen is where that separation stops
 * being an implementation detail: keeping a suggestion is a button somebody presses.
 */
@RunWith(RobolectricTestRunner::class)
class PersonDetailScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val suggestion =
        AiSuggestion(assetId = "asset-1", tag = "beach", deviceId = "device-1", counter = 1)

    private val photos =
        List(SAMPLE_SIZE) { index ->
            PersonPhoto(assetId = "asset-$index", aspectRatio = 1.5f, placeholder = 0x445566)
        }.toImmutableList()

    private fun content() =
        PersonDetailUiState(
            phase = PeoplePhase.Content,
            displayName = "Mira",
            photos = photos,
            suggestions = persistentListOf(suggestion),
        )

    @Test
    fun it_shows_the_name_the_user_typed_and_the_photographs_behind_it() {
        render(content())

        compose.onNodeWithText("Mira").assertIsDisplayed()
        compose.onNodeWithTag(PEOPLE_GRID_TAG).assertExists()
    }

    @Test
    fun model_suggestions_are_kept_visibly_apart_from_the_user_s_own_tags() {
        render(content())

        compose.onNodeWithTag(PERSON_SUGGESTIONS_TAG).assertExists()
        compose.onNodeWithText(localized(R.string.people_ai_tags_body)).assertIsDisplayed()
        compose.onNodeWithText("beach").assertIsDisplayed()
    }

    @Test
    fun keeping_a_suggestion_is_an_explicit_action() {
        val promoted = mutableListOf<AiSuggestion>()
        render(state = content(), actions = inertActions(onPromote = { promoted += it }))

        compose.onNodeWithText(localized(R.string.people_ai_tags_promote)).performClick()

        assertEquals(listOf(suggestion), promoted)
    }

    @Test
    fun a_suggestion_can_be_rejected_without_touching_anything_the_user_wrote() {
        val dismissed = mutableListOf<AiSuggestion>()
        render(state = content(), actions = inertActions(onDismissSuggestion = { dismissed += it }))

        compose.onNodeWithText(localized(R.string.people_ai_tags_dismiss)).performClick()

        assertEquals(listOf(suggestion), dismissed)
    }

    @Test
    fun a_person_with_no_current_suggestions_says_so() {
        render(content().copy(suggestions = persistentListOf()))

        compose.onNodeWithText(localized(R.string.people_ai_tags_empty)).assertIsDisplayed()
    }

    @Test
    fun a_person_with_no_photographs_yet_gets_an_empty_state() {
        render(PersonDetailUiState(phase = PeoplePhase.Empty, displayName = "Mira"))

        compose.onNodeWithText(localized(R.string.people_person_empty)).assertIsDisplayed()
    }

    @Test
    fun a_failed_load_localises_the_catalogue_code_and_offers_a_way_back() {
        var retried = 0
        render(
            state = PersonDetailUiState(phase = PeoplePhase.Failed, failure = PeopleFailure.QuotaLocked),
            actions = inertActions(onRetry = { retried++ }),
        )

        compose.onNodeWithText(localized(R.string.error_quota_grace_locked)).assertIsDisplayed()
        compose.onNodeWithText(localized(R.string.people_error_retry)).performClick()

        assertEquals(1, retried)
    }

    @Test
    fun a_refused_write_is_reported_without_taking_the_photographs_away() {
        render(content().copy(actionFailure = PeopleFailure.Suspended))

        compose.onNodeWithTag(PERSON_ACTION_FAILURE_TAG).assertExists()
        compose.onNodeWithTag(PEOPLE_GRID_TAG).assertExists()
    }

    @Test
    fun renaming_opens_the_sheet() {
        val asked = mutableListOf<Boolean>()
        render(state = content(), actions = inertActions(onRenameRequested = { asked += it }))

        compose.onNodeWithText(localized(R.string.people_action_rename)).performClick()

        assertEquals(listOf(true), asked)
    }

    @Test
    fun the_rename_sheet_is_up_when_the_state_says_so() {
        render(content().copy(renaming = true))

        compose.onNodeWithTag(PEOPLE_NAME_SHEET_TAG).assertExists()
    }

    private fun render(
        state: PersonDetailUiState,
        actions: PersonDetailActions = PersonDetailActions.Inert,
    ) {
        compose.setContent {
            CapsuleTheme(darkTheme = false, dynamicColor = false) {
                PersonDetailScreen(state = state, actions = actions)
            }
        }
        compose.waitForIdle()
    }

    private fun inertActions(
        onRenameRequested: (Boolean) -> Unit = {},
        onPromote: (AiSuggestion) -> Unit = {},
        onDismissSuggestion: (AiSuggestion) -> Unit = {},
        onRetry: () -> Unit = {},
    ) = PersonDetailActions(
        onBack = {},
        onOpenAsset = {},
        onRenameRequested = onRenameRequested,
        onRenameSubmit = {},
        onPromote = onPromote,
        onDismissSuggestion = onDismissSuggestion,
        onDismissActionFailure = {},
        onRetry = onRetry,
    )

    private companion object {
        const val SAMPLE_SIZE = 8
    }
}
