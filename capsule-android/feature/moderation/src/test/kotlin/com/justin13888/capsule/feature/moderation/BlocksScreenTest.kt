package com.justin13888.capsule.feature.moderation

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextReplacement
import com.github.takahirom.roborazzi.ExperimentalRoborazziApi
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityCheckOptions
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityChecker
import com.github.takahirom.roborazzi.checkRoboAccessibility
import com.justin13888.capsule.core.data.ModerationError
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * What the blocked list draws in each of the states it can be in.
 *
 * The scope copy is asserted rather than assumed: it is the only thing on the screen that
 * stops a personal block being read as a server-wide one, and a wording that quietly went
 * missing would be invisible to every other test.
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class BlocksScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun a_reading_in_flight_shows_the_loading_state() {
        render(BlocksUiState())

        compose.onNodeWithTag(MODERATION_BLOCKS_LOADING_TAG).assertIsDisplayed()
    }

    @Test
    fun an_empty_list_says_so_rather_than_showing_a_blank_column() {
        render(ModerationFixtures.blocksState(blocks = emptyList()))

        compose.onNodeWithTag(MODERATION_BLOCKS_EMPTY_TAG).assertIsDisplayed()
        compose.onNodeWithText(EMPTY_TITLE).assertIsDisplayed()
    }

    @Test
    fun the_screen_states_how_far_a_block_reaches_every_time_it_is_shown() {
        render(ModerationFixtures.blocksState())

        compose.onNodeWithText(SCOPE_NOTE).assertIsDisplayed()
        compose.onNodeWithText(EPOCH_NOTE).assertIsDisplayed()
    }

    @Test
    fun each_blocked_account_gets_a_row_whose_unblock_names_it() {
        val lifted = mutableListOf<String>()
        render(ModerationFixtures.blocksState(), onUnblock = lifted::add)
        val account = ModerationFixtures.BLOCK.blockedUserId.value

        compose.onNodeWithTag(moderationBlockTag(account)).assertIsDisplayed()
        compose.onNodeWithContentDescription(UNBLOCK_PREFIX + account).performClick()

        assertEquals(listOf(account), lifted)
    }

    @Test
    fun typing_an_account_reports_what_was_typed() {
        val typed = mutableListOf<String>()
        render(ModerationFixtures.blocksState(), onDraftChange = typed::add)

        compose.onNodeWithTag(MODERATION_BLOCKS_FIELD_TAG).performTextReplacement(ACCOUNT)

        assertEquals(listOf(ACCOUNT), typed)
    }

    @Test
    fun an_empty_field_says_what_is_missing() {
        render(ModerationFixtures.blocksState().copy(draftMissing = true))

        compose.onNodeWithText(ID_REQUIRED).assertIsDisplayed()
    }

    @Test
    fun a_failed_first_reading_localizes_its_code_and_offers_a_way_forward() {
        var retried = 0
        render(
            BlocksUiState(isLoading = false, failure = ModerationFailure(ModerationError.ServerBlocked.code)),
            onRetry = { retried++ },
        )

        compose.onNodeWithTag(MODERATION_BLOCKS_ERROR_TAG).assertIsDisplayed()
        compose.onNodeWithText(SERVER_BLOCKED).assertIsDisplayed()
        compose.onNodeWithText(RETRY).performClick()

        assertEquals(1, retried)
    }

    @Test
    fun the_list_passes_the_accessibility_checks_the_design_system_promises() {
        // A draft in the field, so the add action is live: a disabled Material control's
        // greyed label fails the contrast check by construction, and asserting on it here
        // would be testing the palette rather than this layout.
        render(ModerationFixtures.blocksState().copy(draft = ACCOUNT))

        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    private fun render(
        state: BlocksUiState,
        onDraftChange: (String) -> Unit = {},
        onUnblock: (String) -> Unit = {},
        onRetry: () -> Unit = {},
    ) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                BlocksScreen(
                    state = state,
                    onDraftChange = onDraftChange,
                    onBlock = {},
                    onUnblock = onUnblock,
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

        const val ACCOUNT = "alice@other.tld"
        const val EMPTY_TITLE = "You haven't blocked anyone"
        const val SCOPE_NOTE =
            "Blocking takes someone out of the albums you share with them and stops them sharing new " +
                "ones with you. It is yours alone — it never becomes a block for everyone on the server."
        const val EPOCH_NOTE =
            "It lands at their next sync, and keys they already hold are not clawed back."
        const val UNBLOCK_PREFIX = "Unblock "
        const val ID_REQUIRED = "Name the account you want to block."
        const val SERVER_BLOCKED = "This server is blocked from federating with us."
        const val RETRY = "Try again"
    }
}
