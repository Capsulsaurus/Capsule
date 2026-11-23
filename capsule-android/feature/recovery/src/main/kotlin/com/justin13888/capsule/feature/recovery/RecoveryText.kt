package com.justin13888.capsule.feature.recovery

import android.content.Context
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.TextFieldColors
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.ErrorCode
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale

/** The inset every screen in this feature lays its content out against. */
internal val ScreenInset = 16.dp

/** The gap between two stacked blocks of one screen. */
internal val ScreenSpacing = 12.dp

/**
 * Fills the `{name}` placeholders of a catalog message.
 *
 * The catalog is ICU MessageFormat and the Android generator copies simple placeholders
 * through verbatim, so `stringResource` hands back a template and something has to
 * substitute. Deliberately the *simple* subset — no plural, no select — because the generator
 * refuses to emit those to Android in the first place.
 *
 * Substitution is a single pass rather than repeated [String.replace] so a value that itself
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
 * A date formatter in the reader's own locale and zone.
 *
 * Recovery dates are read as commitments — "when will it ask me again?" — so they are
 * rendered absolutely rather than as "in 3 months", which is unusable for planning around.
 */
@Composable
private fun rememberRecoveryDateFormatter(): DateTimeFormatter {
    val locale = Locale.getDefault()
    val zone = ZoneId.systemDefault()
    return remember(locale, zone) {
        DateTimeFormatter.ofLocalizedDate(FormatStyle.MEDIUM).withLocale(locale).withZone(zone)
    }
}

/** [instant] as a date, the way the reader's locale spells it. */
@Composable
internal fun formattedDate(instant: Instant): String = rememberRecoveryDateFormatter().format(instant)

/**
 * A byte count in the units a person reads.
 *
 * Binary prefixes, because a backup's size is compared against the free space a file manager
 * reports, and those are binary everywhere the user will look.
 */
internal fun formattedBytes(bytes: Long): String {
    if (bytes < UNIT) return "$bytes B"
    var value = bytes.toDouble()
    var unit = 0
    while (value >= UNIT && unit < UNITS.lastIndex) {
        value /= UNIT
        unit += 1
    }
    return String.format(Locale.getDefault(), "%.1f %s", value, UNITS[unit])
}

private const val UNIT = 1024.0
private val UNITS = listOf("KiB", "MiB", "GiB", "TiB")

/**
 * The English detail behind an `error.*` code, localized.
 *
 * Resolved by name rather than through a hand-written `when` over the catalogue's seventy-odd
 * codes: the mapping from key to resource is mechanical (dots become underscores), and a
 * hand-written table silently stops covering codes added later. An unresolved code falls back
 * to the generic message rather than rendering the raw key at the user.
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

/**
 * Text-field colours that clear the contrast floor at rest.
 *
 * `core:designsystem` has no text-field wrapper yet, so these screens reach for Material's own
 * — and Material draws an unfocused outline in `outline`, which measures 4.27:1 against this
 * theme's light surface where small text needs 4.5:1. The border is the only thing marking
 * where a field begins, so a user who cannot resolve it cannot find the field at all.
 *
 * Overriding only the two resting borders keeps every focused, errored and selected colour
 * exactly as the theme defines it; this is a floor being raised, not a palette being forked.
 */
@Composable
internal fun capsuleFieldColors(): TextFieldColors =
    OutlinedTextFieldDefaults.colors(
        unfocusedBorderColor = MaterialTheme.colorScheme.onSurfaceVariant,
        disabledBorderColor = MaterialTheme.colorScheme.onSurfaceVariant,
    )

/** Joins two already-localized fragments of one line. */
internal const val SEPARATOR: String = " · "
