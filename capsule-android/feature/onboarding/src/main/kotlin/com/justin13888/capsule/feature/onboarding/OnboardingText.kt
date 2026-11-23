package com.justin13888.capsule.feature.onboarding

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.ErrorCode

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
 * rather than rendering a raw key — or, worse, a transport status — at the user.
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

/** The catalogue row naming one auth path. */
internal fun AuthMethod.labelRes(): Int =
    when (this) {
        AuthMethod.Passkey -> R.string.android_onboarding_server_auth_passkey
        AuthMethod.PasswordTotp -> R.string.android_onboarding_server_auth_password_totp
        AuthMethod.Oidc -> R.string.android_onboarding_server_auth_oidc
    }
