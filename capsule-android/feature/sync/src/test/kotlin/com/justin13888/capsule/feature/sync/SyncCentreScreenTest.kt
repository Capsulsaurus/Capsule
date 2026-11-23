package com.justin13888.capsule.feature.sync

import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.StaleSyncNotification
import kotlinx.collections.immutable.persistentListOf
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.time.Duration
import java.time.Instant

/**
 * The sync centre, in the states the design names.
 *
 * The staleness block gets the most attention here because it is the one place where getting
 * the copy wrong changes what a user believes about their own data: snoozing and disabling
 * must read as suppressing a *warning*, never as switching sync off.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class SyncCentreScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun `a settled library says everything is backed up`() {
        render(healthy())

        compose.onNodeWithTag(SYNC_STATUS_TAG).assertExists()
        compose.onNodeWithText("Everything is backed up").assertExists()
    }

    @Test
    fun `outstanding work is counted rather than described vaguely`() {
        render(healthy().copy(pendingUploads = 3))

        compose.onNodeWithText("3 photos still to upload").assertExists()
    }

    @Test
    fun `an offline device says so instead of claiming a failure`() {
        render(healthy().copy(connection = ConnectionClass.Offline))

        compose.onNodeWithText("Offline — nothing is syncing").assertExists()
        compose.onNodeWithText("No usable connection").assertExists()
    }

    @Test
    fun `a fortnight behind raises the warning with a snooze and a way to switch it off`() {
        val snoozes = mutableListOf<Duration?>()
        render(stale(), actions(onSnoozeStale = { snoozes += it }))

        compose.onNodeWithTag(SYNC_STALE_BANNER_TAG).assertExists()
        compose.onNodeWithText("Remind me tomorrow").performClick()
        compose.onNodeWithText("Remind me in a week").performClick()
        compose.onNodeWithText("Stop warning me").performClick()
        compose.waitForIdle()

        assertEquals(listOf(Duration.ofHours(24), Duration.ofDays(7), null), snoozes)
    }

    @Test
    fun `a snoozed warning says until when, and offers to come back`() {
        var enabled = false
        render(
            state = stale().copy(staleNotification = StaleSyncNotification.Snoozed(NOW.plus(Duration.ofDays(7)))),
            actions = actions(onEnableStale = { enabled = true }),
        )

        compose.onNodeWithTag(SYNC_STALE_MUTED_TAG).assertExists()
        compose.onNodeWithText("Warn me again").performClick()
        compose.waitForIdle()

        assertTrue(enabled)
    }

    @Test
    fun `a disabled warning states that auto-sync is unaffected`() {
        render(stale().copy(staleNotification = StaleSyncNotification.Disabled))

        compose.onNodeWithText("Staleness warnings are off. Auto-sync is unaffected.").assertExists()
    }

    @Test
    fun `the forced sync says what it bypasses before it is pressed`() {
        var forced = false
        render(healthy(), actions(onForceSync = { forced = true }))

        compose
            .onNodeWithText(
                "Runs a full reconciliation now. This is the one path that ignores the metered and Wi-Fi rules, " +
                    "because you asked for it.",
            ).assertExists()
        compose.onNodeWithTag(SYNC_FORCE_TAG).performClick()
        compose.waitForIdle()

        assertTrue(forced)
    }

    @Test
    fun `a peer that reconciled locally says so`() {
        render(healthy().copy(peers = persistentListOf(peer(local = true))))

        compose.onNodeWithTag(SYNC_PEERS_TAG).assertExists()
        compose.onNodeWithText("Synced over local network").assertExists()
    }

    @Test
    fun `no peers is a statement rather than an empty gap`() {
        render(healthy())

        compose.onNodeWithText("No other devices answered on this network").assertExists()
    }

    @Test
    fun `a refused sync is a banner over content that is still true`() {
        render(healthy().copy(failure = ErrorCode("error.upload.stale_revival")))

        compose.onNodeWithText("This change is based on an out-of-date version of the item.").assertExists()
        compose.onNodeWithTag(SYNC_STATUS_TAG).assertExists()
    }

    @Test
    fun `a first read that fails replaces the content and offers a way back`() {
        var retried = false
        render(
            state = SyncCentreUiState(isLoading = true, failure = ErrorCode("error.sync.unauthenticated")),
            actions = actions(onRetry = { retried = true }),
        )

        compose.onNodeWithText("Could not read sync state").assertExists()
        compose.onNodeWithText("Try again").performClick()
        compose.waitForIdle()

        assertTrue(retried)
    }

    @Test
    fun `the queues are reachable from here, with what is waiting on each`() {
        var queues = 0
        var held = 0
        render(
            state = healthy().copy(pendingUploads = 4, quarantined = 2),
            actions = actions(onOpenQueues = { queues++ }, onOpenQuarantine = { held++ }),
        )

        compose.onNodeWithText("Upload queue").performClick()
        compose.onNodeWithText("Needs your attention").performClick()
        compose.waitForIdle()

        assertEquals(1, queues)
        assertEquals(1, held)
    }

    private fun render(
        state: SyncCentreUiState,
        actions: SyncCentreActions = actions(),
    ) {
        compose.setContent { CapsuleTheme { SyncCentreScreen(state = state, actions = actions) } }
        compose.waitForIdle()
    }

    @Suppress("LongParameterList")
    private fun actions(
        onForceSync: () -> Unit = {},
        onSnoozeStale: (Duration?) -> Unit = {},
        onEnableStale: () -> Unit = {},
        onOpenQueues: () -> Unit = {},
        onOpenQuarantine: () -> Unit = {},
        onRetry: () -> Unit = {},
        onBack: () -> Unit = {},
    ) = SyncCentreActions(
        onForceSync,
        onSnoozeStale,
        onEnableStale,
        onOpenQueues,
        onOpenQuarantine,
        onRetry,
        onBack,
    )

    private fun healthy() =
        SyncCentreUiState(
            isLoading = false,
            connection = ConnectionClass.Unmetered,
            lastCompletedSync = NOW.minus(Duration.ofMinutes(4)),
            now = NOW,
        )

    private fun stale() = healthy().copy(lastCompletedSync = NOW.minus(Duration.ofDays(15)), pendingUploads = 12)

    private fun peer(local: Boolean) =
        SyncPeer(
            id = DeviceId("peer"),
            displayName = "Studio iMac",
            lastSeen = NOW.minus(Duration.ofMinutes(2)),
            syncedOverLocalNetwork = local,
        )

    private companion object {
        val NOW: Instant = Instant.parse("2026-06-15T19:00:00Z")
    }
}
