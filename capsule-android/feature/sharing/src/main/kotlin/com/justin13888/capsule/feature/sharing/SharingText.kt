package com.justin13888.capsule.feature.sharing

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.ErrorCode
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale

/**
 * Fills the `{name}` placeholders of a catalog message.
 *
 * The catalog is ICU MessageFormat and the Android generator copies simple placeholders
 * through verbatim, so `stringResource` hands back a template and something has to
 * substitute. Deliberately the *simple* subset — no plural, no select, no number skeletons —
 * because the generator refuses to emit those to Android in the first place.
 *
 * Duplicated from the identical helper in the other feature modules rather than shared.
 * Publishing it would make a substitution routine part of some module's public contract, and
 * the honest home for it is the i18n layer once a generated formatter lands.
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
 * A date formatter in the reader's own locale and zone.
 *
 * Expiry is rendered as an absolute date rather than "in 6 days", because the question a user
 * asks of a share link — "is this still going to be live when they get round to opening it?"
 * — is about a calendar, and a relative phrase makes them do the arithmetic themselves.
 */
@Composable
internal fun rememberShareDateFormatter(): DateTimeFormatter {
    val locale = Locale.getDefault()
    val zone = ZoneId.systemDefault()
    return remember(locale, zone) {
        DateTimeFormatter.ofLocalizedDate(FormatStyle.MEDIUM).withLocale(locale).withZone(zone)
    }
}

/** [instant] as the reader's locale spells the date. */
@Composable
internal fun formattedDate(instant: Instant): String = rememberShareDateFormatter().format(instant)

/**
 * The message behind an `error.*` code, localized.
 *
 * Resolved by name rather than through a hand-written `when` over the catalogue's seventy-odd
 * codes: the mapping from key to resource is mechanical, and a hand-written table is a list
 * that silently stops covering codes added later. An unresolved code falls back to the
 * generic message rather than rendering the raw key — or, worse, a transport status — at the
 * user.
 */
@Composable
internal fun localizedErrorMessage(code: ErrorCode?): String {
    val context = LocalContext.current
    val resolved = remember(context, code) { code?.let(context::resolveErrorString) }
    return resolved ?: stringResource(R.string.common_error_generic)
}

/** The body a [SharingFailure] should explain itself with. */
@Composable
internal fun failureMessage(failure: SharingFailure): String =
    when (failure.kind) {
        SharingFailureKind.Offline -> stringResource(R.string.android_sharing_offline_body)
        SharingFailureKind.Reported -> localizedErrorMessage(failure.code)
        SharingFailureKind.Unknown -> stringResource(R.string.common_error_generic)
    }

/** The headline a [SharingFailure] should explain itself with. */
@Composable
internal fun failureTitle(failure: SharingFailure): String =
    when (failure.kind) {
        SharingFailureKind.Offline -> stringResource(R.string.android_sharing_offline_title)
        else -> stringResource(R.string.android_sharing_error_title)
    }

private fun Context.resolveErrorString(code: ErrorCode): String? {
    val name = code.value.replace('.', '_').replace('-', '_')
    val id = resources.getIdentifier(name, "string", packageName)
    return if (id == 0) null else getString(id)
}

/** Joins two already-localized fragments of one row. */
internal const val SEPARATOR: String = " · "
