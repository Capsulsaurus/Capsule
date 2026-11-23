package com.justin13888.capsule.feature.onboarding

import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation

/**
 * The one text field these screens use.
 *
 * `core:designsystem` wraps no text field, so the choice of colours would otherwise be made
 * separately on every form. Made once here, and made deliberately: Material's unfocused
 * outline, label and supporting text all default to the `outline`/`onSurfaceVariant` family,
 * which lands at 4.29:1 against this theme's surface — under the 4.5:1 the accessibility
 * framework asks for, and `OnboardingAccessibilityTest` fails on it. The outline is what
 * tells a user where the field is and the label is what tells them what it holds, so both are
 * drawn at `onSurface` instead of being suppressed in the check.
 */
@Suppress("LongParameterList")
@Composable
internal fun OnboardingTextField(
    value: String,
    onValueChange: (String) -> Unit,
    labelRes: Int,
    modifier: Modifier = Modifier,
    placeholderRes: Int? = null,
    supportingRes: Int? = null,
    isError: Boolean = false,
    enabled: Boolean = true,
    isSecret: Boolean = false,
    keyboardOptions: KeyboardOptions = KeyboardOptions.Default,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        modifier = modifier.fillMaxWidth(),
        singleLine = true,
        enabled = enabled,
        isError = isError,
        label = { Text(text = stringResource(labelRes)) },
        placeholder = placeholderRes?.let { { Text(text = stringResource(it)) } },
        supportingText = supportingRes?.let { { Text(text = stringResource(it)) } },
        visualTransformation =
            if (isSecret) PasswordVisualTransformation() else VisualTransformation.None,
        keyboardOptions = keyboardOptions,
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
