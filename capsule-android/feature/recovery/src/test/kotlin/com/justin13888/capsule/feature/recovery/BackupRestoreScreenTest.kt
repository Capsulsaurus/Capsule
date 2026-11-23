package com.justin13888.capsule.feature.recovery

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextReplacement
import com.github.takahirom.roborazzi.ExperimentalRoborazziApi
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityCheckOptions
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityChecker
import com.github.takahirom.roborazzi.checkRoboAccessibility
import com.justin13888.capsule.core.data.EscrowError
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * What the restore ladder draws at each rung, and what it refuses to draw.
 *
 * The commit action is asserted to be absent before a dry run rather than merely disabled,
 * and the reconciliation rule is asserted to be on screen whenever a report is: those two
 * facts are what stop a user believing that pressing on will overwrite their library.
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class BackupRestoreScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun it_opens_asking_only_where_the_archive_is() {
        render(BackupRestoreUiState())

        compose.onNodeWithTag(RECOVERY_RESTORE_PATH_TAG).assertIsDisplayed()
        compose.onNodeWithText(PREVIEW_ACTION).assertIsDisplayed()
        compose.onNodeWithText(COMMIT_ACTION).assertDoesNotExist()
    }

    @Test
    fun naming_no_archive_says_what_is_missing() {
        render(BackupRestoreUiState(pathMissing = true))

        compose.onNodeWithText(PATH_REQUIRED).assertIsDisplayed()
    }

    @Test
    fun editing_the_path_reports_what_was_typed() {
        val typed = mutableListOf<String>()
        render(BackupRestoreUiState(), onPathChange = typed::add)

        compose.onNodeWithTag(RECOVERY_RESTORE_PATH_TAG).performTextReplacement(RecoveryFixtures.ARCHIVE_PATH)

        assertEquals(listOf(RecoveryFixtures.ARCHIVE_PATH), typed)
    }

    @Test
    fun a_preview_says_what_is_inside_and_that_nothing_was_decrypted() {
        render(
            BackupRestoreUiState(
                path = RecoveryFixtures.ARCHIVE_PATH,
                stage = RestoreStage.Previewed,
                artifact = RecoveryFixtures.ARTIFACT,
            ),
        )

        compose.onNodeWithTag(RECOVERY_RESTORE_PREVIEW_TAG).assertIsDisplayed()
        compose.onNodeWithText(PREVIEW_NOTE).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(AUTHENTICATED).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(DRY_RUN_ACTION).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun an_archive_that_did_not_authenticate_is_refused_rather_than_offered_anyway() {
        render(
            BackupRestoreUiState(
                path = RecoveryFixtures.ARCHIVE_PATH,
                stage = RestoreStage.Previewed,
                artifact = RecoveryFixtures.UNAUTHENTICATED,
            ),
        )

        compose.onNodeWithTag(RECOVERY_RESTORE_UNAUTH_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(DRY_RUN_ACTION).assertDoesNotExist()
    }

    @Test
    fun a_dry_run_report_shows_the_four_outcomes_and_the_rule_behind_them() {
        render(RecoveryFixtures.dryRunState())

        compose.onNodeWithTag(RECOVERY_RESTORE_REPORT_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(WOULD_ADD).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(CONFLICTS).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(NO_WRITE).performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag(RECOVERY_RESTORE_RULE_TAG).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun the_conflicts_are_reachable_from_the_report() {
        val opened = mutableListOf<Boolean>()
        render(RecoveryFixtures.dryRunState(), onReviewConflicts = opened::add)

        compose.onNodeWithText(REVIEW_CONFLICTS).performScrollTo().performClick()

        assertEquals(listOf(true), opened)
    }

    @Test
    fun the_commit_asks_for_the_phrase_and_names_what_it_will_write() {
        render(RecoveryFixtures.dryRunState().copy(stage = RestoreStage.Confirming))

        compose.onNodeWithTag(RECOVERY_RESTORE_CONFIRM_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(COMMIT_BODY).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun a_mistyped_phrase_says_exactly_what_to_type() {
        render(
            RecoveryFixtures.dryRunState().copy(
                stage = RestoreStage.Confirming,
                confirmation = "restore",
                confirmationWrong = true,
            ),
        )

        compose.onNodeWithText(CONFIRM_MISMATCH).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun confirming_hands_the_localized_phrase_back_so_the_comparison_is_in_one_language() {
        val committed = mutableListOf<String>()
        render(
            RecoveryFixtures.dryRunState().copy(stage = RestoreStage.Confirming, confirmation = PHRASE),
            onCommit = committed::add,
        )

        compose.onNodeWithText(COMMIT_ACTION).performScrollTo().performClick()

        assertEquals(listOf(PHRASE), committed)
    }

    @Test
    fun a_commit_that_added_nothing_says_the_library_was_already_ahead() {
        render(
            RecoveryFixtures.dryRunState().copy(
                stage = RestoreStage.Committed,
                committed = RecoveryFixtures.report(applied = emptyList()),
            ),
        )

        compose.onNodeWithTag(RECOVERY_RESTORE_DONE_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(NOTHING_APPLIED).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun a_failure_localizes_its_code_and_offers_a_way_forward() {
        var reset = 0
        render(
            BackupRestoreUiState(
                path = RecoveryFixtures.ARCHIVE_PATH,
                failure = RecoveryFailure(EscrowError.Malformed.code),
            ),
            onReset = { reset++ },
        )

        compose.onNodeWithTag(RECOVERY_RESTORE_ERROR_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(ESCROW_MALFORMED).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(RETRY).performScrollTo().performClick()

        assertEquals(1, reset)
    }

    @Test
    fun a_commit_in_flight_says_so() {
        render(RecoveryFixtures.dryRunState().copy(busy = RestoreBusy.Committing))

        compose.onNodeWithText(COMMITTING).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun the_conflict_surface_defaults_to_keeping_what_is_here_and_says_so() {
        render(RecoveryFixtures.dryRunState().copy(reviewingConflicts = true))

        compose.onNodeWithTag(RECOVERY_CONFLICTS_TAG).assertIsDisplayed()
        compose
            .onNodeWithTag(recoveryConflictTag(RecoveryFixtures.DIVERGENT.assetId.value))
            .performScrollTo()
            .assertIsDisplayed()
        compose.onNodeWithText(DIVERGENT_EXPLANATION).performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag(RECOVERY_CONFLICTS_UNDECIDED_TAG).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun choosing_the_older_copy_is_recorded_against_that_asset_alone() {
        val decisions = mutableListOf<Pair<String, RestoreDecision>>()
        render(
            RecoveryFixtures.dryRunState().copy(reviewingConflicts = true),
            onDecision = { id, decision -> decisions += id to decision },
        )

        compose.onNodeWithContentDescription(USE_BACKUP_FOR_DIVERGENT).performScrollTo().performClick()

        assertEquals(1, decisions.size)
        assertEquals(RecoveryFixtures.DIVERGENT.assetId.value, decisions.single().first)
        assertEquals(RestoreDecision.UseBackup, decisions.single().second)
    }

    @Test
    fun a_report_with_no_conflicts_says_there_is_nothing_to_decide() {
        render(
            RecoveryFixtures
                .dryRunState(report = RecoveryFixtures.report(conflicts = emptyList()))
                .copy(reviewingConflicts = true),
        )

        compose.onNodeWithTag(RECOVERY_CONFLICTS_EMPTY_TAG).assertIsDisplayed()
    }

    @Test
    fun the_ladder_passes_the_accessibility_checks_the_design_system_promises() {
        render(RecoveryFixtures.dryRunState())

        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    @Test
    fun the_conflict_surface_passes_the_accessibility_checks_too() {
        render(RecoveryFixtures.dryRunState().copy(reviewingConflicts = true))

        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    @Test
    fun the_commit_action_is_dead_while_a_call_is_in_flight() {
        render(
            RecoveryFixtures.dryRunState().copy(stage = RestoreStage.Confirming, busy = RestoreBusy.Committing),
        )

        compose.onNodeWithText(COMMIT_ACTION).performScrollTo().assertIsNotEnabled()
    }

    @Suppress("LongParameterList")
    private fun render(
        state: BackupRestoreUiState,
        onPathChange: (String) -> Unit = {},
        onCommit: (String) -> Unit = {},
        onReviewConflicts: (Boolean) -> Unit = {},
        onReset: () -> Unit = {},
        onDecision: (String, RestoreDecision) -> Unit = { _, _ -> },
    ) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                BackupRestoreScreen(
                    state = state,
                    actions =
                        BackupRestoreActions(
                            onPathChange = onPathChange,
                            onPreview = {},
                            onDryRun = {},
                            onBeginCommit = {},
                            onConfirmationChange = {},
                            onCommit = onCommit,
                            onReviewConflicts = onReviewConflicts,
                        ),
                    onDecision = onDecision,
                    onApplyMerge = {},
                    onReset = onReset,
                )
            }
        }
        compose.waitForIdle()
    }

    private companion object {
        val ACCESSIBILITY_OPTIONS =
            RoborazziATFAccessibilityCheckOptions(
                checker = RoborazziATFAccessibilityChecker(),
                failureLevel = RoborazziATFAccessibilityChecker.CheckLevel.Warning,
            )

        const val PHRASE = "RESTORE"
        const val PREVIEW_ACTION = "Preview this archive"
        const val DRY_RUN_ACTION = "Run the dry run"
        const val COMMIT_ACTION = "Restore"
        const val PATH_REQUIRED = "Point at a backup archive first."
        const val PREVIEW_NOTE = "Nothing has been decrypted and nothing has been written."
        const val AUTHENTICATED = "Manifest and exporter signature both verify"
        const val WOULD_ADD = "1 missing from this device, and would be added"
        const val CONFLICTS = "2 differ and need your decision"
        const val NO_WRITE = "Nothing has been written yet."
        const val REVIEW_CONFLICTS = "Decide the 2 that differ"
        const val COMMIT_BODY = "Only the 1 entries missing from this device get written. Type RESTORE to go ahead."
        const val CONFIRM_MISMATCH = "Type RESTORE exactly as shown to go ahead."
        const val NOTHING_APPLIED = "Nothing needed adding — this device was already ahead of the archive."
        const val COMMITTING = "Restoring…"
        const val ESCROW_MALFORMED = "The recovery backup could not be saved."
        const val RETRY = "Try again"
        val USE_BACKUP_FOR_DIVERGENT =
            "Take the backup's older copy of photo " + RecoveryFixtures.DIVERGENT.assetId.value
        const val DIVERGENT_EXPLANATION =
            "The two histories have split apart. This may be something you deleted after the backup " +
                "was taken, and taking the older copy would bring it back."
    }
}
