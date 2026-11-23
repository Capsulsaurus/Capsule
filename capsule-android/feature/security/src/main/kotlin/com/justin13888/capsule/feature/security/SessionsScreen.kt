package com.justin13888.capsule.feature.security

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleSheet
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.Session
import com.justin13888.capsule.core.model.SessionId

/**
 * Where the account is signed in, grouped by the device each session appears to come from.
 *
 * Stateless. The grouping is a reading aid and the screen says so once, plainly, rather than
 * offering the user a control over it: the hash is client-asserted and unverifiable, so a
 * "this isn't my device" switch would record an opinion nothing could act on. The dispute path
 * is one tap that produces a report a person can act on instead.
 */
@Composable
fun SessionsScreen(
    state: SessionsUiState,
    actions: SessionsActions,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(SECURITY_SESSIONS_TAG)) {
        CapsuleTopAppBar(
            title = stringResource(R.string.security_sessions_title),
            subtitle = stringResource(R.string.android_security_sessions_subtitle),
        )
        when {
            state.isLoading -> SecurityLoadingState()
            state.failure != null -> SecurityFailureState(state.failure, actions.onRetry)
            else -> SessionsBody(state, actions)
        }
    }
    if (state.revokeAll != RevokeAllState.Idle) {
        RevokeAllSheet(state.revokeAll, actions)
    }
    state.supportReport?.let { report -> SupportReportSheet(report, actions.onDismissReport) }
}

@Composable
private fun SessionsBody(
    state: SessionsUiState,
    actions: SessionsActions,
) {
    LazyColumn(modifier = Modifier.fillMaxSize().testTag(SECURITY_LIST_TAG)) {
        item(key = ADVISORY_KEY) {
            SecurityFootNote(
                text = stringResource(R.string.android_security_cohort_advisory),
                modifier = Modifier.testTag(SECURITY_ADVISORY_TAG),
            )
        }
        item(key = SESSIONS_HEADER_KEY) {
            CapsuleSectionHeader(title = stringResource(R.string.android_security_sessions_section))
        }
        if (state.hasNoSessions) {
            item(key = SESSIONS_EMPTY_KEY) {
                CapsuleEmptyState(
                    title = stringResource(R.string.security_sessions_empty),
                    modifier = Modifier.fillMaxWidth().testTag(SECURITY_EMPTY_TAG),
                )
            }
        }
        state.groups.forEach { group ->
            item(key = COHORT_KEY_PREFIX + (group.cohortHash ?: NO_COHORT)) {
                CohortHeader(group, actions.onReportCohort)
            }
            items(count = group.sessions.size, key = { index -> group.sessions[index].id.value }) { index ->
                SessionRow(group.sessions[index], actions.onRevokeSession)
            }
        }
        item(key = REVOKE_ALL_KEY) { RevokeAllRow(state, actions.onAskRevokeAll) }
        item(key = DEVICES_HEADER_KEY) {
            CapsuleSectionHeader(title = stringResource(R.string.android_security_devices_section))
        }
        if (state.devices.isEmpty()) {
            item(key = DEVICES_EMPTY_KEY) {
                CapsuleEmptyState(title = stringResource(R.string.android_security_devices_empty))
            }
        }
        items(count = state.devices.size, key = { index -> state.devices[index].id.value }) { index ->
            DeviceRow(state.devices[index], actions.onRevokeDevice)
        }
        item(key = DEVICES_NOTE_KEY) {
            SecurityFootNote(stringResource(R.string.android_security_device_retired_note))
        }
    }
}

/**
 * One cohort's heading, and the dispute path beside it.
 *
 * The report action sits on the group rather than on a session, because the thing the user
 * does not recognise is the *device*, and a report about one of its four sessions would be a
 * quarter of the answer.
 */
@Composable
private fun CohortHeader(
    group: SessionCohortGroup,
    onReport: (String?) -> Unit,
) {
    val headline = cohortHeadline(group)
    Column(
        modifier = Modifier.fillMaxWidth().testTag(securityCohortTag(group.cohortHash)),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        CapsuleListItem(
            headline = headline,
            modifier = Modifier.fillMaxWidth(),
            supporting = cohortSupporting(group),
        )
        if (!group.isCurrent) {
            val label = stringResource(R.string.android_security_support_bundle)
            CapsuleButton(
                onClick = { onReport(group.cohortHash) },
                label = label,
                modifier = Modifier.padding(horizontal = ScreenInset),
                emphasis = CapsuleButtonEmphasis.Tertiary,
                // Named after the group it belongs to. Two identical "report this" buttons on
                // one screen is a list a screen-reader user cannot navigate.
                contentDescription = label + SEPARATOR + headline,
            )
        }
    }
}

@Composable
private fun SessionRow(
    session: Session,
    onRevoke: (SessionId) -> Unit,
) {
    val lastActive = formattedInstant(session.lastActiveAt)
    val headline =
        if (session.isCurrent) {
            stringResource(R.string.android_security_session_current)
        } else {
            session.userAgent ?: stringResource(R.string.security_unknown_device)
        }
    val supporting =
        stringResource(R.string.security_last_active).withArguments("date" to lastActive) +
            SEPARATOR +
            stringResource(R.string.android_security_session_expires)
                .withArguments("date" to formattedDate(session.effectiveExpiry))
    CapsuleListItem(
        headline = headline,
        modifier = Modifier.fillMaxWidth().testTag(securitySessionTag(session.id.value)),
        supporting = supporting,
        overline =
            stringResource(R.string.android_security_session_started)
                .withArguments("date" to formattedDate(session.createdAt)),
        trailing = {
            CapsuleButton(
                onClick = { onRevoke(session.id) },
                label = stringResource(R.string.android_security_revoke),
                emphasis = CapsuleButtonEmphasis.Tertiary,
                // Named after the row it ends. A ledger of identically-announced "Sign out"
                // buttons is one a screen-reader user cannot tell apart.
                contentDescription =
                    stringResource(R.string.android_security_revoke_description)
                        .withArguments("date" to lastActive) + SEPARATOR + headline,
            )
        },
    )
}

@Composable
private fun DeviceRow(
    device: Device,
    onRevoke: (DeviceId) -> Unit,
) {
    val added =
        stringResource(R.string.android_security_device_added)
            .withArguments("date" to formattedDate(device.addedAt))
    val retired =
        device.revokedAt?.let {
            stringResource(R.string.android_security_device_retired).withArguments("date" to formattedDate(it))
        }
    CapsuleListItem(
        headline = device.displayName ?: stringResource(R.string.security_unknown_device),
        modifier = Modifier.fillMaxWidth().testTag(securityDeviceTag(device.id.value)),
        supporting = if (retired == null) added else added + SEPARATOR + retired,
        trailing = {
            if (device.isActive && !device.isThisDevice) {
                CapsuleButton(
                    onClick = { onRevoke(device.id) },
                    label = stringResource(R.string.android_security_revoke_device),
                    emphasis = CapsuleButtonEmphasis.Tertiary,
                )
            }
        },
    )
}

/** The entry point to the global revoke, stated as costing more than the row above it. */
@Composable
private fun RevokeAllRow(
    state: SessionsUiState,
    onAsk: () -> Unit,
) {
    Column(modifier = Modifier.fillMaxWidth().padding(ScreenInset)) {
        CapsuleButton(
            onClick = onAsk,
            label = stringResource(R.string.android_security_revoke_all),
            emphasis = CapsuleButtonEmphasis.Destructive,
            enabled = !state.hasNoSessions,
        )
    }
}

/**
 * The confirmation, which is where the asymmetry is explained.
 *
 * The reason is in the copy rather than in a tooltip: "confirmed with your account key rather
 * than with this session" is the sentence that makes the extra step make sense, and a user who
 * does not see it reads the step as friction.
 */
@Composable
private fun RevokeAllSheet(
    revokeAll: RevokeAllState,
    actions: SessionsActions,
) {
    CapsuleSheet(onDismissRequest = actions.onDismissRevokeAll) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(ScreenInset).testTag(SECURITY_REVOKE_ALL_TAG),
            verticalArrangement = Arrangement.spacedBy(BlockSpacing),
        ) {
            Text(
                text = stringResource(R.string.android_security_revoke_all_title),
                style = MaterialTheme.typography.titleLargeEmphasized,
            )
            Text(
                text = stringResource(R.string.android_security_revoke_all_body),
                style = MaterialTheme.typography.bodyMedium,
            )
            RevokeAllOutcome(revokeAll)
            if (revokeAll != RevokeAllState.Done) {
                CapsuleButton(
                    onClick = actions.onConfirmRevokeAll,
                    label =
                        if (revokeAll == RevokeAllState.Proving) {
                            stringResource(R.string.android_security_revoke_all_working)
                        } else {
                            stringResource(R.string.android_security_revoke_all_confirm)
                        },
                    emphasis = CapsuleButtonEmphasis.Destructive,
                    enabled = revokeAll != RevokeAllState.Proving,
                )
            }
            CapsuleButton(
                onClick = actions.onDismissRevokeAll,
                label = stringResource(R.string.common_close),
                emphasis = CapsuleButtonEmphasis.Tertiary,
            )
        }
    }
}

/** What the global revoke did — including the all-or-nothing refusal. */
@Composable
private fun RevokeAllOutcome(revokeAll: RevokeAllState) {
    when (revokeAll) {
        is RevokeAllState.Done ->
            CapsuleBanner(
                title = stringResource(R.string.android_security_revoke_all_done),
                modifier = Modifier.fillMaxWidth(),
                severity = CapsuleBannerSeverity.Info,
            )

        is RevokeAllState.Refused ->
            CapsuleBanner(
                title = stringResource(R.string.android_security_revoke_all_nothing),
                modifier = Modifier.fillMaxWidth(),
                body = securityFailureMessage(revokeAll.failure),
                severity = CapsuleBannerSeverity.Error,
            )

        else -> Unit
    }
}

/** The dispute path's output: the exact identifiers, and nothing else. */
@Composable
private fun SupportReportSheet(
    report: String,
    onDismiss: () -> Unit,
) {
    CapsuleSheet(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(ScreenInset).testTag(SECURITY_SUPPORT_TAG),
            verticalArrangement = Arrangement.spacedBy(BlockSpacing),
        ) {
            Text(
                text = stringResource(R.string.android_security_support_bundle_title),
                style = MaterialTheme.typography.titleLargeEmphasized,
            )
            Text(
                text = stringResource(R.string.android_security_support_bundle_body),
                style = MaterialTheme.typography.bodyMedium,
            )
            Text(text = report, style = MaterialTheme.typography.bodySmall)
            Text(
                text = stringResource(R.string.android_security_support_bundle_note),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            CapsuleButton(
                onClick = onDismiss,
                label = stringResource(R.string.common_close),
                emphasis = CapsuleButtonEmphasis.Tertiary,
            )
        }
    }
}

private const val ADVISORY_KEY = "security.advisory-note"
private const val SESSIONS_HEADER_KEY = "security.sessions-header"
private const val SESSIONS_EMPTY_KEY = "security.sessions-empty"
private const val DEVICES_HEADER_KEY = "security.devices-header"
private const val DEVICES_EMPTY_KEY = "security.devices-empty"
private const val DEVICES_NOTE_KEY = "security.devices-note"
private const val REVOKE_ALL_KEY = "security.revoke-all-row"
private const val COHORT_KEY_PREFIX = "security.cohort:"
private const val NO_COHORT = "none"
