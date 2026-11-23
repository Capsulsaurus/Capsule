package com.justin13888.capsule.feature.security

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
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
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R

/**
 * The passkeys registered against the account.
 *
 * Stateless. Deleting the last one is warned about and then allowed: the account key still
 * opens the account, and a client that vetoed the removal of a credential the user owns would
 * be deciding something that is theirs to decide.
 */
@Composable
fun PasskeysScreen(
    state: CredentialsUiState,
    onAddPasskey: () -> Unit,
    onDeletePasskey: (PasskeyId) -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(SECURITY_PASSKEYS_TAG)) {
        CapsuleTopAppBar(
            title = stringResource(R.string.security_passkeys_title),
            subtitle = stringResource(R.string.security_passkeys_description),
        )
        when {
            state.isLoading -> SecurityLoadingState()
            state.failure != null && state.passkeys.isEmpty() ->
                SecurityFailureState(state.failure, onRetry)

            else -> PasskeysBody(state, onAddPasskey, onDeletePasskey)
        }
    }
}

@Composable
private fun PasskeysBody(
    state: CredentialsUiState,
    onAddPasskey: () -> Unit,
    onDeletePasskey: (PasskeyId) -> Unit,
) {
    LazyColumn(modifier = Modifier.fillMaxSize()) {
        state.failure?.let { failure ->
            item(key = FAILURE_KEY) {
                CapsuleBanner(
                    title = stringResource(R.string.android_security_passkey_add_failed),
                    modifier = Modifier.fillMaxWidth().padding(ScreenInset),
                    body = securityFailureMessage(failure),
                    severity = CapsuleBannerSeverity.Error,
                )
            }
        }
        if (state.hasNoPasskeys) {
            item(key = EMPTY_KEY) {
                CapsuleEmptyState(
                    title = stringResource(R.string.security_passkeys_empty),
                    modifier = Modifier.fillMaxWidth().testTag(SECURITY_EMPTY_TAG),
                    body = stringResource(R.string.security_passkeys_description),
                )
            }
        }
        items(items = state.passkeys, key = { passkey -> passkey.id.value }) { passkey ->
            PasskeyRow(passkey, state.isBusy, onDeletePasskey)
        }
        if (state.isLastFactor) {
            item(key = LAST_FACTOR_KEY) {
                CapsuleBanner(
                    title = stringResource(R.string.android_security_passkey_last_warning),
                    modifier = Modifier.fillMaxWidth().padding(ScreenInset),
                    severity = CapsuleBannerSeverity.Warning,
                )
            }
        }
        item(key = ADD_KEY) {
            CapsuleButton(
                onClick = onAddPasskey,
                label = stringResource(R.string.security_passkeys_add_button),
                modifier = Modifier.padding(ScreenInset),
                enabled = !state.isBusy,
            )
        }
    }
}

@Composable
private fun PasskeyRow(
    passkey: Passkey,
    isBusy: Boolean,
    onDelete: (PasskeyId) -> Unit,
) {
    val added = stringResource(R.string.security_added).withArguments("date" to formattedDate(passkey.addedAt))
    val used =
        passkey.lastUsedAt
            ?.let { stringResource(R.string.security_last_active).withArguments("date" to formattedDate(it)) }
            ?: stringResource(R.string.android_security_passkey_never_used)
    CapsuleListItem(
        headline = passkey.label,
        modifier = Modifier.fillMaxWidth().testTag(securityPasskeyTag(passkey.id.value)),
        supporting = added + SEPARATOR + used,
        trailing = {
            CapsuleButton(
                onClick = { onDelete(passkey.id) },
                label =
                    if (isBusy) {
                        stringResource(R.string.security_deleting)
                    } else {
                        stringResource(R.string.common_delete)
                    },
                emphasis = CapsuleButtonEmphasis.Tertiary,
                enabled = !isBusy,
                contentDescription = stringResource(R.string.android_security_passkey_delete),
            )
        },
    )
}

private const val FAILURE_KEY = "security.passkey-failure"
private const val EMPTY_KEY = "security.passkey-empty"
private const val ADD_KEY = "security.passkey-add"
private const val LAST_FACTOR_KEY = "security.passkey-last-factor"
