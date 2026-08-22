package com.justin13888.capsule.core.ui.gallery

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The corner markers are the only thing on a thumbnail a screen reader can be told about,
 * and they are also the only place in the gallery where a catalog message carries an
 * argument. These prove both at once: the markers announce themselves, and the ICU
 * placeholders really are substituted by the time they reach the semantics tree rather
 * than being announced as a literal `{count}`.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w800dp-h1200dp-mdpi")
class GalleryCellChromeTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun `every marker announces itself with a substituted catalog message`() {
        compose.setContent {
            CapsuleTheme {
                Box(Modifier.size(200.dp)) {
                    GalleryCellChrome(
                        markers =
                            GalleryCellMarkers(
                                stackCount = 4,
                                videoDuration = "1:23",
                                isFavourite = true,
                                sync = GalleryCellSync.Failed,
                                isDegraded = true,
                            ),
                    )
                }
            }
        }
        compose.waitForIdle()

        compose.onNodeWithText("⧉ 4", useUnmergedTree = true).assertExists()
        compose.onNodeWithContentDescription("Stack of 4", useUnmergedTree = true).assertExists()
        compose.onNodeWithContentDescription("Video, 1:23", useUnmergedTree = true).assertExists()
        compose.onNodeWithContentDescription("Favorite", useUnmergedTree = true).assertExists()
        compose.onNodeWithContentDescription("Upload failed", useUnmergedTree = true).assertExists()
        compose
            .onNodeWithContentDescription("Original unavailable, showing a preview", useUnmergedTree = true)
            .assertExists()
    }

    @Test
    fun `an asset with nothing to say draws nothing at all`() {
        compose.setContent {
            CapsuleTheme {
                Box(Modifier.size(200.dp)) { GalleryCellChrome(markers = GalleryCellMarkers.None) }
            }
        }
        compose.waitForIdle()

        compose.onNodeWithContentDescription("Favorite", useUnmergedTree = true).assertDoesNotExist()
    }
}
