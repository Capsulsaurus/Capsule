package com.justin13888.capsule.feature.importer

import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.DefaultAlbumRule
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.ImportCandidate
import com.justin13888.capsule.core.model.ImportDecision
import com.justin13888.capsule.core.model.ImportId
import com.justin13888.capsule.core.model.ImportPlan
import com.justin13888.capsule.core.model.UploadPolicy
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The plan-and-confirm screen, in each of the states it has to be able to draw.
 *
 * The confirm action is what most of these are really about: it is the last point before
 * bytes move, so a state that must not proceed — conflicts outstanding, one file larger than
 * the disk, streaming against a staged policy — has to reach the button, not just the copy.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class ImportPlanScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun `a plan that fits shows its counts and offers to run`() {
        render(ImportPlanUiState(isLoading = false, plan = plan(), destinationName = "Trip"))

        compose.onNodeWithTag(IMPORT_PLAN_SUMMARY_TAG).assertExists()
        compose.onNodeWithText("Trip").assertExists()
        compose.onNodeWithTag(IMPORT_CONFIRM_TAG).assertIsEnabled()
    }

    @Test
    fun `the nameless default album is named rather than left blank`() {
        render(ImportPlanUiState(isLoading = false, plan = plan(), destinationName = ""))

        compose.onNodeWithText("Library").assertExists()
    }

    @Test
    fun `a streaming recommendation is shown before anything runs`() {
        render(
            ImportPlanUiState(
                isLoading = false,
                plan = plan(),
                space = ImportSpaceVerdict.StreamingRecommended(freeBytes = 1_000, neededBytes = 10_000),
            ),
        )

        compose.onNodeWithTag(IMPORT_STREAMING_BANNER_TAG).assertExists()
        compose.onNodeWithTag(IMPORT_CONFIRM_TAG).assertIsEnabled()
    }

    @Test
    fun `one file larger than the disk blocks the run and says what to free`() {
        render(
            ImportPlanUiState(
                isLoading = false,
                plan = plan(),
                space =
                    ImportSpaceVerdict.SingleFileTooLarge(
                        locator = "DCIM/Camera/IMG_9999.RAF",
                        sizeBytes = 900_000_000,
                        freeBytes = 100_000_000,
                    ),
            ),
        )

        compose.onNodeWithTag(IMPORT_BLOCKED_TAG).assertExists()
        compose.onNodeWithTag(IMPORT_CONFIRM_TAG).assertIsNotEnabled()
    }

    @Test
    fun `streaming under a staged policy offers the switch rather than proceeding`() {
        var switched = false
        render(
            state =
                ImportPlanUiState(
                    isLoading = false,
                    plan = plan(),
                    space = ImportSpaceVerdict.StreamingRecommended(freeBytes = 1_000, neededBytes = 10_000),
                    uploadPolicy = UploadPolicy.Staged,
                ),
            actions = actions(onSwitchToFullUploads = { switched = true }),
        )

        compose.onNodeWithTag(IMPORT_CONFIRM_TAG).assertIsNotEnabled()
        compose.onNodeWithText("Switch to full uploads").performClick()
        compose.waitForIdle()

        assertTrue(switched)
    }

    @Test
    fun `outstanding conflicts hold the run back and say why`() {
        render(ImportPlanUiState(isLoading = false, plan = plan(conflicts = 2)))

        compose.onNodeWithTag(IMPORT_CONFIRM_TAG).assertIsNotEnabled()
        compose.onNodeWithText("Resolve the files that need a decision before this import can run.").assertExists()
    }

    @Test
    fun `an empty plan is an empty state, not an error`() {
        render(ImportPlanUiState(isLoading = false, plan = plan(added = 0, skipped = 3)))

        compose.onNodeWithText("Nothing new to import").assertExists()
    }

    @Test
    fun `a failure localizes its code and offers a way back`() {
        var retried = false
        render(
            state = ImportPlanUiState(isLoading = false, failure = ErrorCode("error.quota.exceeded")),
            actions = actions(onRetry = { retried = true }),
        )

        compose.onNodeWithText("Your storage is full.").assertExists()
        compose.onNodeWithText("Try again").performClick()
        compose.waitForIdle()

        assertTrue(retried)
    }

    @Test
    fun `the loading state announces what it is waiting for`() {
        render(ImportPlanUiState(isLoading = true))

        compose.onNodeWithText("Planning the import…").assertExists()
    }

    private fun render(
        state: ImportPlanUiState,
        actions: ImportPlanActions = actions(),
    ) {
        compose.setContent { CapsuleTheme { ImportPlanScreen(state = state, actions = actions) } }
        compose.waitForIdle()
    }

    private fun actions(
        onSetMode: (ImportMode) -> Unit = {},
        onConfirm: () -> Unit = {},
        onSwitchToFullUploads: () -> Unit = {},
        onRetry: () -> Unit = {},
        onBack: () -> Unit = {},
    ) = ImportPlanActions(onSetMode, onConfirm, onSwitchToFullUploads, onRetry, onBack)

    private fun plan(
        added: Int = 4,
        skipped: Int = 1,
        conflicts: Int = 0,
    ): ImportPlan {
        val destination = AlbumId("album")
        return ImportPlan(
            importId = ImportId("import"),
            added = (1..added).map { candidate("add$it", ImportDecision.Import(destination)) },
            skipped = (1..skipped).map { candidate("skip$it", ImportDecision.SkipUnsupported) },
            conflicts = (1..conflicts).map { candidate("bad$it", ImportDecision.SkipError("unreadable")) },
            totalSize = added.toLong() * 1_024 * 1_024,
            destinationRule = DefaultAlbumRule.OwnerPointer,
        )
    }

    private fun candidate(
        name: String,
        decision: ImportDecision,
    ) = ImportCandidate(sourceLocator = "DCIM/Camera/$name.JPG", sizeBytes = 1_024 * 1_024, decision = decision)
}
