package com.justin13888.capsule.feature.enrollment

import android.content.Context
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.ErrorCode
import java.time.Duration

/**
 * Fills the `{name}` placeholders of a catalog message.
 *
 * The catalog is ICU MessageFormat and the Android generator copies simple placeholders
 * through verbatim, so `stringResource` hands back a template that something has to
 * substitute into. Deliberately the *simple* subset — no plural, no select — because the
 * generator refuses to emit those to Android in the first place.
 *
 * A single pass rather than repeated [String.replace], so a substituted value that itself
 * contains braces cannot be rescanned and expanded a second time.
 */
internal fun String.withArguments(vararg arguments: Pair<String, Any>): String {
    if (arguments.isEmpty() || '{' !in this) return this
    val out = StringBuilder(length)
    var index = 0
    while (index < length) {
        val open = indexOf('{', index)
        val close = if (open < 0) -1 else indexOf('}', open + 1)
        if (open < 0 || close < 0) {
            out.append(this, index, length)
            index = length
        } else {
            out.append(this, index, open)
            val name = substring(open + 1, close)
            val match = arguments.firstOrNull { it.first == name }
            // An unknown placeholder is left verbatim: a visible `{count}` is a bug report,
            // whereas silently dropping it is a mistranslation nobody notices.
            if (match == null) out.append(this, open, close + 1) else out.append(match.second.toString())
            index = close + 1
        }
    }
    return out.toString()
}

/**
 * The message behind an `error.*` code, localized.
 *
 * Resolved by name rather than through a hand-written `when` over the catalogue's seventy-odd
 * codes: the mapping from key to resource is mechanical, and a hand-written table silently
 * stops covering codes added later. An unresolved code falls back to the generic message
 * rather than rendering a raw key — or a transport status — at the user.
 */
@Composable
internal fun localizedErrorMessage(code: ErrorCode?): String {
    val context = LocalContext.current
    val resolved = remember(context, code) { code?.let(context::resolveErrorString) }
    return resolved ?: stringResource(R.string.common_error_generic)
}

private fun Context.resolveErrorString(code: ErrorCode): String? {
    val name = code.value.replace('.', '_').replace('-', '_')
    val id = resources.getIdentifier(name, "string", packageName)
    return if (id == 0) null else getString(id)
}

/** The catalogue row naming one setup step. */
internal fun EnrollmentStep.labelRes(): Int =
    when (this) {
        EnrollmentStep.MasterKey -> R.string.android_enrollment_setup_step_master_key
        EnrollmentStep.IdentityKey -> R.string.android_enrollment_setup_step_identity_key
        EnrollmentStep.DeviceKeys -> R.string.android_enrollment_setup_step_device_keys
        EnrollmentStep.DeviceDirectory -> R.string.android_enrollment_setup_step_directory
        EnrollmentStep.DefaultAlbum -> R.string.android_enrollment_setup_step_default_album
    }

/** The catalogue row naming one step status. */
internal fun StepStatus.labelRes(): Int =
    when (this) {
        StepStatus.Pending -> R.string.android_enrollment_setup_status_pending
        StepStatus.Running -> R.string.android_enrollment_setup_status_running
        StepStatus.Done -> R.string.android_enrollment_setup_status_done
        StepStatus.Failed -> R.string.android_enrollment_setup_status_failed
        StepStatus.Deferred -> R.string.android_enrollment_setup_status_failed
    }

/**
 * A countdown as `m:ss`.
 *
 * Digits rather than "about nine minutes left": the window is a hard ten minutes and the user
 * may be reading a code aloud to someone in another room, so the number has to be exact
 * enough to decide whether there is time to finish.
 */
internal fun Duration.asCountdown(): String {
    val clamped = if (isNegative) Duration.ZERO else this
    val minutes = clamped.toMinutes()
    val seconds = clamped.minusMinutes(minutes).seconds
    return "$minutes:${seconds.toString().padStart(2, '0')}"
}

/**
 * The one text field these screens use.
 *
 * `core:designsystem` wraps no text field, so the choice of colours would otherwise be made
 * separately on every form. Made once here, and made deliberately: Material's unfocused
 * outline, label and supporting text all default to the `outline`/`onSurfaceVariant` family,
 * which lands at 4.29:1 against this theme's surface — under the 4.5:1 the accessibility
 * framework asks for. The outline is what tells a user where the field is and the label is
 * what tells them what it holds, so both are drawn at `onSurface` rather than suppressed.
 */
@Suppress("LongParameterList")
@Composable
internal fun EnrollmentTextField(
    value: String,
    onValueChange: (String) -> Unit,
    label: String,
    modifier: Modifier = Modifier,
    supporting: String? = null,
    isError: Boolean = false,
    enabled: Boolean = true,
    keyboardOptions: KeyboardOptions = KeyboardOptions.Default,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        modifier = modifier.fillMaxWidth(),
        singleLine = true,
        enabled = enabled,
        isError = isError,
        label = { Text(text = label) },
        supportingText = supporting?.let { { Text(text = it) } },
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
