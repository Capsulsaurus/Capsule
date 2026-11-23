package com.justin13888.capsule.feature.drops

import android.content.Context
import android.text.format.Formatter
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
 * Duplicated from the identical helper in the other feature modules rather than shared:
 * publishing it would make a substitution routine part of some module's public contract, and
 * its honest home is the i18n layer once a generated formatter lands.
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
 * A byte count as the platform spells it.
 *
 * Android's own formatter rather than a hand-rolled one: it is already localized, it already
 * knows which unit ladder each locale expects, and every alternative here would be a second
 * spelling of file sizes inside one app.
 */
@Composable
internal fun formattedBytes(bytes: Long): String {
    val context = LocalContext.current
    return remember(context, bytes) { Formatter.formatShortFileSize(context, bytes) }
}

/**
 * A date in the reader's own locale and zone.
 *
 * Absolute rather than relative. "Expires in 3 days" is a phrase a user has to convert before
 * they can decide whether to hand the link out today, and the conversion is the part they get
 * wrong.
 */
@Composable
internal fun rememberDropsDateFormatter(): DateTimeFormatter {
    val locale = Locale.getDefault()
    val zone = ZoneId.systemDefault()
    return remember(locale, zone) {
        DateTimeFormatter.ofLocalizedDate(FormatStyle.MEDIUM).withLocale(locale).withZone(zone)
    }
}

/** [instant] as the reader's locale spells the date. */
@Composable
internal fun formattedDate(instant: Instant): String = rememberDropsDateFormatter().format(instant)

/**
 * The message behind an `error.*` code, localized.
 *
 * Resolved by name rather than through a hand-written `when` over the catalogue's seventy-odd
 * codes: the mapping from key to resource is mechanical, and a hand-written table is one that
 * silently stops covering codes added later. An unresolved code falls back to the generic
 * message rather than showing the raw key — or a transport status — to the user.
 */
@Composable
internal fun localizedErrorMessage(code: ErrorCode?): String {
    val context = LocalContext.current
    val resolved = remember(context, code) { code?.let(context::resolveErrorString) }
    return resolved ?: stringResource(R.string.common_error_generic)
}

/** The headline a [DropsFailure] should explain itself with. */
@Composable
internal fun failureTitle(failure: DropsFailure): String =
    when (failure.kind) {
        DropsFailureKind.Offline -> stringResource(R.string.android_drops_offline_title)
        else -> stringResource(R.string.android_drops_error_title)
    }

/** The body a [DropsFailure] should explain itself with. */
@Composable
internal fun failureMessage(failure: DropsFailure): String =
    when (failure.kind) {
        DropsFailureKind.Offline -> stringResource(R.string.android_drops_offline_body)
        DropsFailureKind.Reported -> localizedErrorMessage(failure.code)
        DropsFailureKind.Unknown -> stringResource(R.string.common_error_generic)
    }

private fun Context.resolveErrorString(code: ErrorCode): String? {
    val name = code.value.replace('.', '_').replace('-', '_')
    val id = resources.getIdentifier(name, "string", packageName)
    return if (id == 0) null else getString(id)
}

/** Joins two already-localized fragments of one row. */
internal const val SEPARATOR: String = " · "
