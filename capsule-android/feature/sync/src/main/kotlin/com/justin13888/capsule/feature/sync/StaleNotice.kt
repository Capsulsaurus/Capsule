package com.justin13888.capsule.feature.sync

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.i18n.R
import java.time.Duration

/** Test handle for the two-week staleness warning. */
const val SYNC_STALE_BANNER_TAG: String = "sync.stale"

/** Test handle for the notice shown while the warning is snoozed or off. */
const val SYNC_STALE_MUTED_TAG: String = "sync.stale-muted"

/**
 * The two-week staleness warning, and what the user may do about it.
 *
 * Snoozing and disabling are offered separately and worded separately because they are not
 * degrees of the same thing: snoozing suppresses the warning until a date, disabling opts out
 * of it entirely, and *neither* touches auto-sync. A user who silences a notification has not
 * asked to stop backing up their photos, and this is the surface where that could most easily
 * be gotten wrong.
 */
@Composable
internal fun StaleNotice(
    state: SyncCentreUiState,
    actions: SyncCentreActions,
) {
    when {
        state.showsStaleWarning ->
            CapsuleBanner(
                title =
                    stringResource(R.string.android_sync_stale_title).withArguments("days" to state.behindDays),
                modifier = Modifier.fillMaxWidth().testTag(SYNC_STALE_BANNER_TAG),
                body = stringResource(R.string.android_sync_stale_body),
                severity = CapsuleBannerSeverity.Warning,
                action = {
                    CapsuleButton(
                        onClick = actions.onForceSync,
                        label = stringResource(R.string.android_sync_force_sync),
                        emphasis = CapsuleButtonEmphasis.Secondary,
                    )
                },
            )

        state.snoozedUntil != null ->
            MutedNotice(
                text =
                    stringResource(R.string.android_sync_stale_snoozed)
                        .withArguments("date" to formattedInstant(requireNotNull(state.snoozedUntil))),
                actionLabel = stringResource(R.string.android_sync_stale_reenable),
                onAction = actions.onEnableStale,
            )

        state.isStaleWarningDisabled ->
            MutedNotice(
                text = stringResource(R.string.android_sync_stale_disabled),
                actionLabel = stringResource(R.string.android_sync_stale_reenable),
                onAction = actions.onEnableStale,
            )

        else -> return
    }
    if (state.showsStaleWarning) {
        StaleChoices(actions = actions)
    }
}

@Composable
private fun StaleChoices(actions: SyncCentreActions) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(RowSpacing),
    ) {
        CapsuleButton(
            onClick = { actions.onSnoozeStale(SHORT_SNOOZE) },
            label = stringResource(R.string.android_sync_stale_snooze_day),
            emphasis = CapsuleButtonEmphasis.Tertiary,
        )
        CapsuleButton(
            onClick = { actions.onSnoozeStale(LONG_SNOOZE) },
            label = stringResource(R.string.android_sync_stale_snooze_week),
            emphasis = CapsuleButtonEmphasis.Tertiary,
        )
        CapsuleButton(
            onClick = { actions.onSnoozeStale(null) },
            label = stringResource(R.string.android_sync_stale_disable),
            emphasis = CapsuleButtonEmphasis.Tertiary,
        )
    }
}

@Composable
private fun MutedNotice(
    text: String,
    actionLabel: String,
    onAction: () -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxWidth().testTag(SYNC_STALE_MUTED_TAG),
        verticalArrangement = Arrangement.spacedBy(RowSpacing),
    ) {
        CapsuleBanner(
            title = text,
            modifier = Modifier.fillMaxWidth(),
            severity = CapsuleBannerSeverity.Info,
            action = {
                CapsuleButton(
                    onClick = onAction,
                    label = actionLabel,
                    emphasis = CapsuleButtonEmphasis.Tertiary,
                )
            },
        )
    }
}

/** A banner over content that is still true — a refused force-sync, not a broken screen. */
@Composable
internal fun SyncFailureBanner(
    state: SyncCentreUiState,
    onRetry: () -> Unit,
) {
    CapsuleBanner(
        title = stringResource(R.string.android_sync_force_sync_failed),
        modifier = Modifier.fillMaxWidth(),
        body = localizedErrorMessage(state.failure),
        severity = CapsuleBannerSeverity.Error,
        action = {
            CapsuleButton(
                onClick = onRetry,
                label = stringResource(R.string.android_sync_retry),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

/** The shorter of the two snoozes the design names. */
private val SHORT_SNOOZE: Duration = Duration.ofHours(24)

/** The longer of the two. */
private val LONG_SNOOZE: Duration = Duration.ofDays(7)
