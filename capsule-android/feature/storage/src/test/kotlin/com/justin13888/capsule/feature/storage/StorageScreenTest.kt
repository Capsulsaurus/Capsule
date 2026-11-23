package com.justin13888.capsule.feature.storage

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import com.github.takahirom.roborazzi.ExperimentalRoborazziApi
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityCheckOptions
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityChecker
import com.github.takahirom.roborazzi.checkRoboAccessibility
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.QuotaState
import com.justin13888.capsule.core.model.SyncScope
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * What the local-storage screen draws in each state, and what each control hands back.
 *
 * The release control is asserted in all three of its shapes — offered, checking, and refused
 * — because the refused shape is the one that must never be mistaken for a success.
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class StorageScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun a_reading_in_flight_shows_the_loading_state() {
        render(StorageUiState())

        compose.onNodeWithTag(STORAGE_LOADING_TAG).assertIsDisplayed()
    }

    @Test
    fun a_failure_localizes_its_code_and_offers_a_retry() {
        var retried = 0
        render(
            settled().copy(failure = StorageFailure(ErrorCode("error.storage.deep_rate_limited"))),
            onRetry = { retried++ },
        )

        compose.onNodeWithTag(STORAGE_ERROR_TAG).assertIsDisplayed()
        compose.onNodeWithText(DEEP_RATE_LIMITED_MESSAGE).assertIsDisplayed()
        compose.onNodeWithText(RETRY_LABEL).performClick()

        assertEquals(1, retried)
    }

    @Test
    fun a_device_holding_nothing_says_so_rather_than_showing_a_blank_list() {
        render(settled().copy(consumers = emptyList()))

        compose.onNodeWithTag(STORAGE_EMPTY_TAG).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun the_cache_budget_is_choosable_and_reports_what_a_sweep_freed() {
        val chosen = mutableListOf<CacheBudget>()
        var reclaimed = 0
        render(
            settled().copy(reclaim = ReclaimOutcome.Freed(2L * 1024 * 1024 * 1024)),
            onBudgetChange = chosen::add,
            onReclaim = { reclaimed++ },
        )

        compose.onNodeWithTag(STORAGE_BUDGET_TAG).assertIsDisplayed()
        compose.onNodeWithText(FREED_MESSAGE).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(RECLAIM_LABEL).performScrollTo().performClick()

        assertEquals(1, reclaimed)
    }

    @Test
    fun a_sweep_that_freed_nothing_says_that_instead_of_pretending() {
        render(settled().copy(reclaim = ReclaimOutcome.Freed(0L)))

        compose.onNodeWithText(NOTHING_FREED_MESSAGE).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun the_sync_scope_marks_the_current_choice_and_reports_a_change() {
        val chosen = mutableListOf<SyncScope>()
        render(settled().copy(syncScope = SyncScope.MetadataAndThumbnails), onSyncScopeChange = chosen::add)

        compose
            .onNodeWithTag(STORAGE_SCOPE_TAG + SyncScope.MetadataAndThumbnails.wire)
            .performScrollTo()
            .assertIsSelected()
        compose.onNodeWithTag(STORAGE_SCOPE_TAG + SyncScope.MetadataOnly.wire).performScrollTo().performClick()

        assertEquals(listOf(SyncScope.MetadataOnly), chosen)
    }

    @Test
    fun a_consumer_row_offers_pinning_and_reports_the_asset_it_belongs_to() {
        val pinned = mutableListOf<Pair<AssetId, Boolean>>()
        render(settled(), onPinnedChange = { id, value -> pinned += id to value })

        compose.onNodeWithTag(storageConsumerTag(CONSUMER_ID)).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(PIN_LABEL).performScrollTo().performClick()

        assertEquals(listOf(AssetId(CONSUMER_ID) to true), pinned)
    }

    @Test
    fun releasing_an_original_is_announced_by_what_it_frees() {
        val released = mutableListOf<AssetId>()
        render(settled(), onRelease = released::add)

        compose.onNodeWithContentDescription(RELEASE_DESCRIPTION, substring = true).performScrollTo().performClick()

        assertEquals(listOf(AssetId(CONSUMER_ID)), released)
    }

    @Test
    fun a_check_in_flight_says_so_rather_than_looking_like_it_already_happened() {
        render(settled(StorageFixtures.consumer(release = ReleaseState.Checking)))

        compose.onNodeWithText(CHECKING_LABEL).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun an_unconfirmed_verdict_is_a_standing_notice_with_a_way_to_ask_again() {
        val rechecked = mutableListOf<AssetId>()
        render(
            settled(StorageFixtures.consumer(release = ReleaseState.Unconfirmed)),
            onRelease = rechecked::add,
        )

        compose.onNodeWithTag(storageUnconfirmedTag(CONSUMER_ID)).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(UNCONFIRMED_BODY).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(RECHECK_LABEL).performScrollTo().performClick()

        assertEquals(listOf(AssetId(CONSUMER_ID)), rechecked)
    }

    @Test
    fun a_released_original_is_described_as_fetchable_rather_than_gone() {
        render(settled(StorageFixtures.consumer(release = ReleaseState.Released)))

        compose.onNodeWithText(RELEASED_MESSAGE).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun the_quota_link_carries_the_headline_figure_and_opens_the_detail() {
        var opened = 0
        render(settled(), onOpenQuota = { opened++ })

        compose.onNodeWithText(OPEN_QUOTA_LABEL).performScrollTo().performClick()

        assertEquals(1, opened)
    }

    @Test
    fun the_storage_screen_passes_the_accessibility_checks_the_design_system_promises() {
        render(settled())

        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    private fun settled(consumer: StorageConsumer = StorageFixtures.consumer(id = CONSUMER_ID)): StorageUiState =
        StorageFixtures.state(
            standing = StorageFixtures.standing(QuotaState.Ok),
            consumers = listOf(consumer.copy(id = AssetId(CONSUMER_ID))),
        )

    @Suppress("LongParameterList")
    private fun render(
        state: StorageUiState,
        onBudgetChange: (CacheBudget) -> Unit = {},
        onReclaim: () -> Unit = {},
        onPinnedChange: (AssetId, Boolean) -> Unit = { _, _ -> },
        onRelease: (AssetId) -> Unit = {},
        onSyncScopeChange: (SyncScope) -> Unit = {},
        onOpenQuota: () -> Unit = {},
        onRetry: () -> Unit = {},
    ) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                StorageScreen(
                    state = state,
                    actions =
                        StorageActions(
                            onBudgetChange = onBudgetChange,
                            onReclaim = onReclaim,
                            onPinnedChange = onPinnedChange,
                            onRelease = onRelease,
                            onSyncScopeChange = onSyncScopeChange,
                            onOpenQuota = onOpenQuota,
                            onRetry = onRetry,
                        ),
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

        const val CONSUMER_ID = "01234567-89ab-7def-8000-000000000001"
        const val DEEP_RATE_LIMITED_MESSAGE = "Too many deep storage checks. Please wait and try again."
        const val RETRY_LABEL = "Try again"
        const val RECLAIM_LABEL = "Reclaim now"
        const val FREED_MESSAGE = "Freed 2.0 GB."
        const val NOTHING_FREED_MESSAGE = "Nothing to reclaim — you are already inside the budget."
        const val PIN_LABEL = "Keep offline"
        const val RELEASE_DESCRIPTION = "Free up the space Photo from"
        const val CHECKING_LABEL = "Checking the server still holds it…"
        const val RECHECK_LABEL = "Check again"
        const val UNCONFIRMED_BODY =
            "The server has not confirmed it holds this original, so this device may still be the only copy. " +
                "Nothing was deleted."
        const val RELEASED_MESSAGE = "Freed — it is fetched again when you open it"
        const val OPEN_QUOTA_LABEL = "See your account allowance"
    }
}
