package com.justin13888.capsule.feature.albums.ui.common

import java.time.Instant
import java.time.LocalDate
import java.time.ZoneOffset

/**
 * Fills the `{name}` placeholders of a catalog message.
 *
 * The catalog is ICU MessageFormat and the Android generator copies simple placeholders
 * through verbatim, so `stringResource` hands back a template rather than a finished string
 * and something has to substitute. Deliberately the *simple* subset — no plural, no select,
 * no number skeletons — because the generator refuses to emit those to Android at all, so a
 * message needing them would never reach this function.
 *
 * One pass over the template rather than repeated [String.replace], so a substituted value
 * that itself contains braces — an album name, a server hostname — cannot be rescanned and
 * expanded a second time.
 */
internal fun String.withArgs(vararg arguments: Pair<String, Any>): String {
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
            // An unrecognised placeholder is left verbatim: a visible `{count}` is a bug
            // report, while silently dropping it is a mistranslation nobody notices.
            if (match == null) out.append(this, open, close + 1) else out.append(match.second.toString())
            index = close + 1
        }
    }
    return out.toString()
}

/**
 * An instant as the `YYYY-MM-DD` the surfaces here show.
 *
 * UTC rather than the device zone, and deliberately so: every date in this feature comes off
 * a signed record — when a member joined, when a delete manifest was stamped — and rendering
 * those in a zone the record does not carry would make the same album read differently in
 * two places.
 */
internal fun Instant.asIsoDate(): String = LocalDate.ofInstant(this, ZoneOffset.UTC).toString()
