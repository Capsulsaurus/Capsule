package com.justin13888.capsule.feature.albums.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.feature.albums.AlbumsFixtures
import com.justin13888.capsule.feature.albums.model.RetentionCountdown
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.trash.RecentlyDeletedScreen
import com.justin13888.capsule.feature.albums.ui.trash.RecentlyDeletedUiState
import com.justin13888.capsule.feature.albums.ui.trash.TrashedItem
import kotlinx.collections.immutable.persistentListOf
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.time.Duration

/**
 * Recently Deleted shows the countdown, offers restore, and gates emptying behind a
 * confirmation.
 *
 * The disabled-empty case matters: with nothing past its signed window, emptying the trash
 * would do nothing at all, and an enabled control that silently no-ops teaches the user their
 * deletions are not being honoured.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class RecentlyDeletedScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val deletedAt = AlbumsFixtures.SPEC.now.minus(Duration.ofDays(2))

    private fun item(
        id: String,
        countdown: RetentionCountdown,
    ) = TrashedItem(
        asset = AlbumsFixtures.asset(id),
        deletedAt = deletedAt,
        retentionUntil = deletedAt.plus(Duration.ofDays(30)),
        countdown = countdown,
    )

    private fun setContent(
        state: RecentlyDeletedUiState,
        onRestore: (AssetId) -> Unit = {},
        onEmpty: () -> Unit = {},
    ) {
        compose.setContent {
            CapsuleTheme(dynamicColor = false) {
                RecentlyDeletedScreen(
                    state = state,
                    onBack = {},
                    onRestore = onRestore,
                    onEmptyTrash = onEmpty,
                    onDismissRefusal = {},
                    onRetry = {},
                )
            }
        }
        compose.waitForIdle()
    }

    @Test
    fun `each item shows how long it has left`() {
        setContent(
            RecentlyDeletedUiState.Content(
                items = persistentListOf(item("a", RetentionCountdown.Days(28))),
                hasPurgeable = false,
            ),
        )
        compose.onNodeWithTag("${AlbumsTestTags.TRASH_ITEM}:a").assertIsDisplayed()
        compose.onNodeWithText("28 days left").assertIsDisplayed()
    }

    @Test
    fun `restoring an item inside its window is offered`() {
        var restored: AssetId? = null
        setContent(
            RecentlyDeletedUiState.Content(items = persistentListOf(item("a", RetentionCountdown.Hours(3)))),
            onRestore = { restored = it },
        )
        compose.onNodeWithTag("${AlbumsTestTags.TRASH_ITEM}:a:restore").performClick()
        compose.waitForIdle()
        assertEquals(AssetId("a"), restored)
    }

    @Test
    fun `an expired item is marked rather than offered for restore`() {
        setContent(RecentlyDeletedUiState.Content(items = persistentListOf(item("a", RetentionCountdown.Expired))))
        compose.onNodeWithTag("${AlbumsTestTags.TRASH_ITEM}:a:restore").assertDoesNotExist()
    }

    @Test
    fun `emptying is disabled while nothing is past its window`() {
        setContent(
            RecentlyDeletedUiState.Content(
                items = persistentListOf(item("a", RetentionCountdown.Days(4))),
                hasPurgeable = false,
            ),
        )
        compose.onNodeWithTag("${AlbumsTestTags.TRASH}:empty").assertIsNotEnabled()
    }

    @Test
    fun `emptying asks first, and only runs on confirmation`() {
        var emptied = 0
        setContent(
            RecentlyDeletedUiState.Content(
                items = persistentListOf(item("a", RetentionCountdown.Expired)),
                hasPurgeable = true,
            ),
            onEmpty = { emptied++ },
        )
        compose.onNodeWithTag("${AlbumsTestTags.TRASH}:empty").assertIsEnabled().performClick()
        compose.waitForIdle()
        assertEquals(0, emptied)

        compose.onNodeWithTag("albums.trash.empty:confirm").performClick()
        compose.waitForIdle()
        assertEquals(1, emptied)
    }

    @Test
    fun `an empty trash says so rather than showing a blank list`() {
        setContent(RecentlyDeletedUiState.Content())
        compose.onNodeWithTag(AlbumsTestTags.TRASH).assertIsDisplayed()
        compose.onNodeWithText("Trash is empty").assertIsDisplayed()
    }

    @Test
    fun `a failed read renders its own state`() {
        setContent(RecentlyDeletedUiState.Failed(code = null))
        compose.onNodeWithTag(AlbumsTestTags.TRASH).assertIsDisplayed()
    }
}
