package com.justin13888.capsule.feature.audit

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import com.github.takahirom.roborazzi.ExperimentalRoborazziApi
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityCheckOptions
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityChecker
import com.github.takahirom.roborazzi.checkRoboAccessibility
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.model.ErrorCode
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * What the listing pane draws in each of the states it can be in.
 *
 * Every branch of the screen is exercised, including the two that only differ in what they
 * *mean*: an empty library and an over-narrow filter look almost the same and must never be
 * confused, because one of them says "nothing ever happened".
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class AuditLogScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun a_reading_in_flight_shows_the_loading_state() {
        renderList(AuditUiState())

        compose.onNodeWithTag(AUDIT_LOADING_TAG).assertIsDisplayed()
    }

    @Test
    fun a_failure_localizes_its_code_and_offers_a_way_forward() {
        var retried = 0
        renderList(
            AuditFixtures.state().copy(failure = AuditFailure(ErrorCode("error.quota.exceeded"))),
            onRetry = { retried++ },
        )

        compose.onNodeWithTag(AUDIT_ERROR_TAG).assertIsDisplayed()
        compose.onNodeWithText(QUOTA_EXCEEDED_MESSAGE).assertIsDisplayed()
        compose.onNodeWithText(RETRY_LABEL).performClick()

        assertEquals(1, retried)
    }

    @Test
    fun a_failure_with_no_catalogue_code_falls_back_to_the_generic_message() {
        renderList(AuditFixtures.state().copy(failure = AuditFailure(code = null)))

        compose.onNodeWithText(GENERIC_ERROR_MESSAGE).assertIsDisplayed()
    }

    @Test
    fun a_library_with_no_history_says_so_rather_than_showing_a_blank_list() {
        renderList(AuditFixtures.state(log = AuditFixtures.log(entries = emptyList())))

        compose.onNodeWithTag(AUDIT_EMPTY_TAG).assertIsDisplayed()
        compose.onNodeWithText(EMPTY_TITLE).assertIsDisplayed()
    }

    @Test
    fun an_over_narrow_filter_blames_the_filter_and_offers_to_clear_it() {
        var cleared = 0
        renderList(
            AuditFixtures.state(filter = AuditFilter(categories = emptySet())),
            onClearFilters = { cleared++ },
        )

        compose.onNodeWithText(NO_MATCHES_TITLE).assertIsDisplayed()
        compose.onNodeWithText(CLEAR_FILTERS_LABEL).performClick()

        assertEquals(1, cleared)
    }

    @Test
    fun each_entry_renders_a_row_that_opens_its_record() {
        val opened = mutableListOf<String>()
        renderList(AuditFixtures.state(), onSelectEntry = opened::add)

        compose.onNodeWithTag(auditEntryTag(AuditFixtures.DELETE.id)).assertIsDisplayed()
        compose.onNodeWithTag(auditEntryTag(AuditFixtures.TAKEDOWN.id)).performClick()

        assertEquals(listOf(AuditFixtures.TAKEDOWN.id), opened)
    }

    @Test
    fun a_partial_reading_admits_it_rather_than_passing_for_the_whole_history() {
        renderList(AuditFixtures.state(log = AuditFixtures.log(truncated = true, scannedAssets = 120)))

        compose.onNodeWithTag(AUDIT_TRUNCATED_TAG).assertIsDisplayed()
        compose.onNodeWithText(TRUNCATED_BODY).assertIsDisplayed()
    }

    @Test
    fun the_filter_chips_report_what_they_are_and_toggle_one_origin_each() {
        val toggled = mutableListOf<AuditCategory>()
        renderList(AuditFixtures.state(), onToggleCategory = toggled::add)

        compose.onNodeWithText(MODERATION_CHIP).performClick()

        assertEquals(listOf(AuditCategory.Moderation), toggled)
    }

    @Test
    fun the_listing_passes_the_accessibility_checks_the_design_system_promises() {
        renderList(AuditFixtures.state())

        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    @Test
    fun an_operator_action_is_never_hidden_from_the_user_it_was_taken_against() {
        renderList(AuditFixtures.state())

        compose.onNodeWithText(MODERATION_NOTE).assertIsDisplayed()
        assertTrue(
            "the default filter includes moderation",
            AuditFixtures.state().visible.any { it.category == AuditCategory.Moderation },
        )
    }

    private fun renderList(
        state: AuditUiState,
        onSelectEntry: (String) -> Unit = {},
        onToggleCategory: (AuditCategory) -> Unit = {},
        onToggleThisDevice: () -> Unit = {},
        onClearFilters: () -> Unit = {},
        onRetry: () -> Unit = {},
    ) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                AuditLogScreen(
                    state = state,
                    onSelectEntry = onSelectEntry,
                    onToggleCategory = onToggleCategory,
                    onToggleThisDevice = onToggleThisDevice,
                    onClearFilters = onClearFilters,
                    onRetry = onRetry,
                )
            }
        }
        compose.waitForIdle()
    }

    private companion object {
        val ACCESSIBILITY_OPTIONS =
            RoborazziATFAccessibilityCheckOptions(
                checker = RoborazziATFAccessibilityChecker(),
                failureLevel = RoborazziATFAccessibilityChecker.CheckLevel.Warning,
            )

        const val QUOTA_EXCEEDED_MESSAGE = "Your storage is full."
        const val GENERIC_ERROR_MESSAGE = "Something went wrong. Please try again."
        const val RETRY_LABEL = "Try again"
        const val EMPTY_TITLE = "Nothing recorded yet"
        const val NO_MATCHES_TITLE = "No entries match these filters"
        const val CLEAR_FILTERS_LABEL = "Clear filters"
        const val MODERATION_CHIP = "Moderation"
        const val TRUNCATED_BODY =
            "Your library is larger than one page of history. The most recent 120 photos were read; " +
                "older entries are not listed."
        const val MODERATION_NOTE =
            "A moderation action against your own library is always shown here — it is never applied silently."
    }
}
