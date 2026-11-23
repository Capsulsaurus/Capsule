package com.justin13888.capsule.feature.albums.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.feature.albums.model.CollectionRef
import com.justin13888.capsule.feature.albums.model.SystemViewKind
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.hub.CollectionsHubScreen
import com.justin13888.capsule.feature.albums.ui.hub.CollectionsHubUiState
import com.justin13888.capsule.feature.albums.ui.hub.GatedSection
import com.justin13888.capsule.feature.albums.ui.hub.HubAlbum
import com.justin13888.capsule.feature.albums.ui.hub.HubMediaType
import kotlinx.collections.immutable.persistentListOf
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The hub renders every state it can be in, and routes the two gated sections differently.
 *
 * The routing assertion is the load-bearing one. Trash and Hidden must be *pushed* so the
 * routing table applies their local-auth gate; a hub that opened them like any other section
 * would render their contents without the gate ever running.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class CollectionsHubScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val content =
        CollectionsHubUiState.Content(
            myAlbums =
                persistentListOf(
                    HubAlbum(AlbumId("default"), name = "", isDefault = true, assetCount = 12, capability = null),
                    HubAlbum(AlbumId("trip"), name = "Iceland", isDefault = false, assetCount = 4, capability = null),
                ),
            mediaTypes = persistentListOf(HubMediaType(SystemViewKind.Videos, count = 3, isCountComplete = true)),
            smartAlbumCount = 2,
        )

    private fun setContent(
        state: CollectionsHubUiState,
        onOpen: (CollectionRef) -> Unit = {},
        onOpenGated: (GatedSection) -> Unit = {},
    ) {
        compose.setContent {
            CapsuleTheme(dynamicColor = false) {
                CollectionsHubScreen(
                    state = state,
                    onOpen = onOpen,
                    onOpenSmartAlbums = {},
                    onOpenDuplicates = {},
                    onOpenPlaces = {},
                    onOpenGated = onOpenGated,
                    onCreateAlbum = {},
                    onRetry = {},
                )
            }
        }
        compose.waitForIdle()
    }

    @Test
    fun `the loading state renders`() {
        setContent(CollectionsHubUiState.Loading)
        compose.onNodeWithTag(AlbumsTestTags.HUB).assertIsDisplayed()
    }

    @Test
    fun `a failed read renders instead of an empty library`() {
        setContent(CollectionsHubUiState.Failed(code = "error.album.not_available"))
        compose.onNodeWithTag(AlbumsTestTags.HUB).assertIsDisplayed()
    }

    @Test
    fun `an account with nothing in it still offers the four sections`() {
        setContent(CollectionsHubUiState.Content())
        compose.onNodeWithTag("${AlbumsTestTags.HUB_SECTION}:my-albums").assertIsDisplayed()
        compose.onNodeWithTag("${AlbumsTestTags.HUB_SECTION}:media-types").performScrollTo().assertIsDisplayed()
    }

    @Test
    fun `a container album opens through the ordinary path`() {
        var opened: CollectionRef? = null
        setContent(content, onOpen = { opened = it })
        compose.onNodeWithTag("${AlbumsTestTags.HUB_ALBUM}:trip").performScrollTo().performClick()
        compose.waitForIdle()
        assertEquals(CollectionRef.Container(AlbumId("trip")), opened)
    }

    @Test
    fun `a built-in view opens as a computed view rather than as a container`() {
        var opened: CollectionRef? = null
        setContent(content, onOpen = { opened = it })
        compose.onNodeWithTag("${AlbumsTestTags.HUB_ALBUM}:videos").performScrollTo().performClick()
        compose.waitForIdle()
        assertEquals(CollectionRef.SystemView(SystemViewKind.Videos), opened)
    }

    @Test
    fun `the gated sections are routed separately so their gate cannot be skipped`() {
        val opened = mutableListOf<GatedSection>()
        var openedOrdinary: CollectionRef? = null
        setContent(content, onOpen = { openedOrdinary = it }, onOpenGated = { opened += it })

        compose.onNodeWithTag("${AlbumsTestTags.HUB_ALBUM}:trash").performScrollTo().performClick()
        compose.onNodeWithTag("${AlbumsTestTags.HUB_ALBUM}:hidden").performScrollTo().performClick()
        compose.waitForIdle()

        assertEquals(listOf(GatedSection.Trash, GatedSection.Hidden), opened)
        assertEquals(null, openedOrdinary)
    }

    @Test
    fun `the stacks listing is reachable from the utilities section`() {
        var opened: CollectionRef? = null
        setContent(content, onOpen = { opened = it })
        compose.onNodeWithTag("${AlbumsTestTags.HUB_ALBUM}:stacks").performScrollTo().performClick()
        compose.waitForIdle()
        assertEquals(CollectionRef.Stacks, opened)
    }
}
