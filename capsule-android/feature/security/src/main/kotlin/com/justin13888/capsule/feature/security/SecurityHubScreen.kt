package com.justin13888.capsule.feature.security

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
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R

/**
 * The one door to everything that protects the account.
 *
 * Reads from both view models rather than owning a merged state: the two halves answer
 * different questions of different authorities, and merging them would put the session ledger
 * behind whatever the credential surface is doing.
 */
@Composable
fun SecurityHubScreen(
    sessions: SessionsUiState,
    credentials: CredentialsUiState,
    totp: TotpActions,
    onOpenSessions: () -> Unit,
    onOpenPasskeys: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(SECURITY_HUB_TAG)) {
        CapsuleTopAppBar(
            title = stringResource(R.string.security_title),
            subtitle = stringResource(R.string.android_security_hub_subtitle),
        )
        Column(
            modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(BlockSpacing),
        ) {
            SessionsRow(sessions, onOpenSessions)
            PasskeysRow(credentials, onOpenPasskeys)
            TotpSection(credentials, totp)
        }
    }
}

@Composable
private fun SessionsRow(
    state: SessionsUiState,
    onOpen: () -> Unit,
) {
    CapsuleListItem(
        headline = stringResource(R.string.android_security_sessions_row),
        modifier = Modifier.fillMaxWidth(),
        supporting =
            if (state.isLoading) {
                stringResource(R.string.security_sessions_loading)
            } else {
                stringResource(R.string.android_security_sessions_row_body).withArguments(
                    "sessions" to state.sessionCount,
                    "devices" to state.activeDeviceCount,
                )
            },
        onClick = onOpen,
    )
}

@Composable
private fun PasskeysRow(
    state: CredentialsUiState,
    onOpen: () -> Unit,
) {
    CapsuleListItem(
        headline = stringResource(R.string.security_passkeys_title),
        modifier = Modifier.fillMaxWidth(),
        supporting =
            if (state.isLoading) {
                stringResource(R.string.security_passkeys_loading)
            } else {
                stringResource(R.string.android_security_passkeys_row_body)
                    .withArguments("count" to state.passkeys.size)
            },
        onClick = onOpen,
    )
}

/**
 * The authenticator factor, in whichever of its three states it is in.
 *
 * Enrollment is two steps on screen because it is two steps in the contract: the secret is
 * shown, and then a generated code proves the authenticator actually took it. A one-tap
 * "enable" would leave a user behind a factor they never verified.
 */
@Composable
private fun TotpSection(
    state: CredentialsUiState,
    actions: TotpActions,
) {
    Column(
        modifier = Modifier.fillMaxWidth().testTag(SECURITY_TOTP_TAG),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        CapsuleSectionHeader(title = stringResource(R.string.security_totp_title))
        Text(
            text = stringResource(R.string.security_totp_description),
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(horizontal = ScreenInset),
        )
        state.failure?.let { failure ->
            CapsuleBanner(
                title = securityFailureMessage(failure),
                modifier = Modifier.fillMaxWidth().padding(horizontal = ScreenInset),
                severity = CapsuleBannerSeverity.Error,
            )
        }
        when {
            state.isEnrolling -> TotpEnrolling(state, actions)
            state.isTotpActive -> TotpActive(state, actions)
            else -> TotpInactive(actions)
        }
    }
}

@Composable
private fun TotpInactive(actions: TotpActions) {
    SecurityFootNote(stringResource(R.string.android_security_totp_not_enabled))
    CapsuleButton(
        onClick = actions.onBegin,
        label = stringResource(R.string.security_totp_setup_button),
        modifier = Modifier.padding(horizontal = ScreenInset),
        emphasis = CapsuleButtonEmphasis.Secondary,
    )
}

@Composable
private fun TotpEnrolling(
    state: CredentialsUiState,
    actions: TotpActions,
) {
    SecurityFootNote(stringResource(R.string.android_security_totp_secret))
    Text(
        text = state.totp?.secret.orEmpty(),
        style = MaterialTheme.typography.titleMediumEmphasized,
        modifier = Modifier.padding(horizontal = ScreenInset),
    )
    SecurityFootNote(stringResource(R.string.android_security_totp_secret_body))
    SecurityCodeField(
        value = state.code,
        onValueChange = actions.onCodeChange,
        enabled = !state.isBusy,
        isError = state.failure is SecurityFailure.TotpCode,
    )
    CapsuleButton(
        onClick = actions.onConfirm,
        label = stringResource(R.string.android_security_totp_confirm),
        modifier = Modifier.padding(horizontal = ScreenInset),
        enabled = !state.isBusy,
    )
    CapsuleButton(
        onClick = actions.onCancel,
        label = stringResource(R.string.android_security_totp_cancel),
        modifier = Modifier.padding(horizontal = ScreenInset),
        emphasis = CapsuleButtonEmphasis.Tertiary,
        enabled = !state.isBusy,
    )
}

@Composable
private fun TotpActive(
    state: CredentialsUiState,
    actions: TotpActions,
) {
    SecurityFootNote(stringResource(R.string.security_totp_enabled))
    SecurityFootNote(stringResource(R.string.security_totp_disable_prompt))
    SecurityCodeField(
        value = state.code,
        onValueChange = actions.onCodeChange,
        enabled = !state.isBusy,
        isError = state.failure is SecurityFailure.TotpCode,
    )
    CapsuleButton(
        onClick = actions.onDisable,
        label =
            if (state.isBusy) {
                stringResource(R.string.security_totp_disabling)
            } else {
                stringResource(R.string.security_totp_disable_button)
            },
        modifier = Modifier.padding(horizontal = ScreenInset),
        emphasis = CapsuleButtonEmphasis.Destructive,
        enabled = !state.isBusy,
    )
}
