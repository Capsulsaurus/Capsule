package com.justin13888.capsule.feature.onboarding

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
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
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSheet
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviews
import com.justin13888.capsule.core.i18n.R

/** Test handle for the credential form. */
const val CREDENTIALS_FORM_TAG: String = "onboarding.credentials"

/** Test handle for the time-based challenge. */
const val TOTP_FORM_TAG: String = "onboarding.totp"

/** Test handle for the passkey step, whose biometric sheet has no typed input at all. */
const val PASSKEY_STEP_TAG: String = "onboarding.passkey"

/** Test handle for the identity-provider hand-off. */
const val OIDC_STEP_TAG: String = "onboarding.oidc"

private val ScreenPadding = 24.dp
private val BlockSpacing = 16.dp

/**
 * Establishing a session along the path the server offered.
 *
 * Refusals are rendered from the `error.*` code the gateway carried, which is why the wrong
 * password and an account that does not exist read identically here: the endpoint is
 * deliberately not an oracle for which half of a credential pair was wrong, and a client that
 * split them in the UI would undo that.
 */
@Composable
fun SignInScreen(
    state: SignInUiState,
    actions: SignInActions,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(ScreenPadding),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        Text(
            text =
                stringResource(
                    if (state.isRegistering) {
                        R.string.android_onboarding_signin_register_title
                    } else {
                        R.string.android_onboarding_signin_title
                    },
                ),
            style = MaterialTheme.typography.headlineMediumEmphasized,
        )
        state.failure?.let { failure ->
            CapsuleBanner(
                title = stringResource(R.string.android_onboarding_signin_error_title),
                body = localizedErrorMessage(failure.code),
                severity = CapsuleBannerSeverity.Error,
                modifier = Modifier.fillMaxWidth(),
            )
        }
        when (state.stage) {
            SignInStage.Credentials -> CredentialsForm(state, actions)
            SignInStage.Totp -> TotpForm(state, actions)
            SignInStage.Passkey -> PasskeyStep(state, actions)
            SignInStage.Oidc -> OidcStep(state, actions)
        }
    }
}

/**
 * The callbacks the sign-in screen raises.
 *
 * Bundled rather than passed one by one because there are seven of them and a screen with a
 * eight-lambda signature is a screen nobody previews. The bundle is also what a test
 * substitutes wholesale to record what the screen asked for — which is why it stays a plain
 * list of callbacks rather than being split into two objects nobody would pass separately.
 */
@Suppress("LongParameterList")
class SignInActions(
    /** The handle changed. */
    val onEmailChanged: (String) -> Unit,
    /** The password changed. */
    val onPasswordChanged: (String) -> Unit,
    /** The one-time code changed. */
    val onTotpCodeChanged: (String) -> Unit,
    /** Submit the first factor, or create the account. */
    val onSubmitCredentials: () -> Unit,
    /** Answer the time-based challenge. */
    val onSubmitTotp: () -> Unit,
    /** Run the platform passkey ceremony. */
    val onAuthenticateWithPasskey: () -> Unit,
    /** Complete the identity-provider hand-off. */
    val onCompleteOidc: () -> Unit,
    /** Switch between signing in and registering. */
    val onToggleRegistering: () -> Unit,
)

@Composable
private fun CredentialsForm(
    state: SignInUiState,
    actions: SignInActions,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxWidth().testTag(CREDENTIALS_FORM_TAG),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        OnboardingTextField(
            value = state.email,
            onValueChange = actions.onEmailChanged,
            labelRes = R.string.common_email,
            placeholderRes = R.string.common_email_placeholder,
            enabled = !state.isWorking,
            keyboardOptions =
                KeyboardOptions(
                    keyboardType = KeyboardType.Email,
                    capitalization = KeyboardCapitalization.None,
                ),
        )
        OnboardingTextField(
            value = state.password,
            onValueChange = actions.onPasswordChanged,
            labelRes = R.string.common_password,
            supportingRes = R.string.common_password_min,
            enabled = !state.isWorking,
            isSecret = true,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
        )
        if (state.isWorking) {
            CapsuleLoadingState(
                title = stringResource(R.string.android_onboarding_signin_working),
                contentDescription = stringResource(R.string.android_onboarding_signin_working),
            )
        }
        CapsuleButton(
            onClick = actions.onSubmitCredentials,
            label =
                stringResource(
                    if (state.isRegistering) {
                        R.string.android_onboarding_signin_register_submit
                    } else {
                        R.string.common_sign_in
                    },
                ),
            enabled = state.canSubmitCredentials,
            modifier = Modifier.fillMaxWidth(),
        )
        CapsuleButton(
            onClick = actions.onToggleRegistering,
            label =
                stringResource(
                    if (state.isRegistering) {
                        R.string.android_onboarding_signin_to_signin
                    } else {
                        R.string.android_onboarding_signin_to_register
                    },
                ),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

@Composable
private fun TotpForm(
    state: SignInUiState,
    actions: SignInActions,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxWidth().testTag(TOTP_FORM_TAG),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        Text(
            text = stringResource(R.string.android_onboarding_signin_totp_body),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        OnboardingTextField(
            value = state.totpCode,
            onValueChange = actions.onTotpCodeChanged,
            labelRes = R.string.auth_totp_code_label,
            placeholderRes = R.string.common_code_placeholder,
            enabled = !state.isWorking,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword),
        )
        CapsuleButton(
            onClick = actions.onSubmitTotp,
            label = stringResource(R.string.auth_verify),
            enabled = state.canSubmitTotp,
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

/**
 * The passkey path, whose whole point is that nothing is typed.
 *
 * The sheet is the platform prompt's stand-in. It stays modal while the ceremony is open
 * because dismissing it is how a user says no, and a cancelled ceremony has to be
 * distinguishable from one that is still waiting.
 */
@Composable
private fun PasskeyStep(
    state: SignInUiState,
    actions: SignInActions,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxWidth().testTag(PASSKEY_STEP_TAG),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        Text(
            text = stringResource(R.string.android_onboarding_signin_passkey_body),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        CapsuleButton(
            onClick = actions.onAuthenticateWithPasskey,
            label = stringResource(R.string.auth_sign_in_passkey),
            enabled = !state.isWorking,
            modifier = Modifier.fillMaxWidth(),
        )
        if (state.isWorking) {
            CapsuleSheet(onDismissRequest = {}) {
                Column(
                    modifier = Modifier.fillMaxWidth().padding(ScreenPadding),
                    verticalArrangement = Arrangement.spacedBy(BlockSpacing),
                ) {
                    Text(
                        text = stringResource(R.string.android_onboarding_signin_passkey_sheet_title),
                        style = MaterialTheme.typography.titleLargeEmphasized,
                    )
                    CapsuleLoadingState(
                        title = stringResource(R.string.android_onboarding_signin_passkey_waiting),
                        contentDescription = stringResource(R.string.android_onboarding_signin_passkey_waiting),
                    )
                }
            }
        }
    }
}

@Composable
private fun OidcStep(
    state: SignInUiState,
    actions: SignInActions,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxWidth().testTag(OIDC_STEP_TAG),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        Text(
            text = stringResource(R.string.android_onboarding_signin_oidc_title),
            style = MaterialTheme.typography.titleLargeEmphasized,
        )
        Text(
            text = stringResource(R.string.android_onboarding_signin_oidc_body),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (state.isWorking) {
            CapsuleLoadingState(
                title = stringResource(R.string.android_onboarding_signin_oidc_waiting),
                contentDescription = stringResource(R.string.android_onboarding_signin_oidc_waiting),
            )
        }
        CapsuleButton(
            onClick = actions.onCompleteOidc,
            label = stringResource(R.string.android_onboarding_signin_oidc_action),
            enabled = !state.isWorking,
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

/** Callbacks that do nothing, for previews. */
internal fun inertSignInActions(): SignInActions =
    SignInActions(
        onEmailChanged = {},
        onPasswordChanged = {},
        onTotpCodeChanged = {},
        onSubmitCredentials = {},
        onSubmitTotp = {},
        onAuthenticateWithPasskey = {},
        onCompleteOidc = {},
        onToggleRegistering = {},
    )

@CapsulePreviews
@Composable
internal fun CredentialsPreview() {
    CapsulePreviewSurface {
        SignInScreen(state = SignInUiState(email = "ada@example.org"), actions = inertSignInActions())
    }
}

@CapsulePreviews
@Composable
internal fun TotpPreview() {
    CapsulePreviewSurface {
        SignInScreen(state = SignInUiState(stage = SignInStage.Totp), actions = inertSignInActions())
    }
}
