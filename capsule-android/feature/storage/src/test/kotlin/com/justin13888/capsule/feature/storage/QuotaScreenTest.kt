package com.justin13888.capsule.feature.storage

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
import com.justin13888.capsule.core.model.QuotaState
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import java.time.Duration

/**
 * All five rungs of the ladder, and the different thing each one asks the user to do.
 *
 * Every rung is asserted separately rather than through a shared "is it over quota" helper,
 * because the whole point of the screen is that the five states do not mean the same thing —
 * and the two that look most like a lockout are the ones that must not read as one.
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class QuotaScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun a_reading_in_flight_shows_the_loading_state() {
        render(StorageUiState())

        compose.onNodeWithTag(STORAGE_LOADING_TAG).assertIsDisplayed()
    }

    @Test
    fun a_failure_localizes_its_code_and_offers_a_way_forward() {
        var retried = 0
        render(
            StorageFixtures
                .state(StorageFixtures.standing(QuotaState.Ok))
                .copy(failure = StorageFailure(ErrorCode("error.quota.exceeded"))),
            onRetry = { retried++ },
        )

        compose.onNodeWithTag(STORAGE_ERROR_TAG).assertIsDisplayed()
        compose.onNodeWithText(QUOTA_EXCEEDED_MESSAGE).assertIsDisplayed()
        compose.onNodeWithText(RETRY_LABEL).performClick()

        assertEquals(1, retried)
    }

    @Test
    fun a_failure_with_no_catalogue_code_falls_back_to_the_generic_message() {
        render(
            StorageFixtures
                .state(StorageFixtures.standing(QuotaState.Ok))
                .copy(failure = StorageFailure(code = null)),
        )

        compose.onNodeWithText(GENERIC_ERROR_MESSAGE).assertIsDisplayed()
    }

    @Test
    fun a_healthy_account_says_so_and_asks_for_nothing() {
        render(StorageFixtures.state(StorageFixtures.standing(QuotaState.Ok)))

        compose.onNodeWithTag(QUOTA_STATE_TAG).assertIsDisplayed()
        compose.onNodeWithText(OK_TITLE).assertIsDisplayed()
    }

    @Test
    fun the_soft_warning_points_at_what_is_large_without_alarming() {
        var reviewed = 0
        render(
            StorageFixtures.state(StorageFixtures.standing(QuotaState.SoftWarning)),
            onReviewLargest = { reviewed++ },
        )

        compose.onNodeWithText(SOFT_TITLE).assertIsDisplayed()
        compose.onNodeWithText(REVIEW_LARGEST_LABEL).performClick()

        assertEquals(1, reviewed)
    }

    @Test
    fun a_full_account_says_edits_still_work_and_counts_the_grace_window_down() {
        val standing =
            StorageFixtures
                .standing(QuotaState.HardExceeded)
                .copy(graceRemaining = Duration.ofDays(9))
        render(StorageFixtures.state(standing))

        compose.onNodeWithText(HARD_TITLE).assertIsDisplayed()
        compose.onNodeWithText(GRACE_REMAINING, substring = true).assertIsDisplayed()
    }

    @Test
    fun a_grace_expired_account_is_offered_the_way_out_rather_than_a_dead_end() {
        var trashOpened = 0
        render(
            StorageFixtures.state(StorageFixtures.standing(QuotaState.GraceExpired)),
            onOpenTrash = { trashOpened++ },
        )

        compose.onNodeWithText(GRACE_TITLE).assertIsDisplayed()
        compose.onNodeWithTag(QUOTA_DELETES_TAG).assertIsDisplayed()
        compose.onNodeWithText(OPEN_TRASH_LABEL).performClick()

        assertEquals(1, trashOpened)
    }

    @Test
    fun a_suspension_names_itself_administrative_and_points_at_a_human() {
        var support = 0
        render(
            StorageFixtures.state(StorageFixtures.standing(QuotaState.Suspended)),
            onContactSupport = { support++ },
        )

        compose.onNodeWithText(SUSPENDED_TITLE).assertIsDisplayed()
        compose.onNodeWithText(CONTACT_SUPPORT_LABEL).performClick()

        assertEquals(1, support)
    }

    @Test
    fun an_unlimited_deployment_reads_as_usage_rather_than_a_nearly_empty_bar() {
        val standing = StorageFixtures.standing(QuotaState.Ok).copy(hardLimit = null)
        render(StorageFixtures.state(standing))

        compose.onNodeWithText(NO_LIMIT, substring = true).assertIsDisplayed()
        compose.onNodeWithTag(QUOTA_BAR_TAG).assertDoesNotExist()
    }

    @Test
    fun the_quota_screen_passes_the_accessibility_checks_the_design_system_promises() {
        render(StorageFixtures.state(StorageFixtures.standing(QuotaState.GraceExpired)))

        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    private fun render(
        state: StorageUiState,
        onReviewLargest: () -> Unit = {},
        onOpenTrash: () -> Unit = {},
        onContactSupport: () -> Unit = {},
        onRetry: () -> Unit = {},
    ) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                QuotaScreen(
                    state = state,
                    onReviewLargest = onReviewLargest,
                    onOpenTrash = onOpenTrash,
                    onContactSupport = onContactSupport,
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
        const val OK_TITLE = "You have room"
        const val SOFT_TITLE = "You're close to full"
        const val HARD_TITLE = "Storage is full"
        const val GRACE_TITLE = "Full past the grace period"
        const val SUSPENDED_TITLE = "Your account is suspended"
        const val REVIEW_LARGEST_LABEL = "Review largest items"
        const val OPEN_TRASH_LABEL = "Open trash"
        const val CONTACT_SUPPORT_LABEL = "Contact support"
        const val GRACE_REMAINING = "another 9 days"
        const val NO_LIMIT = "this server sets no limit"
    }
}
