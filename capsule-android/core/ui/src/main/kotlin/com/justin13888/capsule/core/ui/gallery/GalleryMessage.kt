package com.justin13888.capsule.core.ui.gallery

/**
 * Fills the `{name}` placeholders of a catalog message.
 *
 * The catalog is ICU MessageFormat and the Android generator copies simple placeholders
 * through verbatim — `gallery.a11y.stack` reaches `strings.xml` as `Stack of {count}`,
 * not as `Stack of %1$d`. So `stringResource` hands back a template, and something has to
 * substitute. This is deliberately the *simple* subset: no plural, no select, no number
 * skeletons. The generator already refuses to emit those to Android, so a message that
 * needed them would never reach this function in the first place.
 *
 * Substitution is single-pass over the template rather than repeated [String.replace] so
 * a value that itself contains braces — a filename, a user-typed album title — cannot be
 * re-scanned and expanded a second time.
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
            // An unknown placeholder is left verbatim: a visible `{count}` in a screenshot
            // is a bug report, whereas silently dropping it is a mistranslation nobody
            // notices.
            if (match == null) out.append(this, open, close + 1) else out.append(match.second.toString())
            index = close + 1
        }
    }
    return out.toString()
}
