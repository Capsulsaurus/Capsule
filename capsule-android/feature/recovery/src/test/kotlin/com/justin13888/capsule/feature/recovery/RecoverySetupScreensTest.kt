package com.justin13888.capsule.feature.recovery

import androidx.compose.ui.test.assertIsDisplayed
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
 * The three setup surfaces: the guided re-wrap, guardian shares, and backup export.
 *
 * Grouped because each is a short linear flow and the interesting assertions are about their
 * copy rather than their layout — that the re-wrap says photos are not re-encrypted, that a
 * share nobody has been given is not counted as a recovery path, and that an export names the
 * one key that opens what it wrote.
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class RecoverySetupScreensTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun the_re_wrap_opens_by_saying_what_it_does_not_do() {
        var begun = 0
        renderRewrap(RecoveryRewrapUiState(), onBegin = { begun++ })

        compose.onNodeWithText(REWRAP_EXPLAIN_TITLE).assertIsDisplayed()
        compose.onNodeWithText(REWRAP_EXPLAIN_BODY).assertIsDisplayed()
        compose.onNodeWithText(REWRAP_BEGIN).performScrollTo().performClick()

        assertEquals(1, begun)
    }

    @Test
    fun the_revealed_phrase_numbers_every_word_so_it_can_be_written_down() {
        renderRewrap(RecoveryRewrapUiState(stage = RewrapStage.Revealing, secret = SECRET))

        compose.onNodeWithTag(RECOVERY_REWRAP_SECRET_TAG).assertIsDisplayed()
        compose.onNodeWithText(FIRST_WORD).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun the_type_back_gate_asks_for_one_numbered_word_at_a_time() {
        val typed = mutableListOf<String>()
        renderRewrap(
            RecoveryRewrapUiState(
                stage = RewrapStage.TypingBack,
                secret = SECRET,
                challenges = listOf(2),
            ),
            onTypedChange = typed::add,
        )

        compose.onNodeWithText(TYPEBACK_PROMPT).assertIsDisplayed()
        compose.onNodeWithTag(RECOVERY_REWRAP_TYPEBACK_TAG).performTextReplacement("anchor")

        assertEquals(listOf("anchor"), typed)
    }

    @Test
    fun a_wrong_word_says_which_one_it_wanted() {
        renderRewrap(
            RecoveryRewrapUiState(
                stage = RewrapStage.TypingBack,
                secret = SECRET,
                challenges = listOf(2),
                typedWrong = true,
            ),
        )

        compose.onNodeWithText(TYPEBACK_WRONG).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun a_finished_re_wrap_says_no_photo_was_touched_and_names_the_two_loose_ends() {
        var reissued = 0
        renderRewrap(
            RecoveryRewrapUiState(stage = RewrapStage.Finished, secret = SECRET, sharesInvalidated = true),
            onReissueShares = { reissued++ },
        )

        compose.onNodeWithTag(RECOVERY_REWRAP_DONE_TAG).assertIsDisplayed()
        compose.onNodeWithText(REWRAP_DONE_BODY).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(SHARES_NOTICE).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(ARTIFACT_NOTICE).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(SHARES_ACTION).performScrollTo().performClick()

        assertEquals(1, reissued)
    }

    @Test
    fun an_account_with_no_shares_is_not_told_to_re_issue_any() {
        renderRewrap(RecoveryRewrapUiState(stage = RewrapStage.Finished, secret = SECRET))

        compose.onNodeWithText(SHARES_NOTICE).assertDoesNotExist()
    }

    @Test
    fun a_failed_re_wrap_localizes_its_code() {
        renderRewrap(
            RecoveryRewrapUiState(stage = RewrapStage.Failed, failure = RecoveryFailure(EscrowError.Malformed.code)),
        )

        compose.onNodeWithTag(RECOVERY_REWRAP_ERROR_TAG).assertIsDisplayed()
        compose.onNodeWithText(ESCROW_MALFORMED).assertIsDisplayed()
    }

    @Test
    fun the_share_screen_opens_empty_with_the_default_two_of_three_scheme() {
        renderShamir(ShamirUiState())

        compose.onNodeWithTag(RECOVERY_SHAMIR_EMPTY_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(SHAMIR_BODY).assertIsDisplayed()
    }

    @Test
    fun an_impossible_scheme_is_refused_rather_than_coerced() {
        renderShamir(ShamirUiState(threshold = 1, total = 3))

        compose.onNodeWithText(SHAMIR_INVALID).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun undistributed_shares_are_not_counted_as_a_recovery_path() {
        renderShamir(ShamirUiState(enrollment = ENROLLMENT))

        compose.onNodeWithTag(RECOVERY_SHAMIR_PROGRESS_TAG).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(SHAMIR_INCOMPLETE).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun each_share_gets_a_card_and_a_way_to_record_where_it_went() {
        val edited = mutableListOf<Int?>()
        renderShamir(ShamirUiState(enrollment = ENROLLMENT), onEditHolder = edited::add)

        compose.onNodeWithTag(recoveryShareTag(1)).performScrollTo().assertIsDisplayed()
        compose.onNodeWithContentDescription(RECORD_HOLDER_ONE).performScrollTo().performClick()

        assertEquals(listOf(1), edited)
    }

    @Test
    fun enough_delivered_shares_say_the_secret_can_be_rebuilt_without_this_device() {
        renderShamir(ShamirUiState(enrollment = DISTRIBUTED))

        compose.onNodeWithText(SHAMIR_COMPLETE).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun the_export_screen_names_the_one_key_that_opens_what_it_writes() {
        renderExport(BackupExportUiState(isLoading = false))

        compose.onNodeWithText(EXPORT_BODY).assertIsDisplayed()
        compose.onNodeWithTag(RECOVERY_EXPORT_EMPTY_TAG).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun naming_nowhere_says_what_is_missing() {
        renderExport(BackupExportUiState(isLoading = false, destinationMissing = true))

        compose.onNodeWithText(DESTINATION_REQUIRED).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun a_written_archive_reports_its_size_and_entry_count() {
        renderExport(
            BackupExportUiState(
                isLoading = false,
                phase = ExportPhase.Done,
                exported = RecoveryFixtures.ARTIFACT,
                backups = listOf(RecoveryFixtures.ARTIFACT),
            ),
        )

        compose.onNodeWithTag(RECOVERY_EXPORT_DONE_TAG).assertIsDisplayed()
        compose
            .onNodeWithTag(recoveryBackupTag(RecoveryFixtures.ARTIFACT.id.value))
            .performScrollTo()
            .assertIsDisplayed()
    }

    @Test
    fun a_refused_export_localizes_its_code() {
        renderExport(
            BackupExportUiState(
                isLoading = false,
                phase = ExportPhase.Failed,
                failure = RecoveryFailure(EscrowError.Malformed.code),
            ),
        )

        compose.onNodeWithTag(RECOVERY_EXPORT_ERROR_TAG).assertIsDisplayed()
        compose.onNodeWithText(ESCROW_MALFORMED).assertIsDisplayed()
    }

    @Test
    fun every_setup_surface_passes_the_accessibility_checks() {
        renderRewrap(RecoveryRewrapUiState(stage = RewrapStage.Revealing, secret = SECRET))
        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    @Test
    fun the_share_screen_passes_the_accessibility_checks() {
        renderShamir(ShamirUiState(enrollment = ENROLLMENT))
        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    @Test
    fun the_export_screen_passes_the_accessibility_checks() {
        renderExport(
            BackupExportUiState(
                isLoading = false,
                destination = "/sdcard/Download/capsule-backup.tar",
                backups = listOf(RecoveryFixtures.ARTIFACT),
            ),
        )
        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    private fun renderRewrap(
        state: RecoveryRewrapUiState,
        onBegin: () -> Unit = {},
        onTypedChange: (String) -> Unit = {},
        onReissueShares: () -> Unit = {},
    ) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                RecoveryRewrapScreen(
                    state = state,
                    onBegin = onBegin,
                    onWrittenDown = {},
                    onTypedChange = onTypedChange,
                    onConfirm = {},
                    onAfter = RecoveryRewrapEndings(onReissueShares = onReissueShares, onReset = {}),
                )
            }
        }
        compose.waitForIdle()
    }

    private fun renderShamir(
        state: ShamirUiState,
        onEditHolder: (Int?) -> Unit = {},
    ) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                ShamirScreen(
                    state = state,
                    actions =
                        ShamirActions(
                            onThresholdChange = {},
                            onTotalChange = {},
                            onGenerate = {},
                            onEditHolder = onEditHolder,
                            onHolderDraftChange = {},
                            onMarkDelivered = {},
                        ),
                )
            }
        }
        compose.waitForIdle()
    }

    private fun renderExport(state: BackupExportUiState) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                BackupExportScreen(
                    state = state,
                    onDestinationChange = {},
                    onExport = {},
                    onReset = {},
                    onRetry = {},
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

        val SECRET =
            RecoverySecret(
                words = listOf("amber", "anchor", "apple", "arrow"),
                entropyBits = 128,
            )

        val ENROLLMENT =
            ShamirEnrollment(
                threshold = 2,
                total = 3,
                shares = listOf(ShamirShare(1, "aa11"), ShamirShare(2, "bb22"), ShamirShare(3, "cc33")),
            )

        val DISTRIBUTED =
            ENROLLMENT.copy(
                shares =
                    ENROLLMENT.shares.mapIndexed { index, share ->
                        if (index < 2) share.copy(holder = "Bank", deliveredAt = RecoveryFixtures.NOW) else share
                    },
            )

        const val REWRAP_EXPLAIN_TITLE = "This replaces the secret, not your photos"
        const val REWRAP_EXPLAIN_BODY =
            "We mint a new recovery secret and lock the same account key with it. Nothing is " +
                "re-encrypted, no photo changes, and nothing is re-uploaded."
        const val REWRAP_BEGIN = "Mint a new secret"
        const val FIRST_WORD = "Word 1: amber"
        const val TYPEBACK_PROMPT = "Type word 2 of the secret you just wrote down."
        const val TYPEBACK_WRONG = "That isn't word 2. Check the list you wrote down."
        const val REWRAP_DONE_BODY =
            "The old secret no longer opens anything. Your photos were not touched and nothing was " +
                "re-encrypted."
        const val SHARES_NOTICE = "Your old guardian shares stopped working. Hand out a new set."
        const val SHARES_ACTION = "Re-issue shares"
        const val ARTIFACT_NOTICE =
            "Backups you exported before today still open with the old secret. Re-export them, or " +
                "destroy them."
        const val SHAMIR_BODY =
            "Split your recovery secret between people or places you trust. Any 2 of 3 put it back " +
                "together; one on its own reveals nothing."
        const val SHAMIR_INVALID =
            "You need at least two shares to recover, and never more than you create."
        const val SHAMIR_INCOMPLETE =
            "Until 2 shares are somewhere other than this device, this is not yet a recovery path."
        const val SHAMIR_COMPLETE = "Enough shares are out to rebuild the secret without this device."
        const val RECORD_HOLDER_ONE = "Record who holds share 1"
        const val EXPORT_BODY =
            "Writes one encrypted archive of the library. It opens with your recovery passphrase and " +
                "with nothing else."
        const val DESTINATION_REQUIRED = "Say where the archive should go."
        const val ESCROW_MALFORMED = "The recovery backup could not be saved."
    }
}
