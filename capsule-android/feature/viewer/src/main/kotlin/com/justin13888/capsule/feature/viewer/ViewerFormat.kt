package com.justin13888.capsule.feature.viewer

import java.time.Duration
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale

/**
 * Fills the `{name}` placeholders of a catalog message.
 *
 * The catalog is ICU MessageFormat and the Android generator copies simple placeholders
 * through verbatim — `viewer.a11y.stage` reaches `strings.xml` as `Photo, {position} of
 * {total}`, not as `Photo, %1$d of %2$d`. So `stringResource` hands back a template and
 * something has to substitute.
 *
 * Deliberately the *simple* subset: no plural, no select, no number skeletons. The generator
 * refuses to emit those to Android, so a message needing them would never reach here. The
 * scan is single-pass rather than repeated [String.replace] so a substituted value that
 * itself contains braces — a filename, a user-typed tag — cannot be expanded a second time.
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

/**
 * The renderings the viewer needs that no catalog key can carry.
 *
 * Numbers, clocks and hashes are formatted here rather than at each call site so that the
 * same fact reads identically in the info panel, the provenance chain and the editor — a
 * content hash abbreviated to twelve characters in one place and sixteen in another looks
 * like two different hashes to the person comparing them.
 *
 * Every function is pure and takes its zone and locale explicitly, so a screenshot golden
 * that pins a timestamp does not drift when the host machine's defaults change.
 */
internal object ViewerFormat {
    /** How many leading characters of a digest are shown before the ellipsis. */
    const val HASH_PREFIX_LENGTH = 12

    /** Decimal places kept when a stored coordinate is displayed. */
    private const val COORDINATE_SCALE = 100_000.0

    private const val SECONDS_PER_MINUTE = 60L
    private const val SECONDS_PER_HOUR = 3600L
    private const val PERCENT = 100

    /** A capture or import instant, in the reader's locale and the given zone. */
    fun timestamp(
        value: Instant,
        zone: ZoneId = ZoneId.systemDefault(),
        locale: Locale = Locale.getDefault(),
    ): String =
        DateTimeFormatter
            .ofLocalizedDateTime(FormatStyle.MEDIUM)
            .withLocale(locale)
            .withZone(zone)
            .format(value)

    /** A date without its time, for a retention deadline where the hour is noise. */
    fun date(
        value: Instant,
        zone: ZoneId = ZoneId.systemDefault(),
        locale: Locale = Locale.getDefault(),
    ): String =
        DateTimeFormatter
            .ofLocalizedDate(FormatStyle.MEDIUM)
            .withLocale(locale)
            .withZone(zone)
            .format(value)

    /**
     * A running time as `m:ss`, widening to `h:mm:ss` only once there is an hour to show.
     *
     * Always padding to hours would put `0:01:23` under every clip in a phone library, which
     * reads as a stopwatch rather than a duration.
     */
    fun clock(value: Duration): String {
        val total = value.seconds.coerceAtLeast(0)
        val hours = total / SECONDS_PER_HOUR
        val minutes = (total % SECONDS_PER_HOUR) / SECONDS_PER_MINUTE
        val seconds = total % SECONDS_PER_MINUTE
        return if (hours > 0) {
            String.format(Locale.ROOT, "%d:%02d:%02d", hours, minutes, seconds)
        } else {
            String.format(Locale.ROOT, "%d:%02d", minutes, seconds)
        }
    }

    /**
     * A digest abbreviated to its first [HASH_PREFIX_LENGTH] characters.
     *
     * Enough to compare two records by eye, short enough not to wrap on a phone. The full
     * value is never truncated in the model — only in what is drawn.
     */
    fun shortHash(value: String): String {
        if (value.length <= HASH_PREFIX_LENGTH) return value
        return value.take(HASH_PREFIX_LENGTH) + "…"
    }

    /** One coordinate at five decimal places — roughly a metre, and far below GPS noise. */
    fun coordinate(
        degrees: Double,
        locale: Locale = Locale.getDefault(),
    ): String {
        val rounded = Math.round(degrees * COORDINATE_SCALE) / COORDINATE_SCALE
        return String.format(locale, "%.5f", rounded)
    }

    /** Bytes confirmed against bytes declared, as a whole percentage clamped to `0..100`. */
    fun percent(
        sent: Long,
        total: Long,
    ): Int {
        if (total <= 0L) return 0
        return ((sent.toDouble() / total.toDouble()) * PERCENT).toInt().coerceIn(0, PERCENT)
    }

    /** A zoom factor at one decimal place, which is all the precision a reader can use. */
    fun scale(
        value: Float,
        locale: Locale = Locale.getDefault(),
    ): String = String.format(locale, "%.1f", value)
}
