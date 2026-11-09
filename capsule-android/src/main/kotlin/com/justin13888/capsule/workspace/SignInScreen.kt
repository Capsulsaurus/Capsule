package com.justin13888.capsule.workspace

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.justin13888.capsule.R

/**
 * Sign in to a Capsule server: its domain, the handle and password, and the
 * authenticator code when the account asks for one. Closes itself once signed in.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SignInScreen(
    viewModel: SignInViewModel,
    navigateBack: () -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(state.isDone) { if (state.isDone) navigateBack() }
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.app_auth_login_title)) },
                navigationIcon = {
                    IconButton(onClick = navigateBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.back))
                    }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            val editable = !state.isSubmitting && !state.askingForCode
            Text(stringResource(R.string.app_auth_login_subtitle), style = MaterialTheme.typography.bodyMedium)
            Field(
                value = state.server,
                onChange = viewModel::updateServer,
                label = R.string.app_auth_server_domain_label,
                placeholder = R.string.app_auth_server_domain_prompt,
                enabled = editable,
                keyboard = KeyboardType.Uri,
            )
            Field(
                value = state.handle,
                onChange = viewModel::updateHandle,
                label = R.string.app_auth_login_handle_label,
                placeholder = R.string.app_auth_login_handle_prompt,
                enabled = editable,
                keyboard = KeyboardType.Email,
            )
            if (state.askingForCode) {
                Field(
                    value = state.code,
                    onChange = viewModel::updateCode,
                    label = R.string.app_auth_login_totp_label,
                    placeholder = null,
                    enabled = !state.isSubmitting,
                    keyboard = KeyboardType.NumberPassword,
                )
                Text(stringResource(R.string.app_auth_login_totp_footer), style = MaterialTheme.typography.bodySmall)
            } else {
                OutlinedTextField(
                    value = state.password,
                    onValueChange = viewModel::updatePassword,
                    label = { Text(stringResource(R.string.app_auth_login_password_label)) },
                    enabled = editable,
                    singleLine = true,
                    visualTransformation = PasswordVisualTransformation(),
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                    modifier = Modifier.fillMaxWidth(),
                )
            }
            state.message?.let { message ->
                Text(
                    stringResource(message.text()),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            Button(onClick = viewModel::submit, enabled = !state.isSubmitting, modifier = Modifier.fillMaxWidth()) {
                Text(
                    stringResource(
                        if (state.isSubmitting) R.string.app_auth_login_submitting else R.string.app_auth_login_submit,
                    ),
                )
            }
        }
    }
}

@Composable
private fun Field(
    value: String,
    onChange: (String) -> Unit,
    @StringRes label: Int,
    @StringRes placeholder: Int?,
    enabled: Boolean,
    keyboard: KeyboardType,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onChange,
        label = { Text(stringResource(label)) },
        placeholder = placeholder?.let { { Text(stringResource(it)) } },
        enabled = enabled,
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = keyboard),
        modifier = Modifier.fillMaxWidth(),
    )
}

@StringRes
private fun SignInMessage.text(): Int =
    when (this) {
        SignInMessage.InvalidAddress -> R.string.app_auth_server_subtitle
        SignInMessage.CredentialsRejected -> R.string.error_auth_invalid_credentials
        SignInMessage.RateLimited -> R.string.error_auth_rate_limited
        SignInMessage.AccountLocked -> R.string.error_auth_account_locked
        SignInMessage.CodeInvalid -> R.string.error_auth_totp_invalid_code
        SignInMessage.ChallengeExpired -> R.string.error_auth_totp_challenge_invalid
        SignInMessage.ProtocolUnsupported -> R.string.error_protocol_version_unsupported
        SignInMessage.Unavailable -> R.string.error_auth_unavailable
    }
