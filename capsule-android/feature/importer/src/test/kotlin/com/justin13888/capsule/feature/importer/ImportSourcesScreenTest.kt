package com.justin13888.capsule.feature.importer

import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.SourceKind
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableSet
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The source picker.
 *
 * The assertion that matters most is the humblest one: a row is identified by its **relative
 * path**, and that path is what a screen reader is given. A picker that announced a bucket id
 * would be announcing a number that differs between two phones pointed at the same folder.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class ImportSourcesScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun `a source is announced by its kind and its relative path`() {
        render(loaded())

        compose.onNodeWithTag(IMPORT_SOURCES_TAG).assertExists()
        compose.onNodeWithContentDescription("Camera roll, DCIM/Camera", useUnmergedTree = true).assertExists()
        compose.onNodeWithContentDescription("Screenshots, Pictures/Screenshots", useUnmergedTree = true).assertExists()
    }

    @Test
    fun `an unmapped source says so, and a mapped one names its album`() {
        render(loaded())

        compose.onNodeWithText("Not mapped yet").assertExists()
        compose.onNodeWithText("Goes to Trip").assertExists()
    }

    @Test
    fun `scanning is unavailable until something is ticked`() {
        render(loaded(selected = emptySet()))

        compose.onNodeWithTag(IMPORT_SCAN_TAG).assertIsNotEnabled()
    }

    @Test
    fun `ticking a source reaches the caller`() {
        val toggled = mutableListOf<String>()
        render(loaded(), actions(onToggle = { toggled += it }))

        compose.onNodeWithContentDescription("Camera roll, DCIM/Camera", useUnmergedTree = true).performClick()
        compose.waitForIdle()

        assertEquals(listOf("DCIM/Camera"), toggled)
    }

    @Test
    fun `a scan in flight reports which source it is reading`() {
        render(loaded().copy(scan = ScanProgress(locator = "DCIM/Camera", filesFound = 42)))

        compose.onNodeWithText("Reading DCIM/Camera…").assertExists()
        compose.onNodeWithText("42 files so far").assertExists()
    }

    @Test
    fun `the destination question is asked once, and cannot be answered without a choice`() {
        var confirmed = false
        render(
            state = loaded().copy(prompt = ScopeMappingPrompt(source = source(SourceKind.Screenshots))),
            actions = actions(onConfirmPrompt = { confirmed = true }),
        )

        compose.onNodeWithText("Where should photos from Pictures/Screenshots go?").assertExists()
        compose.onNodeWithText("Always use this album").assertIsNotEnabled()
        assertTrue(!confirmed)
    }

    @Test
    fun `choosing an album unlocks the answer`() {
        var confirmed = false
        render(
            state =
                loaded().copy(
                    prompt =
                        ScopeMappingPrompt(source = source(SourceKind.Screenshots), chosen = AlbumId("trip")),
                ),
            actions = actions(onConfirmPrompt = { confirmed = true }),
        )

        compose.onNodeWithText("Always use this album").assertIsEnabled()
        compose.onNodeWithText("Always use this album").performClick()
        compose.waitForIdle()

        assertTrue(confirmed)
    }

    @Test
    fun `an empty device offers a folder picker rather than a dead end`() {
        var picked = false
        render(
            state = ImportSourcesUiState(isLoading = false),
            actions = actions(onPickFolder = { picked = true }),
        )

        compose.onNodeWithText("No sources to import from").assertExists()
        compose.onNodeWithText("Pick a folder").performClick()
        compose.waitForIdle()

        assertTrue(picked)
    }

    @Test
    fun `a failure localizes its code and offers a way back`() {
        var retried = false
        render(
            state = ImportSourcesUiState(isLoading = false, failure = ErrorCode("error.sync.unauthenticated")),
            actions = actions(onRetry = { retried = true }),
        )

        compose.onNodeWithText("Please sign in again to continue syncing.").assertExists()
        compose.onNodeWithText("Try again").performClick()
        compose.waitForIdle()

        assertTrue(retried)
    }

    private fun render(
        state: ImportSourcesUiState,
        actions: ImportSourcesActions = actions(),
    ) {
        compose.setContent { CapsuleTheme { ImportSourcesScreen(state = state, actions = actions) } }
        compose.waitForIdle()
    }

    @Suppress("LongParameterList")
    private fun actions(
        onToggle: (String) -> Unit = {},
        onPickFolder: () -> Unit = {},
        onAddCamera: () -> Unit = {},
        onScan: () -> Unit = {},
        onChoosePromptAlbum: (AlbumId) -> Unit = {},
        onConfirmPrompt: () -> Unit = {},
        onDismissPrompt: () -> Unit = {},
        onRetry: () -> Unit = {},
        onBack: () -> Unit = {},
    ) = ImportSourcesActions(
        onToggle,
        onPickFolder,
        onAddCamera,
        onScan,
        onChoosePromptAlbum,
        onConfirmPrompt,
        onDismissPrompt,
        onRetry,
        onBack,
    )

    private fun loaded(selected: Set<String> = setOf("DCIM/Camera")) =
        ImportSourcesUiState(
            isLoading = false,
            sources =
                persistentListOf(
                    source(SourceKind.CameraRoll).copy(destination = AlbumId("trip")),
                    source(SourceKind.Screenshots),
                ),
            selected = selected.toImmutableSet(),
            albums =
                persistentListOf(
                    ImportAlbumChoice(AlbumId("trip"), "Trip", isDefault = false),
                    ImportAlbumChoice(AlbumId("default"), "", isDefault = true),
                ),
        )

    private fun source(kind: SourceKind): ImportSource =
        ImportSource.adHoc(
            locator = if (kind == SourceKind.CameraRoll) "DCIM/Camera" else "Pictures/Screenshots",
            kind = kind,
        )
}
