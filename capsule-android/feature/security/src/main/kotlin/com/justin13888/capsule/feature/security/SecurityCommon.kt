package com.justin13888.capsule.feature.security

import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.i18n.R

/** The inset every security screen shares. */
internal val ScreenInset = 16.dp

/** The gap between blocks inside a section. */
internal val BlockSpacing = 8.dp

/** The loading state every security screen shares. */
@Composable
internal fun SecurityLoadingState(modifier: Modifier = Modifier) {
    CapsuleLoadingState(
        modifier = modifier.fillMaxWidth().testTag(SECURITY_LOADING_TAG),
        title = stringResource(R.string.android_security_loading),
        contentDescription = stringResource(R.string.android_security_loading),
    )
}

/** The failure state every security screen shares. */
@Composable
internal fun SecurityFailureState(
    failure: SecurityFailure,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleErrorState(
        title = stringResource(R.string.android_security_error_title),
        modifier = modifier.fillMaxWidth().testTag(SECURITY_ERROR_TAG),
        // The localized `error.*` code, never a status: the code is the contract and a status
        // is a transport detail the user can do nothing with.
        body = securityFailureMessage(failure),
        action = {
            CapsuleButton(
                onClick = onRetry,
                label = stringResource(R.string.android_security_retry),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

/**
 * The six-digit authenticator field.
 *
 * `core:designsystem` wraps no text field, so the colour choice would otherwise be made
 * separately wherever a form appears — and made wrong: Material's unfocused outline and label
 * default to the `outline`/`onSurfaceVariant` family, which lands under the 4.5:1 the
 * accessibility framework asks for against this theme's surface. The outline is what tells a
 * user where the field is, so it is drawn at `onSurface` rather than suppressed in the check.
 */
@Composable
internal fun SecurityCodeField(
    value: String,
    onValueChange: (String) -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    isError: Boolean = false,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        modifier = modifier.fillMaxWidth().padding(horizontal = ScreenInset).testTag(SECURITY_CODE_FIELD_TAG),
        singleLine = true,
        enabled = enabled,
        isError = isError,
        label = { Text(text = stringResource(R.string.security_totp_code_label)) },
        placeholder = { Text(text = stringResource(R.string.security_totp_code_placeholder)) },
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword),
        colors =
            OutlinedTextFieldDefaults.colors(
                unfocusedBorderColor = MaterialTheme.colorScheme.onSurface,
                unfocusedLabelColor = MaterialTheme.colorScheme.onSurface,
                unfocusedPlaceholderColor = MaterialTheme.colorScheme.onSurface,
                unfocusedSupportingTextColor = MaterialTheme.colorScheme.onSurface,
                focusedSupportingTextColor = MaterialTheme.colorScheme.onSurface,
                disabledLabelColor = MaterialTheme.colorScheme.onSurface,
                disabledSupportingTextColor = MaterialTheme.colorScheme.onSurface,
            ),
    )
}

/** A quiet line of explanation under a section. */
@Composable
internal fun SecurityFootNote(
    text: String,
    modifier: Modifier = Modifier,
) {
    Text(
        text = text,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = modifier.padding(horizontal = ScreenInset, vertical = BlockSpacing / 2),
    )
}
