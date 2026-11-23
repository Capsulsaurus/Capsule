package com.justin13888.capsule.feature.recovery

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R

/** Test handle for the whole re-wrap flow. */
const val RECOVERY_REWRAP_TAG: String = "recovery.rewrap"

/** Test handle for the revealed phrase. */
const val RECOVERY_REWRAP_SECRET_TAG: String = "recovery.rewrap.secret"

/** Test handle for the type-back field. */
const val RECOVERY_REWRAP_TYPEBACK_TAG: String = "recovery.rewrap.typeback"

/** Test handle for the finished state. */
const val RECOVERY_REWRAP_DONE_TAG: String = "recovery.rewrap.done"

/** Test handle for the failure state. */
const val RECOVERY_REWRAP_ERROR_TAG: String = "recovery.rewrap.error"

/**
 * What is left to do once the flow has ended, either way.
 *
 * Two callbacks rather than two parameters because they belong to the same moment — the flow
 * is over — and because the screen's own five arguments are already the ones that drive it.
 */
@Immutable
class RecoveryRewrapEndings(
    /** Opens guardian-share setup, offered once old shares are invalidated. */
    val onReissueShares: () -> Unit,
    /** Starts over after a failure. */
    val onReset: () -> Unit,
)

/**
 * Minting a recovery secret the user actually holds, and re-wrapping the same master key
 * under it.
 *
 * Stateless, so every step is reachable from a preview and a test without a view model. The
 * first screen is an explanation rather than an action because the word "re-wrap" invites
 * exactly the wrong assumption — that photos are about to be re-encrypted — and a user who
 * believes that will not run it on the aeroplane wifi where they finally have time to.
 *
 * @param state everything to draw.
 * @param onBegin mints the secret and shows it.
 * @param onWrittenDown leaves the reveal for the type-back gate.
 * @param onTypedChange edits the current answer.
 * @param onConfirm checks it, and runs the re-wrap once every word is answered.
 * @param onAfter what to do once the flow has ended: re-issue shares, or start over.
 */
@Composable
fun RecoveryRewrapScreen(
    state: RecoveryRewrapUiState,
    onBegin: () -> Unit,
    onWrittenDown: () -> Unit,
    onTypedChange: (String) -> Unit,
    onConfirm: () -> Unit,
    onAfter: RecoveryRewrapEndings,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(RECOVERY_REWRAP_TAG)) {
        CapsuleTopAppBar(
            title = stringResource(R.string.android_destination_recovery_rewrap),
            subtitle = stringResource(state.stage.labelRes()),
        )
        Column(
            modifier = Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(ScreenInset),
            verticalArrangement = Arrangement.spacedBy(ScreenSpacing),
        ) {
            when (state.stage) {
                RewrapStage.Explaining -> RewrapExplanation(onBegin)
                RewrapStage.Revealing -> RewrapReveal(state, onWrittenDown)
                RewrapStage.TypingBack -> RewrapTypeBack(state, onTypedChange, onConfirm)
                RewrapStage.Working -> RewrapWorking()
                RewrapStage.Finished -> RewrapFinished(state, onAfter.onReissueShares)
                RewrapStage.Failed -> RewrapFailed(state, onAfter.onReset)
            }
        }
    }
}

/** The catalogue row naming one step. */
private fun RewrapStage.labelRes(): Int =
    when (this) {
        RewrapStage.Explaining -> R.string.android_recovery_rewrap_step_explain
        RewrapStage.Revealing -> R.string.android_recovery_rewrap_step_secret
        RewrapStage.TypingBack -> R.string.android_recovery_rewrap_step_typeback
        RewrapStage.Working, RewrapStage.Finished, RewrapStage.Failed -> R.string.android_recovery_rewrap_step_done
    }

@Composable
private fun RewrapExplanation(onBegin: () -> Unit) {
    CapsuleBanner(
        title = stringResource(R.string.android_recovery_rewrap_explain_title),
        modifier = Modifier.fillMaxWidth(),
        body = stringResource(R.string.android_recovery_rewrap_explain_body),
        severity = CapsuleBannerSeverity.Info,
    )
    Text(
        text = stringResource(R.string.android_recovery_rewrap_artifact_notice),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurface,
    )
    CapsuleButton(onClick = onBegin, label = stringResource(R.string.android_recovery_rewrap_begin))
}

/**
 * The phrase, once.
 *
 * Each word carries its own position *in the text*, not merely in its visual position,
 * because a screen reader reading twenty-four bare words as one run gives a listener no way
 * to write down word nineteen — and word nineteen is exactly what the next step asks for.
 */
@Composable
private fun RewrapReveal(
    state: RecoveryRewrapUiState,
    onWrittenDown: () -> Unit,
) {
    val secret = state.secret ?: return
    Text(
        text =
            stringResource(R.string.android_recovery_rewrap_secret_caption)
                .withArguments("count" to secret.words.size),
        style = MaterialTheme.typography.bodyMedium,
    )
    val wordTemplate = stringResource(R.string.android_recovery_rewrap_word)
    CapsuleCard(modifier = Modifier.fillMaxWidth().testTag(RECOVERY_REWRAP_SECRET_TAG)) {
        secret.words.forEachIndexed { index, word ->
            Text(
                text = wordTemplate.withArguments("position" to index + 1, "word" to word),
                style = MaterialTheme.typography.bodyLarge,
            )
        }
    }
    CapsuleButton(onClick = onWrittenDown, label = stringResource(R.string.android_recovery_rewrap_secret_ready))
}

@Composable
private fun RewrapTypeBack(
    state: RecoveryRewrapUiState,
    onTypedChange: (String) -> Unit,
    onConfirm: () -> Unit,
) {
    val position = state.currentChallenge ?: return
    Text(
        text = stringResource(R.string.android_recovery_rewrap_typeback_prompt).withArguments("position" to position),
        style = MaterialTheme.typography.bodyMedium,
    )
    OutlinedTextField(
        value = state.typed,
        onValueChange = onTypedChange,
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_REWRAP_TYPEBACK_TAG),
        label = {
            Text(
                text =
                    stringResource(R.string.android_recovery_rewrap_typeback_label)
                        .withArguments("position" to position),
            )
        },
        supportingText = {
            if (state.typedWrong) {
                Text(
                    text =
                        stringResource(R.string.android_recovery_rewrap_typeback_wrong)
                            .withArguments("position" to position),
                )
            }
        },
        isError = state.typedWrong,
        singleLine = true,
        colors = capsuleFieldColors(),
    )
    CapsuleButton(
        onClick = onConfirm,
        label = stringResource(R.string.android_recovery_rewrap_confirm),
        enabled = state.canConfirm,
    )
}

@Composable
private fun RewrapWorking() {
    CapsuleLoadingState(
        modifier = Modifier.fillMaxWidth(),
        title = stringResource(R.string.android_recovery_rewrap_working),
        contentDescription = stringResource(R.string.android_recovery_rewrap_working),
    )
}

/**
 * What is left to deal with.
 *
 * The two loose ends are stated even though neither is an error: guardian shares minted under
 * the old secret now rebuild nothing, and backup archives exported before today still open
 * with the old phrase. Both are consequences of the escrow being singular, which is the
 * property that makes the old secret stop working at all.
 */
@Composable
private fun RewrapFinished(
    state: RecoveryRewrapUiState,
    onReissueShares: () -> Unit,
) {
    CapsuleBanner(
        title = stringResource(R.string.android_recovery_rewrap_done_title),
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_REWRAP_DONE_TAG),
        body = stringResource(R.string.android_recovery_rewrap_done_body),
        severity = CapsuleBannerSeverity.Info,
    )
    if (state.sharesInvalidated) {
        CapsuleSectionHeader(title = stringResource(R.string.android_recovery_action_shamir))
        Text(
            text = stringResource(R.string.android_recovery_rewrap_shares_notice),
            style = MaterialTheme.typography.bodyMedium,
        )
        CapsuleButton(
            onClick = onReissueShares,
            label = stringResource(R.string.android_recovery_rewrap_shares_action),
            emphasis = CapsuleButtonEmphasis.Secondary,
        )
    }
    Text(
        text = stringResource(R.string.android_recovery_rewrap_artifact_notice),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurface,
    )
}

@Composable
private fun RewrapFailed(
    state: RecoveryRewrapUiState,
    onReset: () -> Unit,
) {
    CapsuleErrorState(
        title = stringResource(R.string.android_recovery_rewrap_error_title),
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_REWRAP_ERROR_TAG),
        body = localizedErrorMessage(state.failure?.code),
        action = {
            CapsuleButton(
                onClick = onReset,
                label = stringResource(R.string.android_recovery_retry),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}
