package com.justin13888.capsule.feature.recovery

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
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
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.RecoveryState

/** Test handle for the whole hub. */
const val RECOVERY_STATUS_TAG: String = "recovery.status"

/** Test handle for the standing card that says where the user stands. */
const val RECOVERY_STANDING_TAG: String = "recovery.standing"

/** Test handle for the loading state. */
const val RECOVERY_STATUS_LOADING_TAG: String = "recovery.status.loading"

/** Test handle for the error state. */
const val RECOVERY_STATUS_ERROR_TAG: String = "recovery.status.error"

/** Test handle for the note that none of this blocks anything. */
const val RECOVERY_ADVISORY_TAG: String = "recovery.advisory"

/**
 * Everything the hub can be asked to do, in one object.
 *
 * Grouped so the screen stays a plain function of state plus intent, and so the five places a
 * user can go from here are one parameter rather than five arguments at every call site.
 */
@Immutable
class RecoveryStatusActions(
    /** Opens the local verification. */
    val onVerify: () -> Unit,
    /** Defers the prompt by 24 hours. */
    val onSnoozeDay: () -> Unit,
    /** Defers it by 7 days. */
    val onSnoozeWeek: () -> Unit,
    /** Opens the guided re-wrap, whether recommended or chosen. */
    val onRewrap: () -> Unit,
    /** Opens guardian-share setup. */
    val onShamir: () -> Unit,
    /** Opens backup export. */
    val onExport: () -> Unit,
    /** Opens backup restore. */
    val onRestore: () -> Unit,
)

/**
 * Where the user stands on being able to get back in, and everything they can do about it.
 *
 * Stateless, so every state it can be in is reachable from a preview and a test without a
 * view model. The advisory note is permanent rather than conditional: this screen exists to
 * nag, and a nag that is not visibly optional reads as a threat.
 *
 * @param state everything to draw.
 * @param actions where the user can go from here.
 * @param onRetry re-reads the state after a failure. Separate from [actions] because it is
 *   this screen recovering itself rather than somewhere the user is going.
 */
@Composable
fun RecoveryStatusScreen(
    state: RecoveryStatusUiState,
    actions: RecoveryStatusActions,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(RECOVERY_STATUS_TAG)) {
        CapsuleTopAppBar(
            title = stringResource(R.string.android_destination_recovery_status),
            subtitle = stringResource(R.string.android_recovery_subtitle),
        )
        when {
            state.isLoading -> RecoveryLoadingState()
            state.failure != null && state.recovery == null -> RecoveryFailureState(state.failure, onRetry)
            else -> RecoveryStanding(state, actions)
        }
    }
}

@Composable
private fun RecoveryLoadingState() {
    CapsuleLoadingState(
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_STATUS_LOADING_TAG),
        title = stringResource(R.string.android_recovery_loading),
        contentDescription = stringResource(R.string.android_recovery_loading),
    )
}

@Composable
private fun RecoveryFailureState(
    failure: RecoveryFailure,
    onRetry: () -> Unit,
) {
    CapsuleErrorState(
        title = stringResource(R.string.android_recovery_error_title),
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_STATUS_ERROR_TAG),
        // The localized `error.*` code, never a status: the code is the contract, and a status
        // is a transport detail the user can do nothing with.
        body = localizedErrorMessage(failure.code),
        action = {
            CapsuleButton(
                onClick = onRetry,
                label = stringResource(R.string.android_recovery_retry),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

@Composable
private fun RecoveryStanding(
    state: RecoveryStatusUiState,
    actions: RecoveryStatusActions,
) {
    Column(
        modifier = Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(ScreenInset),
        verticalArrangement = Arrangement.spacedBy(ScreenSpacing),
    ) {
        StandingBanner(state)
        Text(
            text = stringResource(R.string.android_recovery_advisory),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurface,
            modifier = Modifier.testTag(RECOVERY_ADVISORY_TAG),
        )
        StandingActions(state, actions)
        CapsuleSectionHeader(title = stringResource(R.string.android_destination_recovery_status))
        CapsuleListItem(
            headline = stringResource(R.string.android_recovery_action_shamir),
            onClick = actions.onShamir,
        )
        CapsuleListItem(
            headline = stringResource(R.string.android_recovery_action_export),
            onClick = actions.onExport,
        )
        CapsuleListItem(
            headline = stringResource(R.string.android_recovery_action_restore),
            onClick = actions.onRestore,
        )
        CapsuleListItem(
            headline = stringResource(R.string.android_recovery_action_lost),
            onClick = actions.onRewrap,
        )
    }
}

/**
 * The one sentence that says where the user stands.
 *
 * Severity climbs with the state and stops at `Warning`. Nothing here is an error, because
 * nothing here has gone wrong for the *library* — a recovery secret nobody has re-entered is
 * a risk, not a fault, and dressing it as a fault trains people to dismiss the one banner
 * that will matter later.
 */
@Composable
private fun StandingBanner(state: RecoveryStatusUiState) {
    val current = state.recovery ?: return
    val severity =
        when (current) {
            is RecoveryState.Healthy -> CapsuleBannerSeverity.Info
            is RecoveryState.Snoozed ->
                if (state.isPersistentBadge) CapsuleBannerSeverity.Warning else CapsuleBannerSeverity.Info
            else -> CapsuleBannerSeverity.Warning
        }
    CapsuleBanner(
        title = stringResource(current.titleRes()),
        modifier = Modifier.fillMaxWidth().testTag(RECOVERY_STANDING_TAG),
        body = standingBody(current, state.isPersistentBadge),
        severity = severity,
    )
}

/** The catalogue row naming one standing. */
private fun RecoveryState.titleRes(): Int =
    when (this) {
        is RecoveryState.NotConfigured -> R.string.android_recovery_state_not_configured_title
        is RecoveryState.Healthy -> R.string.android_recovery_state_healthy_title
        is RecoveryState.VerificationDue -> R.string.android_recovery_state_due_title
        is RecoveryState.Snoozed -> R.string.android_recovery_state_snoozed_title
        is RecoveryState.RewrapRecommended -> R.string.android_recovery_state_rewrap_title
    }

/** The supporting line, with the dates and counts this standing turns on. */
@Composable
private fun standingBody(
    current: RecoveryState,
    persistent: Boolean,
): String =
    when (current) {
        is RecoveryState.NotConfigured -> stringResource(R.string.android_recovery_state_not_configured_body)
        is RecoveryState.Healthy ->
            stringResource(R.string.android_recovery_state_healthy_body)
                .withArguments(
                    "date" to formattedDate(current.lastVerifiedAt),
                    "next" to formattedDate(current.nextPromptAt),
                )

        is RecoveryState.VerificationDue ->
            stringResource(R.string.android_recovery_state_due_body)
                .withArguments("date" to formattedDate(current.since))

        is RecoveryState.Snoozed -> {
            val base =
                stringResource(R.string.android_recovery_state_snoozed_body)
                    .withArguments("date" to formattedDate(current.until))
            val note = stringResource(R.string.android_recovery_state_snoozed_persistent)
            if (persistent) base + SEPARATOR + note else base
        }

        is RecoveryState.RewrapRecommended ->
            stringResource(R.string.android_recovery_state_rewrap_body)
                .withArguments("count" to current.consecutiveFailures)
    }

/**
 * The actions this standing calls for, most useful first.
 *
 * Snoozing disappears once the cap is reached rather than staying and doing nothing, and the
 * re-wrap is promoted to the primary action only when repeated failures say the user probably
 * no longer holds the secret.
 */
@Composable
private fun StandingActions(
    state: RecoveryStatusUiState,
    actions: RecoveryStatusActions,
) {
    if (state.rewrapRecommended) {
        CapsuleButton(onClick = actions.onRewrap, label = stringResource(R.string.android_recovery_action_rewrap))
    }
    CapsuleButton(
        onClick = actions.onVerify,
        label = stringResource(R.string.android_recovery_action_verify),
        emphasis = if (state.rewrapRecommended) CapsuleButtonEmphasis.Secondary else CapsuleButtonEmphasis.Primary,
        enabled = !state.isNotConfigured,
    )
    if (state.canSnooze) {
        CapsuleButton(
            onClick = actions.onSnoozeDay,
            label = stringResource(R.string.android_recovery_action_snooze_day),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            enabled = !state.snoozing,
        )
        CapsuleButton(
            onClick = actions.onSnoozeWeek,
            label = stringResource(R.string.android_recovery_action_snooze_week),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            enabled = !state.snoozing,
        )
    }
}
