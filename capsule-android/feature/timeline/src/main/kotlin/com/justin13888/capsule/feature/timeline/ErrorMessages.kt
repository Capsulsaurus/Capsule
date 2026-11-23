package com.justin13888.capsule.feature.timeline

import androidx.compose.runtime.Composable
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.ErrorCode

/**
 * The localized sentence behind a stable `error.*` code.
 *
 * Clients localize the code and never the transport's status, so this is the only place the
 * timeline turns a failure into words. The lookup is by name because the catalog's keys and
 * the generated resource names differ only in their separator — `error.quota.exceeded`
 * becomes `error_quota_exceeded` — and a hand-written `when` over seventy-four codes would be
 * seventy-four chances to fall behind the catalog.
 *
 * An unrecognised code falls back to the generic message rather than showing the raw key. A
 * code this build does not know is a newer server talking to an older client, which is a
 * condition to be honest about, not a string to paste at the user.
 */
@Composable
fun localizedError(code: ErrorCode?): String {
    val generic = stringResource(R.string.common_error_generic)
    if (code == null) return generic
    val context = LocalContext.current
    val name = code.value.replace('.', '_').replace('-', '_')
    val id = context.resources.getIdentifier(name, "string", context.packageName)
    return if (id != 0) context.getString(id) else generic
}
