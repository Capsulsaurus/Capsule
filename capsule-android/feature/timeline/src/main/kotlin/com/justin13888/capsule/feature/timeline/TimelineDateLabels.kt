package com.justin13888.capsule.feature.timeline

import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.remember
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.intl.Locale
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.DayBucket
import java.time.Clock
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale as JavaLocale

/**
 * Every date the timeline says out loud, formatted once.
 *
 * Formatters are expensive to build and are rebuilt by every naive call site, so they are
 * assembled once per locale and handed to the grid, the scrubber and the announced labels
 * alike. Holding them in one place also keeps the three consistent: a header reading
 * "15 June 2026" beside a scrubber bubble reading "6/2026" is two different products.
 *
 * "Today" and "yesterday" come from the catalog rather than from the formatter, because no
 * date formatter knows them — and a timeline whose newest section is labelled with a full
 * date is a timeline that makes the reader work out what day it is.
 *
 * @param zone the zone a capture instant is read in. The device's, not the capture's: a
 *   header groups by the day the user lived through.
 */
@Immutable
class TimelineDateLabels(
    private val zone: ZoneId,
    private val today: LocalDate,
    private val todayLabel: String,
    private val yesterdayLabel: String,
    private val dayFormatter: DateTimeFormatter,
    private val monthFormatter: DateTimeFormatter,
    private val instantFormatter: DateTimeFormatter,
) {
    /** The section header for one day — "Today", "Yesterday", or the formatted date. */
    fun day(bucket: DayBucket): String =
        when (bucket.day) {
            today -> todayLabel
            today.minusDays(1) -> yesterdayLabel
            else -> dayFormatter.format(bucket.day)
        }

    /** The scrubber's bubble and rail ticks — a month, since a rail cannot show every day. */
    fun month(bucket: DayBucket): String = monthFormatter.format(bucket.day)

    /** The date announced beside one asset. */
    fun announce(at: Instant): String = instantFormatter.format(at.atZone(zone))

    companion object {
        /**
         * Builds the labels for the composition's locale.
         *
         * Keyed on the locale tag so a language change rebuilds them and nothing else does —
         * a formatter rebuilt on every recomposition would be an allocation per scrolled
         * frame, on the one path that must not allocate.
         */
        @Composable
        fun rememberLabels(clock: Clock = Clock.systemDefaultZone()): TimelineDateLabels {
            val tag = Locale.current.toLanguageTag()
            val todayLabel = stringResource(R.string.timeline_day_today)
            val yesterdayLabel = stringResource(R.string.timeline_day_yesterday)
            return remember(tag, todayLabel, yesterdayLabel) {
                val locale = JavaLocale.forLanguageTag(tag)
                val zone = clock.zone
                TimelineDateLabels(
                    zone = zone,
                    today = LocalDate.now(clock),
                    todayLabel = todayLabel,
                    yesterdayLabel = yesterdayLabel,
                    dayFormatter =
                        DateTimeFormatter.ofLocalizedDate(FormatStyle.LONG).withLocale(locale).withZone(zone),
                    monthFormatter = DateTimeFormatter.ofPattern(MONTH_PATTERN, locale).withZone(zone),
                    instantFormatter =
                        DateTimeFormatter.ofLocalizedDate(FormatStyle.MEDIUM).withLocale(locale).withZone(zone),
                )
            }
        }

        /**
         * Month and year, with the month standing alone.
         *
         * `LLLL` rather than `MMMM`: in the languages that inflect, a month named on its own
         * takes the nominative, and the genitive a full date would use reads as a fragment.
         */
        private const val MONTH_PATTERN = "LLLL yyyy"
    }
}
