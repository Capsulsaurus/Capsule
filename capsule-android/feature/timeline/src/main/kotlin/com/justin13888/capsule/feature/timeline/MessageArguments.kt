package com.justin13888.capsule.feature.timeline

/**
 * Fills the `{name}` placeholders of a catalog message.
 *
 * The catalog is ICU MessageFormat and the Android generator copies simple placeholders
 * through verbatim — `timeline.selection.count` reaches `strings.xml` as `{count} selected`,
 * not as `%1$d selected`. So `stringResource` hands back a template and something has to
 * substitute. Deliberately the *simple* subset: no plural, no select, no number skeletons,
 * because the generator already refuses to emit those to Android.
 *
 * Duplicated from the identical helper inside `core:ui` rather than shared. Publishing it
 * would make a substitution routine part of the gallery's public contract, and the honest
 * home for it is the i18n layer once a generated formatter lands — until then, twenty lines
 * in two places beats an API nobody wants to keep.
 *
 * Substitution is single-pass over the template rather than repeated [String.replace] so a
 * value that itself contains braces — a filename, a user-typed album title — cannot be
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
            // An unknown placeholder is left verbatim: a visible `{count}` in a screenshot is
            // a bug report, whereas silently dropping it is a mistranslation nobody notices.
            if (match == null) out.append(this, open, close + 1) else out.append(match.second.toString())
            index = close + 1
        }
    }
    return out.toString()
}
