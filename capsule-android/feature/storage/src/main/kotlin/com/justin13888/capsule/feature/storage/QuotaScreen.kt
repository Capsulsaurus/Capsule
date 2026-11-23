package com.justin13888.capsule.feature.storage

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
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleProgress
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.QuotaState
import java.time.Duration
import kotlin.math.roundToInt

private val ScreenInset = 16.dp
private val BlockSpacing = 12.dp

/**
 * What the account is allowed, and what to do about where it stands.
 *
 * Stateless, so all five rungs of the ladder are reachable from a preview and a test without
 * a view model — which matters more here than anywhere else in the feature, because four of
 * the five are states a healthy account never reaches by itself.
 *
 * @param state the current reading.
 * @param onReviewLargest scrolls the user to what is actually taking room.
 * @param onOpenTrash opens the trash, which accepts deletes in every state including the two
 *   that refuse everything else.
 * @param onContactSupport the only path out of an administrative suspension.
 * @param onRetry re-reads after a failure.
 */
@Composable
fun QuotaScreen(
    state: StorageUiState,
    onReviewLargest: () -> Unit,
    onOpenTrash: () -> Unit,
    onContactSupport: () -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize()) {
        CapsuleTopAppBar(
            title = stringResource(R.string.android_destination_quota),
            subtitle = stringResource(R.string.android_storage_subtitle),
        )
        val standing = state.standing
        when {
            state.isLoading || standing == null ->
                CapsuleLoadingState(
                    modifier = Modifier.fillMaxWidth().testTag(STORAGE_LOADING_TAG),
                    title = stringResource(R.string.android_storage_loading),
                    contentDescription = stringResource(R.string.android_storage_loading),
                )

            state.failure != null -> StorageFailureState(state.failure, onRetry)
            else ->
                QuotaBody(
                    standing = standing,
                    onReviewLargest = onReviewLargest,
                    onOpenTrash = onOpenTrash,
                    onContactSupport = onContactSupport,
                )
        }
    }
}

@Composable
private fun QuotaBody(
    standing: QuotaStanding,
    onReviewLargest: () -> Unit,
    onOpenTrash: () -> Unit,
    onContactSupport: () -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(bottom = ScreenInset),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        CapsuleSectionHeader(title = stringResource(R.string.android_storage_quota_section))
        QuotaUsage(standing)
        QuotaStateCard(standing, onReviewLargest, onOpenTrash, onContactSupport)
        // Stated on every state, not only the ones where it is in doubt: the states where a
        // user most needs to know deleting still works are the ones that look like a lockout.
        if (standing.allowsRecoveryWrites) {
            Text(
                text = stringResource(R.string.android_storage_deletes_still_work),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = ScreenInset).testTag(QUOTA_DELETES_TAG),
            )
        }
    }
}

/**
 * The usage bar and the figure under it.
 *
 * An unlimited deployment gets a sentence instead of a bar: a progress bar against no ceiling
 * would have to invent one, and a bar that is always nearly empty reads as "you have almost
 * nothing" rather than "there is no limit here".
 */
@Composable
private fun QuotaUsage(standing: QuotaStanding) {
    Column(
        modifier = Modifier.fillMaxWidth().padding(horizontal = ScreenInset),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing / 2),
    ) {
        val used = formattedBytes(standing.used)
        if (standing.hardLimit == null) {
            Text(
                text = stringResource(R.string.android_storage_quota_usage_unlimited).withArguments("used" to used),
                style = MaterialTheme.typography.bodyLarge,
            )
        } else {
            val percent = (standing.fraction * PERCENT).roundToInt()
            CapsuleProgress(
                progress = { standing.fraction },
                modifier = Modifier.fillMaxWidth().testTag(QUOTA_BAR_TAG),
                contentDescription =
                    stringResource(R.string.android_storage_quota_bar_description)
                        .withArguments("percent" to percent),
            )
            Text(
                text =
                    stringResource(R.string.android_storage_quota_usage)
                        .withArguments("used" to used, "total" to formattedBytes(standing.hardLimit)),
                style = MaterialTheme.typography.bodyLarge,
            )
        }
    }
}

/**
 * The state, its explanation, and the one thing to do about it.
 *
 * Every rung gets its own copy rather than a shared "you are over quota" with a severity
 * colour, because the states differ in what still works — and that difference is the only
 * thing the user actually needs from this screen.
 */
@Composable
private fun QuotaStateCard(
    standing: QuotaStanding,
    onReviewLargest: () -> Unit,
    onOpenTrash: () -> Unit,
    onContactSupport: () -> Unit,
) {
    CapsuleBanner(
        title = stringResource(standing.state.titleRes()),
        modifier = Modifier.fillMaxWidth().padding(horizontal = ScreenInset).testTag(QUOTA_STATE_TAG),
        body = quotaBody(standing),
        severity = standing.state.severity(),
        action = { QuotaAction(standing, onReviewLargest, onOpenTrash, onContactSupport) },
    )
}

/**
 * The body, with the grace countdown appended where one is running.
 *
 * The countdown belongs to the hard-exceeded state alone: before the hard limit nothing is
 * counting, and once the window has run out there is nothing left to count.
 */
@Composable
private fun quotaBody(standing: QuotaStanding): String {
    val base = stringResource(standing.state.bodyRes())
    val remaining = standing.graceRemaining ?: return base
    val countdown =
        if (remaining < Duration.ofDays(1)) {
            stringResource(R.string.android_storage_grace_last_day)
        } else {
            stringResource(R.string.android_storage_grace_remaining)
                .withArguments("days" to remaining.toDays())
        }
    return base + PARAGRAPH + countdown
}

@Composable
private fun QuotaAction(
    standing: QuotaStanding,
    onReviewLargest: () -> Unit,
    onOpenTrash: () -> Unit,
    onContactSupport: () -> Unit,
) {
    when (standing.remediation) {
        QuotaRemediation.None -> Unit

        QuotaRemediation.ReviewLargest ->
            CapsuleButton(
                onClick = onReviewLargest,
                label = stringResource(R.string.android_storage_action_review_largest),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )

        QuotaRemediation.FreeSpace ->
            CapsuleButton(
                onClick = onReviewLargest,
                label = stringResource(R.string.android_storage_action_free_space),
                emphasis = CapsuleButtonEmphasis.Primary,
            )

        // Deletes and restore-from-trash are still admitted here by design, so the way out is
        // foregrounded rather than the wall being described again.
        QuotaRemediation.FreeSpaceToResumeEdits ->
            CapsuleButton(
                onClick = onOpenTrash,
                label = stringResource(R.string.android_storage_action_empty_trash),
                emphasis = CapsuleButtonEmphasis.Primary,
            )

        QuotaRemediation.ContactSupport ->
            CapsuleButton(
                onClick = onContactSupport,
                label = stringResource(R.string.android_storage_action_contact_support),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
    }
}

/** The failure state both storage destinations share. */
@Composable
internal fun StorageFailureState(
    failure: StorageFailure,
    onRetry: () -> Unit,
) {
    CapsuleErrorState(
        title = stringResource(R.string.android_storage_error_title),
        modifier = Modifier.fillMaxWidth().testTag(STORAGE_ERROR_TAG),
        // The localized `error.*` code, never a status: the code is the contract and a status
        // is a transport detail the user can do nothing with.
        body = localizedErrorMessage(failure.code),
        action = {
            CapsuleButton(
                onClick = onRetry,
                label = stringResource(R.string.android_storage_retry),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

private fun QuotaState.titleRes(): Int =
    when (this) {
        QuotaState.Ok -> R.string.android_storage_state_ok_title
        QuotaState.SoftWarning -> R.string.android_storage_state_soft_title
        QuotaState.HardExceeded -> R.string.android_storage_state_hard_title
        QuotaState.GraceExpired -> R.string.android_storage_state_grace_title
        QuotaState.Suspended -> R.string.android_storage_state_suspended_title
    }

private fun QuotaState.bodyRes(): Int =
    when (this) {
        QuotaState.Ok -> R.string.android_storage_state_ok_body
        QuotaState.SoftWarning -> R.string.android_storage_state_soft_body
        QuotaState.HardExceeded -> R.string.android_storage_state_hard_body
        QuotaState.GraceExpired -> R.string.android_storage_state_grace_body
        QuotaState.Suspended -> R.string.android_storage_state_suspended_body
    }

/**
 * How loudly a state is allowed to speak.
 *
 * The two full states are warnings rather than errors on purpose: nothing has failed, and an
 * error tone on a state the user can walk out of teaches them to ignore the tone. Only an
 * administrative suspension — the one they cannot resolve themselves — gets the error voice.
 */
private fun QuotaState.severity(): CapsuleBannerSeverity =
    when (this) {
        QuotaState.Ok -> CapsuleBannerSeverity.Info
        QuotaState.SoftWarning -> CapsuleBannerSeverity.Warning
        QuotaState.HardExceeded -> CapsuleBannerSeverity.Warning
        QuotaState.GraceExpired -> CapsuleBannerSeverity.Degraded
        QuotaState.Suspended -> CapsuleBannerSeverity.Error
    }

private const val PERCENT = 100f
private const val PARAGRAPH = "\n\n"
