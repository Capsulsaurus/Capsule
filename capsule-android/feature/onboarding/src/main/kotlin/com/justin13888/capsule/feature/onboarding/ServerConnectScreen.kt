package com.justin13888.capsule.feature.onboarding

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleChip
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviews
import com.justin13888.capsule.core.i18n.R
import kotlinx.collections.immutable.persistentListOf

/** Test handle for the server-info card, which must appear before any credential is asked for. */
const val SERVER_INFO_CARD_TAG: String = "onboarding.server-info"

/** Test handle for the auth path picker. */
const val AUTH_PICKER_TAG: String = "onboarding.auth-picker"

private val ScreenPadding = 24.dp
private val BlockSpacing = 16.dp

/**
 * Naming the server, and reading what it says about itself before trusting it with anything.
 *
 * The server-info card is shown *before* the auth picker on purpose: `server-info` is public
 * and user-list-free by contract, so it is the one thing a user can look at while still
 * anonymous, and it is what tells them whether this deployment is the one they meant.
 */
@Suppress("LongParameterList")
@Composable
fun ServerConnectScreen(
    state: ServerUiState,
    onAddressChanged: (String) -> Unit,
    onConnect: () -> Unit,
    onSelectMethod: (AuthMethod) -> Unit,
    onContinue: () -> Unit,
    onUseLocalOnly: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(ScreenPadding),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        Text(
            text = stringResource(R.string.android_onboarding_server_title),
            style = MaterialTheme.typography.headlineMediumEmphasized,
        )
        Text(
            text = stringResource(R.string.android_onboarding_server_body),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        AddressField(state = state, onAddressChanged = onAddressChanged)
        CapsuleButton(
            onClick = onConnect,
            label = stringResource(R.string.android_onboarding_server_connect),
            enabled = !state.isProbing && state.address.isNotBlank(),
            modifier = Modifier.fillMaxWidth(),
        )
        when {
            state.isProbing ->
                CapsuleLoadingState(
                    title = stringResource(R.string.android_onboarding_server_probing),
                    contentDescription = stringResource(R.string.android_onboarding_server_probing),
                )

            state.failure != null -> Unreachable(state.failure, onConnect, onUseLocalOnly)
            state.hasNoUsableMethod -> NoUsableMethod(onUseLocalOnly)
            state.info != null -> Answered(state, state.info, onSelectMethod, onContinue)
        }
    }
}

@Composable
private fun AddressField(
    state: ServerUiState,
    onAddressChanged: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    OnboardingTextField(
        value = state.address,
        onValueChange = onAddressChanged,
        labelRes = R.string.android_onboarding_server_url_label,
        modifier = modifier,
        placeholderRes = R.string.android_onboarding_server_url_placeholder,
        supportingRes = R.string.android_onboarding_server_url_invalid.takeIf { state.isAddressInvalid },
        isError = state.isAddressInvalid,
        keyboardOptions =
            KeyboardOptions(
                keyboardType = KeyboardType.Uri,
                capitalization = KeyboardCapitalization.None,
            ),
    )
}

@Composable
private fun Unreachable(
    failure: OnboardingFailure,
    onRetry: () -> Unit,
    onUseLocalOnly: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(BlockSpacing)) {
        CapsuleErrorState(
            title = stringResource(R.string.android_onboarding_server_unreachable_title),
            body = localizedErrorMessage(failure.code),
            action = {
                CapsuleButton(onClick = onRetry, label = stringResource(R.string.android_onboarding_server_retry))
            },
        )
        // The local library is a real answer to a server that will not answer, not a
        // consolation prize: nothing typed so far is lost by taking it.
        CapsuleButton(
            onClick = onUseLocalOnly,
            label = stringResource(R.string.android_onboarding_server_use_local_instead),
            emphasis = CapsuleButtonEmphasis.Secondary,
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

@Composable
private fun NoUsableMethod(
    onUseLocalOnly: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleBanner(
        title = stringResource(R.string.android_onboarding_server_no_auth_methods_title),
        body = stringResource(R.string.android_onboarding_server_no_auth_methods_body),
        severity = CapsuleBannerSeverity.Degraded,
        modifier = modifier.fillMaxWidth(),
        action = {
            CapsuleButton(
                onClick = onUseLocalOnly,
                label = stringResource(R.string.android_onboarding_server_use_local_instead),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

@Composable
private fun Answered(
    state: ServerUiState,
    info: ServerInfo,
    onSelectMethod: (AuthMethod) -> Unit,
    onContinue: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(BlockSpacing)) {
        ServerInfoCard(info)
        AuthPicker(info = info, selected = state.selectedMethod, onSelectMethod = onSelectMethod)
        CapsuleButton(
            onClick = onContinue,
            label = stringResource(R.string.android_onboarding_server_continue),
            enabled = state.canContinue,
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

@Composable
private fun ServerInfoCard(
    info: ServerInfo,
    modifier: Modifier = Modifier,
) {
    CapsuleCard(modifier = modifier.fillMaxWidth().testTag(SERVER_INFO_CARD_TAG)) {
        CapsuleSectionHeader(title = stringResource(R.string.android_onboarding_server_info_title))
        InfoRow(labelRes = R.string.android_onboarding_server_info_api_base, value = info.apiBase)
        InfoRow(
            labelRes = R.string.android_onboarding_server_info_auth_methods,
            value = info.authMethods.map { stringResource(it.labelRes()) }.joinToString(),
        )
        InfoRow(
            labelRes = R.string.android_onboarding_server_info_protocol_range,
            value =
                stringResource(R.string.android_onboarding_server_protocol_range_value)
                    .withArguments("min" to info.minProtocolVersion, "max" to info.maxProtocolVersion),
        )
    }
}

@Composable
private fun InfoRow(
    labelRes: Int,
    value: String,
    modifier: Modifier = Modifier,
) {
    Row(
        modifier = modifier.fillMaxWidth().padding(horizontal = ScreenPadding, vertical = BlockSpacing / 4),
        horizontalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        Text(
            text = stringResource(labelRes),
            style = MaterialTheme.typography.labelLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            text = value,
            style = MaterialTheme.typography.bodyMedium,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/**
 * The paths this deployment offers, and only those.
 *
 * Rendered from what the server advertised rather than from a fixed list, because a
 * self-hosted server with no identity provider offers no OIDC and a corporate one may offer
 * nothing else. Showing a path the server cannot honour would be an invitation to fail.
 */
@Composable
private fun AuthPicker(
    info: ServerInfo,
    selected: AuthMethod?,
    onSelectMethod: (AuthMethod) -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxWidth().testTag(AUTH_PICKER_TAG),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing / 2),
    ) {
        Text(
            text = stringResource(R.string.android_onboarding_server_auth_picker_title),
            style = MaterialTheme.typography.titleMediumEmphasized,
        )
        Text(
            text = stringResource(R.string.android_onboarding_server_auth_picker_body),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        info.authMethods.forEach { method ->
            CapsuleChip(
                label = stringResource(method.labelRes()),
                selected = method == selected,
                onSelectedChange = { onSelectMethod(method) },
            )
        }
    }
}

@CapsulePreviews
@Composable
internal fun ServerAnsweredPreview() {
    CapsulePreviewSurface {
        ServerConnectScreen(
            state =
                ServerUiState(
                    address = "https://photos.example.org",
                    info =
                        ServerInfo(
                            apiBase = "https://photos.example.org/api/v1",
                            authMethods = persistentListOf(AuthMethod.Passkey, AuthMethod.PasswordTotp),
                            minProtocolVersion = 1,
                            maxProtocolVersion = 3,
                        ),
                    selectedMethod = AuthMethod.Passkey,
                ),
            onAddressChanged = {},
            onConnect = {},
            onSelectMethod = {},
            onContinue = {},
            onUseLocalOnly = {},
        )
    }
}
