package com.justin13888.capsule.feature.albums.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.feature.albums.AlbumsFixtures
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.hidden.HiddenScreen
import com.justin13888.capsule.feature.albums.ui.hidden.HiddenUiState
import kotlinx.collections.immutable.persistentListOf
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The Hidden view states what hiding is, in every one of its states.
 *
 * Without that line a user reasonably concludes a hidden photograph has been removed from
 * their library, or pulled out of a share they already sent. It has not: hiding keeps the
 * asset in its container, keeps it syncing, and leaves it reachable from its stack.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class HiddenScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private fun setContent(state: HiddenUiState) {
        compose.setContent {
            CapsuleTheme(dynamicColor = false) {
                HiddenScreen(state = state, onBack = {}, onUnhide = {}, onRetry = {})
            }
        }
        compose.waitForIdle()
    }

    @Test
    fun `nothing hidden reads as a state rather than as a failure`() {
        setContent(HiddenUiState.Content())
        compose.onNodeWithText("Nothing hidden").assertIsDisplayed()
        compose
            .onNodeWithText("Hidden photos stay in their album and keep syncing. Hiding is not deletion.")
            .assertIsDisplayed()
    }

    @Test
    fun `hidden photographs render in the shared gallery`() {
        setContent(HiddenUiState.Content(assets = persistentListOf(AlbumsFixtures.asset("a"))))
        compose.onNodeWithTag(AlbumsTestTags.HIDDEN).assertIsDisplayed()
    }

    @Test
    fun `a failed read renders its own state`() {
        setContent(HiddenUiState.Failed(code = null))
        compose.onNodeWithTag(AlbumsTestTags.HIDDEN).assertIsDisplayed()
    }
}
