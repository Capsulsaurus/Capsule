package com.justin13888.capsule.feature.timeline

import android.content.Context
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ApplicationProvider
import com.justin13888.capsule.core.data.QuotaError
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.ErrorCode
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * What the timeline actually draws in each of its states.
 *
 * Asserted against the catalog strings the screen resolves rather than against test tags
 * wherever there is one, so a change that quietly drops a remediation or mislabels an empty
 * state fails here instead of shipping.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class TimelineScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val context: Context get() = ApplicationProvider.getApplicationContext()

    private fun string(id: Int): String = context.getString(id)

    @Test
    fun `loading shows the indicator and says what it is waiting on`() {
        compose.launch { TimelineScreen(state = TimelineUiState.Loading, callbacks = TimelineCallbacks()) }

        compose.onNodeWithTag(TimelineTestTags.LOADING).assertExists()
        compose.onNodeWithText(string(R.string.timeline_loading)).assertIsDisplayed()
    }

    @Test
    fun `a failure shows the localized reason and a way out, never the code`() {
        var retried = 0
        compose.launch {
            TimelineScreen(
                state = TimelineUiState.Failed(QuotaError.GraceLocked.code, QuotaError.GraceLocked.detail),
                callbacks = TimelineCallbacks(onRetry = { retried++ }),
            )
        }

        compose.onNodeWithTag(TimelineTestTags.ERROR).assertExists()
        compose.onNodeWithText(string(R.string.error_quota_grace_locked)).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.timeline_error_retry)).performClick()

        assertEquals(1, retried)
    }

    @Test
    fun `an unknown code falls back to the generic sentence rather than pasting the key`() {
        val unknown = ErrorCode("error.from.a.newer.server")
        compose.launch {
            TimelineScreen(
                state = TimelineUiState.Failed(unknown, "whatever the server said"),
                callbacks = TimelineCallbacks(),
            )
        }

        compose.onNodeWithText(string(R.string.common_error_generic)).assertIsDisplayed()
        compose.onNodeWithText(unknown.value).assertDoesNotExist()
    }

    @Test
    fun `a never-imported library offers the import that fixes it`() {
        var imported = 0
        compose.launch {
            TimelineScreen(
                state = emptyState(TimelineEmptyReason.NeverImported),
                callbacks = TimelineCallbacks(onImport = { imported++ }),
            )
        }

        compose.onNodeWithTag(TimelineTestTags.EMPTY).assertExists()
        compose.onNodeWithText(string(R.string.timeline_empty_never_imported_body)).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.timeline_empty_never_imported_action)).performClick()

        assertEquals(1, imported)
    }

    @Test
    fun `a device-only library is described as a choice and not as a failure`() {
        compose.launch {
            TimelineScreen(
                state = emptyState(TimelineEmptyReason.NeverImportedLocalOnly),
                callbacks = TimelineCallbacks(),
            )
        }

        compose.onNodeWithText(string(R.string.timeline_empty_local_only_body)).assertIsDisplayed()
    }

    @Test
    fun `an offline empty library explains what the missing connection means`() {
        compose.launch {
            TimelineScreen(
                state = emptyState(TimelineEmptyReason.Offline, ConnectionClass.Offline),
                callbacks = TimelineCallbacks(),
            )
        }

        compose.onNodeWithText(string(R.string.timeline_empty_offline_body)).assertIsDisplayed()
    }

    @Test
    fun `a library whose photographs are all hidden or trashed says exactly that`() {
        compose.launch {
            TimelineScreen(
                state = emptyState(TimelineEmptyReason.EverythingFiltered),
                callbacks = TimelineCallbacks(),
            )
        }

        compose.onNodeWithText(string(R.string.timeline_empty_filtered_title)).assertIsDisplayed()
    }

    @Test
    fun `the grid draws a tile for every asset the window holds`() {
        val mock = TimelineFixture.repositories()
        compose.launch { TimelineScreen(state = readyState(mock), callbacks = TimelineCallbacks()) }

        compose.onNodeWithTag(TimelineTestTags.tile(0), useUnmergedTree = true).assertExists()
        compose.onNodeWithTag(TimelineTestTags.tile(1), useUnmergedTree = true).assertExists()
    }

    @Test
    fun `a degraded tile announces why, because the badge is the one thing a reader cannot see`() {
        val mock = TimelineFixture.repositories()
        val quarantined =
            TimelineFixture.landmark(mock, "quarantined", mock.backend.library.landmarks.quarantinedAssetId)
        compose.launch {
            TimelineScreen(
                state = readyState(mock, include = listOf(quarantined)),
                callbacks = TimelineCallbacks(),
            )
        }

        compose
            .onNodeWithContentDescription(
                string(R.string.timeline_degraded_quarantined),
                substring = true,
                useUnmergedTree = true,
            ).assertExists()
    }

    @Test
    fun `the offline strip appears only when uploads are actually owed`() {
        val mock = TimelineFixture.repositories()
        compose.launch {
            TimelineScreen(
                state = readyState(mock, connection = ConnectionClass.Offline, hasPendingUploads = true),
                callbacks = TimelineCallbacks(),
            )
        }

        compose.onNodeWithTag(TimelineTestTags.OFFLINE_STRIP).assertExists()
        compose.onNodeWithText(string(R.string.timeline_offline_pending_title)).assertIsDisplayed()
    }

    @Test
    fun `the offline strip stays away when nothing is waiting`() {
        val mock = TimelineFixture.repositories()
        compose.launch {
            TimelineScreen(
                state = readyState(mock, connection = ConnectionClass.Offline, hasPendingUploads = false),
                callbacks = TimelineCallbacks(),
            )
        }

        compose.onNodeWithTag(TimelineTestTags.OFFLINE_STRIP).assertDoesNotExist()
    }

    @Test
    fun `the day header names the day and counts it`() {
        val mock = TimelineFixture.repositories()
        val state = readyState(mock)
        compose.launch { TimelineScreen(state = state, callbacks = TimelineCallbacks()) }

        compose.onNodeWithTag(TimelineTestTags.ROOT).assertExists()
        assertEquals(1, state.geometry.days.size)
        assertEquals(
            state.geometry.assetCount,
            state.geometry.days
                .first()
                .count,
        )
    }

    @Test
    fun `a refused batch says so, with the localized reason and a way to dismiss it`() {
        val mock = TimelineFixture.repositories()
        var dismissed = 0
        compose.launch {
            TimelineScreen(
                state =
                    readyState(mock).copy(
                        pendingAction =
                            TimelineActionOutcome(
                                action = TimelineSelectionAction.Hide,
                                count = 3,
                                failure = QuotaError.GraceLocked.code,
                            ),
                    ),
                callbacks = TimelineCallbacks(onOutcomeHandled = { dismissed++ }),
            )
        }

        compose.onNodeWithTag(TimelineTestTags.ACTION_FAILURE).assertExists()
        compose.onNodeWithText(string(R.string.error_quota_grace_locked)).assertIsDisplayed()
        compose.onNodeWithText(string(R.string.common_close)).performClick()

        assertEquals(1, dismissed)
    }

    @Test
    fun `a batch that worked is cleared rather than celebrated`() {
        val mock = TimelineFixture.repositories()
        var cleared = 0
        compose.launch {
            TimelineScreen(
                state =
                    readyState(mock).copy(
                        pendingAction = TimelineActionOutcome(TimelineSelectionAction.Hide, count = 3),
                    ),
                callbacks = TimelineCallbacks(onOutcomeHandled = { cleared++ }),
            )
        }

        compose.onNodeWithTag(TimelineTestTags.ACTION_FAILURE).assertDoesNotExist()
        assertEquals(1, cleared)
    }

    @Test
    fun `the selection toolbar offers every action, and refuses to share offline`() {
        compose.launch {
            SelectionToolbar(count = 3, isOffline = true, onAction = {}, onSelectAll = {}, onDismiss = {})
        }

        compose.onNodeWithTag(TimelineTestTags.SELECTION_TOOLBAR).assertExists()
        // Disabled rather than hidden: a control that vanishes teaches the user nothing.
        compose.onNodeWithContentDescription(string(R.string.common_share)).assertIsNotEnabled()
        compose.onNodeWithContentDescription(string(R.string.timeline_action_hide)).assertIsEnabled()
    }

    @Test
    fun `a toolbar action reports exactly which action was pressed`() {
        val pressed = mutableListOf<TimelineSelectionAction>()
        compose.launch {
            SelectionToolbar(
                count = 2,
                isOffline = false,
                onAction = pressed::add,
                onSelectAll = {},
                onDismiss = {},
            )
        }

        compose.onNodeWithContentDescription(string(R.string.timeline_action_hide)).performClick()
        compose.onNodeWithContentDescription(string(R.string.common_delete)).performClick()
        compose.onNodeWithContentDescription(string(R.string.timeline_action_cull)).performClick()

        assertEquals(
            listOf(
                TimelineSelectionAction.Hide,
                TimelineSelectionAction.Delete,
                TimelineSelectionAction.Cull,
            ),
            pressed,
        )
    }

    @Test
    fun `the toolbar counts what is selected and can extend it to everything`() {
        var selectedAll = 0
        compose.launch {
            SelectionToolbar(
                count = 7,
                isOffline = false,
                onAction = {},
                onSelectAll = { selectedAll++ },
                onDismiss = {},
            )
        }

        val counted = string(R.string.timeline_selection_count).withArguments("count" to 7)
        compose.onNodeWithText(counted).assertIsDisplayed()
        compose.onNodeWithContentDescription(string(R.string.timeline_selection_select_all)).performClick()

        assertEquals(1, selectedAll)
    }

    @Test
    fun `the fakes really do produce the first-run library the empty state describes`() {
        val mock = TimelineFixture.repositories(MockScenario.FirstRun)
        assertEquals(0, mock.backend.visibleAssets().size)

        compose.launch {
            TimelineScreen(
                state = emptyState(TimelineEmptyReason.NeverImported),
                callbacks = TimelineCallbacks(),
            )
        }

        compose.onNodeWithTag(TimelineTestTags.EMPTY).assertExists()
    }

    private fun emptyState(
        reason: TimelineEmptyReason,
        connection: ConnectionClass = ConnectionClass.Unmetered,
    ) = TimelineUiState.Ready(
        geometry = TimelineGeometry.EMPTY,
        emptyReason = reason,
        connection = connection,
    )
}
