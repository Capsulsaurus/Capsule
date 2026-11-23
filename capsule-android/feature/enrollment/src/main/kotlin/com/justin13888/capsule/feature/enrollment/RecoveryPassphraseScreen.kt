package com.justin13888.capsule.feature.enrollment

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.itemsIndexed
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.capsuleDescription
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviews
import com.justin13888.capsule.core.i18n.R
import kotlinx.collections.immutable.persistentListOf
import kotlin.random.Random

/** Test handle for the passphrase grid. */
const val PASSPHRASE_GRID_TAG: String = "enrollment.passphrase-grid"

/** Test handle for the type-back gate, which setup cannot complete without. */
const val TYPE_BACK_GATE_TAG: String = "enrollment.type-back"

/** Test handle for the button that checks the typed-back words. */
const val TYPE_BACK_SUBMIT_TAG: String = "enrollment.type-back-submit"

private val ScreenPadding = 24.dp
private val BlockSpacing = 16.dp
private val GridHeight = 320.dp
private const val GRID_COLUMNS = 3

/**
 * The one time these words are ever on a screen.
 *
 * Nothing here writes them anywhere: there is no clipboard call the user did not ask for, no
 * autosave, and the view model drops them the moment the gate is passed. The screenshot
 * warning is copy rather than a `FLAG_SECURE`, because the honest risk is not that the OS
 * captures the screen — it is that the capture lands in the photo library this app is about to
 * start backing up.
 */
@Suppress("LongParameterList")
@Composable
fun RecoveryPassphraseScreen(
    passphrase: RecoveryPassphrase,
    hasCopied: Boolean,
    onCopy: () -> Unit,
    onPrint: () -> Unit,
    onContinue: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(ScreenPadding),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        Text(
            text = stringResource(R.string.android_enrollment_passphrase_title),
            style = MaterialTheme.typography.headlineMediumEmphasized,
        )
        Text(
            text = stringResource(R.string.android_enrollment_passphrase_body),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        WordGrid(passphrase)
        Text(
            text = stringResource(R.string.android_enrollment_passphrase_never_stored),
            style = MaterialTheme.typography.labelLarge,
        )
        CapsuleBanner(
            title = stringResource(R.string.android_enrollment_passphrase_screenshot_warning),
            severity = CapsuleBannerSeverity.Warning,
            modifier = Modifier.fillMaxWidth(),
        )
        if (hasCopied) {
            CapsuleBanner(
                title = stringResource(R.string.android_enrollment_passphrase_copied),
                severity = CapsuleBannerSeverity.Info,
                modifier = Modifier.fillMaxWidth(),
            )
        }
        Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(BlockSpacing)) {
            CapsuleButton(
                onClick = onCopy,
                label = stringResource(R.string.android_enrollment_passphrase_copy),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
            CapsuleButton(
                onClick = onPrint,
                label = stringResource(R.string.android_enrollment_passphrase_print),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        }
        CapsuleButton(
            onClick = onContinue,
            label = stringResource(R.string.android_enrollment_passphrase_continue),
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

/**
 * The words, numbered.
 *
 * Each cell announces its own position and word together, so a screen-reader user hears
 * "word four, harbor" rather than twelve unlabelled nouns in a grid whose reading order they
 * would have to infer.
 */
@Composable
private fun WordGrid(
    passphrase: RecoveryPassphrase,
    modifier: Modifier = Modifier,
) {
    val template = stringResource(R.string.android_enrollment_passphrase_word)
    LazyVerticalGrid(
        columns = GridCells.Fixed(GRID_COLUMNS),
        modifier = modifier.fillMaxWidth().height(GridHeight).testTag(PASSPHRASE_GRID_TAG),
        horizontalArrangement = Arrangement.spacedBy(BlockSpacing / 2),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing / 2),
    ) {
        itemsIndexed(passphrase.words) { index, word ->
            val announcement = template.withArguments("position" to index + 1, "word" to word)
            CapsuleCard(modifier = Modifier.capsuleDescription(announcement)) {
                Column(modifier = Modifier.padding(BlockSpacing / 2)) {
                    Text(
                        text = (index + 1).toString(),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Text(text = word, style = MaterialTheme.typography.titleMediumEmphasized)
                }
            }
        }
    }
}

/**
 * The gate.
 *
 * Setup is not complete until three of the words come back correctly, and there is deliberately
 * no way past it: the only other button goes *back* to the words. That is the difference the
 * doc is drawing when it calls this gated rather than advisory — a screen a user can dismiss
 * is a screen that teaches them the passphrase does not matter.
 */
@Suppress("LongParameterList")
@Composable
fun TypeBackGateScreen(
    state: FirstDeviceUiState,
    onAnswerChanged: (Int, String) -> Unit,
    onSubmit: () -> Unit,
    onBackToWords: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val challenge = state.challenge ?: return
    val label = stringResource(R.string.android_enrollment_typeback_word_label)
    Column(
        modifier =
            modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(ScreenPadding)
                .testTag(TYPE_BACK_GATE_TAG),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        Text(
            text = stringResource(R.string.android_enrollment_typeback_title),
            style = MaterialTheme.typography.headlineMediumEmphasized,
        )
        Text(
            text = stringResource(R.string.android_enrollment_typeback_body),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            text = stringResource(R.string.android_enrollment_typeback_gate_note),
            style = MaterialTheme.typography.labelLarge,
        )
        challenge.positions.forEach { position ->
            EnrollmentTextField(
                value = state.answers[position].orEmpty(),
                onValueChange = { onAnswerChanged(position, it) },
                label = label.withArguments("position" to position + 1),
                isError = state.typeBackRejected,
            )
        }
        if (state.typeBackRejected) {
            CapsuleBanner(
                title = stringResource(R.string.android_enrollment_typeback_wrong),
                severity = CapsuleBannerSeverity.Error,
                modifier = Modifier.fillMaxWidth(),
            )
        }
        CapsuleButton(
            onClick = onSubmit,
            label = stringResource(R.string.android_enrollment_typeback_check),
            enabled = state.canSubmitTypeBack,
            modifier = Modifier.fillMaxWidth().testTag(TYPE_BACK_SUBMIT_TAG),
        )
        CapsuleButton(
            onClick = onBackToWords,
            label = stringResource(R.string.android_enrollment_typeback_back_to_words),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

@CapsulePreviews
@Composable
internal fun PassphrasePreview() {
    CapsulePreviewSurface {
        RecoveryPassphraseScreen(
            passphrase = RecoveryPassphrase.generate(Random(1)),
            hasCopied = false,
            onCopy = {},
            onPrint = {},
            onContinue = {},
        )
    }
}

@CapsulePreviews
@Composable
internal fun TypeBackPreview() {
    CapsulePreviewSurface {
        TypeBackGateScreen(
            state =
                FirstDeviceUiState(
                    phase = SetupPhase.TypeBack,
                    passphrase = RecoveryPassphrase.generate(Random(1)),
                    challenge = TypeBackChallenge(persistentListOf(1, 4, 9)),
                ),
            onAnswerChanged = { _, _ -> },
            onSubmit = {},
            onBackToWords = {},
        )
    }
}
