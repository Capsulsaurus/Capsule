package com.justin13888.capsule.feature.importer

import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.ImportFileState
import com.justin13888.capsule.core.model.ImportId
import kotlinx.collections.immutable.toImmutableList
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The execution screen, in each state a run can be in.
 *
 * Per-file states are announced as well as drawn, because "importing" with a bar that has not
 * moved in a minute is exactly the case a screen-reader user cannot see their way out of.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class ImportRunScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun `each file announces the rung it is on`() {
        render(running())

        compose.onNodeWithTag(IMPORT_RUN_FILES_TAG).assertExists()
        compose.onNodeWithContentDescription("a.JPG, Encrypting", useUnmergedTree = true).assertExists()
        compose.onNodeWithContentDescription("c.JPG, Waiting", useUnmergedTree = true).assertExists()
    }

    @Test
    fun `a streaming run states the window bounding its local disk`() {
        render(running(streaming = true))

        compose.onNodeWithTag(IMPORT_WINDOW_TAG).assertExists()
    }

    @Test
    fun `losing the server pauses rather than importing ahead, and says so`() {
        render(running(streaming = true, paused = true))

        compose.onNodeWithTag(IMPORT_PAUSED_TAG).assertExists()
        compose.onNodeWithText("Paused — waiting for the server").assertExists()
    }

    @Test
    fun `a halted run localizes its code beside the files it stopped`() {
        render(running(failure = ErrorCode("error.quota.exceeded")))

        compose.onNodeWithText("Your storage is full.").assertExists()
    }

    @Test
    fun `a finished run summarizes what landed, what failed and what was passed over`() {
        render(
            ImportRunUiState(
                importId = ImportId("run"),
                isLoading = false,
                progression = progression(ImportFileState.Done, ImportFileState.Done, ImportFileState.Failed),
                skippedCount = 5,
            ),
        )

        compose.onNodeWithTag(IMPORT_SUMMARY_TAG).assertExists()
        compose.onNodeWithText("Import finished").assertExists()
        compose.onNodeWithText("5").assertExists()
    }

    @Test
    fun `a cancelled run says finished work stayed finished`() {
        var opened = false
        render(
            state =
                ImportRunUiState(
                    importId = ImportId("run"),
                    isLoading = false,
                    progression = progression(ImportFileState.Done),
                    isCancelled = true,
                ),
            actions = actions(onOpenLibrary = { opened = true }),
        )

        compose.onNodeWithText("Import stopped").assertExists()
        compose.onNodeWithText("See them in your library").performClick()
        compose.waitForIdle()

        assertTrue(opened)
    }

    @Test
    fun `a run this process cannot name offers a way back rather than an empty list`() {
        var retried = false
        render(
            state = ImportRunUiState(importId = ImportId("run"), isLoading = false, hasLocalFailure = true),
            actions = actions(onRetry = { retried = true }),
        )

        compose.onNodeWithText("The import stopped early").assertExists()
        compose.onNodeWithText("Try again").performClick()
        compose.waitForIdle()

        assertTrue(retried)
    }

    private fun render(
        state: ImportRunUiState,
        actions: ImportRunActions = actions(),
    ) {
        compose.setContent { CapsuleTheme { ImportRunScreen(state = state, actions = actions) } }
        compose.waitForIdle()
    }

    private fun actions(
        onCancel: () -> Unit = {},
        onRetry: () -> Unit = {},
        onOpenLibrary: () -> Unit = {},
        onBack: () -> Unit = {},
    ) = ImportRunActions(onCancel, onRetry, onOpenLibrary, onBack)

    private fun running(
        streaming: Boolean = false,
        paused: Boolean = false,
        failure: ErrorCode? = null,
    ) = ImportRunUiState(
        importId = ImportId("run"),
        isLoading = false,
        progression =
            progression(ImportFileState.Encrypting, ImportFileState.Uploading, ImportFileState.Queued)
                .copy(
                    windowSize =
                        if (streaming) {
                            ImportProgression.STREAMING_WINDOW
                        } else {
                            ImportProgression.DEFAULT_WINDOW
                        },
                    isPaused = paused,
                ),
        isStreaming = streaming,
        failure = failure,
    )

    private fun progression(vararg states: ImportFileState) =
        ImportProgression(
            rows =
                states
                    .mapIndexed { index, state ->
                        ImportFileRow(
                            locator = "DCIM/Camera/${'a' + index}.JPG",
                            sizeBytes = 1_024,
                            state = state,
                        )
                    }.toImmutableList(),
            windowSize = ImportProgression.DEFAULT_WINDOW,
        )
}
