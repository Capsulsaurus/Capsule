package com.justin13888.capsule.feature.albums.ui

import androidx.compose.runtime.Composable
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.ExperimentalRoborazziApi
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityCheckOptions
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityChecker
import com.github.takahirom.roborazzi.checkRoboAccessibility
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.feature.albums.AlbumsFixtures
import com.justin13888.capsule.feature.albums.model.RetentionCountdown
import com.justin13888.capsule.feature.albums.ui.hub.CollectionsHubScreen
import com.justin13888.capsule.feature.albums.ui.hub.CollectionsHubUiState
import com.justin13888.capsule.feature.albums.ui.hub.HubAlbum
import com.justin13888.capsule.feature.albums.ui.trash.RecentlyDeletedScreen
import com.justin13888.capsule.feature.albums.ui.trash.RecentlyDeletedUiState
import com.justin13888.capsule.feature.albums.ui.trash.TrashedItem
import kotlinx.collections.immutable.persistentListOf
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import java.time.Duration

/**
 * Google's Accessibility Test Framework over the two screens this feature owns outright.
 *
 * Contrast and touch-target size are claims every screen makes, so they are checked
 * mechanically rather than asserted in review. Recently Deleted is included specifically:
 * it is behind a gate, so it is the screen least likely to be looked at by hand, and its
 * countdown is the one piece of text a user genuinely has to be able to read.
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class AlbumsAccessibilityTest {
    @get:Rule
    val compose = createComposeRule()

    private val options =
        RoborazziATFAccessibilityCheckOptions(
            checker = RoborazziATFAccessibilityChecker(),
            failureLevel = RoborazziATFAccessibilityChecker.CheckLevel.Warning,
        )

    private fun check(content: @Composable () -> Unit) {
        compose.setContent { CapsulePreviewSurface(darkTheme = false) { content() } }
        compose.waitForIdle()
        compose.onRoot().checkRoboAccessibility(options)
    }

    @Test
    fun `the collections hub is navigable by a screen reader`() {
        check {
            CollectionsHubScreen(
                state =
                    CollectionsHubUiState.Content(
                        myAlbums =
                            persistentListOf(
                                HubAlbum(AlbumId("a"), "Iceland", isDefault = false, assetCount = 9, capability = null),
                            ),
                    ),
                onOpen = {},
                onOpenSmartAlbums = {},
                onOpenDuplicates = {},
                onOpenPlaces = {},
                onOpenGated = {},
                onCreateAlbum = {},
                onRetry = {},
            )
        }
    }

    @Test
    fun `recently deleted is navigable by a screen reader`() {
        val deletedAt = AlbumsFixtures.SPEC.now.minus(Duration.ofDays(2))
        check {
            RecentlyDeletedScreen(
                state =
                    RecentlyDeletedUiState.Content(
                        items =
                            persistentListOf(
                                TrashedItem(
                                    asset = AlbumsFixtures.asset("a"),
                                    deletedAt = deletedAt,
                                    retentionUntil = deletedAt.plus(Duration.ofDays(30)),
                                    countdown = RetentionCountdown.Days(28),
                                ),
                            ),
                        hasPurgeable = false,
                    ),
                onBack = {},
                onRestore = {},
                onEmptyTrash = {},
                onDismissRefusal = {},
                onRetry = {},
            )
        }
    }
}
