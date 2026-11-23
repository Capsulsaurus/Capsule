package com.justin13888.capsule.feature.albums.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.feature.albums.AlbumsFixtures
import com.justin13888.capsule.feature.albums.model.DuplicateGroup
import com.justin13888.capsule.feature.albums.model.DuplicateKind
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.duplicates.DuplicatesScreen
import com.justin13888.capsule.feature.albums.ui.duplicates.DuplicatesUiState
import kotlinx.collections.immutable.persistentListOf
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Duplicates keeps the two confidence levels apart and never acts without being asked.
 *
 * Identical bytes can be discarded losslessly; a near-identical frame might be the better
 * exposure. Flattening the two into one list would turn that decision into a guess the
 * screen made on the user's behalf.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class DuplicatesScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val identical =
        DuplicateGroup(
            id = "hash:same",
            kind = DuplicateKind.Identical,
            members = listOf(AlbumsFixtures.asset("a"), AlbumsFixtures.asset("b")),
        )

    private val similar =
        DuplicateGroup(
            id = "near:same",
            kind = DuplicateKind.Similar,
            members = listOf(AlbumsFixtures.asset("c"), AlbumsFixtures.asset("d")),
        )

    private fun setContent(
        state: DuplicatesUiState,
        onKeepOnly: (DuplicateGroup, AssetId) -> Unit = { _, _ -> },
    ) {
        compose.setContent {
            CapsuleTheme(dynamicColor = false) {
                DuplicatesScreen(state = state, onBack = {}, onKeepOnly = onKeepOnly, onRetry = {})
            }
        }
        compose.waitForIdle()
    }

    @Test
    fun `a clean library says so`() {
        setContent(DuplicatesUiState.Content())
        compose.onNodeWithText("No duplicates found").assertIsDisplayed()
    }

    @Test
    fun `identical bytes and near-identical frames are labelled differently`() {
        setContent(DuplicatesUiState.Content(groups = persistentListOf(identical, similar)))
        compose.onNodeWithText("Identical bytes").assertIsDisplayed()
        compose.onNodeWithText("Nearly identical").assertIsDisplayed()
    }

    @Test
    fun `discarding the rest is an explicit action on one group`() {
        var kept: Pair<String, AssetId>? = null
        setContent(
            DuplicatesUiState.Content(groups = persistentListOf(identical)),
            onKeepOnly = { group, id -> kept = group.id to id },
        )
        compose.onNodeWithTag("${AlbumsTestTags.DUPLICATES}:hash:same:keep").performClick()
        compose.waitForIdle()
        assertEquals("hash:same" to AssetId("a"), kept)
    }

    @Test
    fun `a sampled scan says how far it looked`() {
        setContent(
            DuplicatesUiState.Content(
                groups = persistentListOf(identical),
                sampled = 4_096,
                isComplete = false,
            ),
        )
        compose.onNodeWithText("Compared the 4096 most recent photos").assertIsDisplayed()
    }

    @Test
    fun `the scan renders its own loading state`() {
        setContent(DuplicatesUiState.Loading)
        compose.onNodeWithTag(AlbumsTestTags.DUPLICATES).assertIsDisplayed()
    }
}
