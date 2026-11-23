package com.justin13888.capsule.feature.security

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
 * substitute. Deliberately the *simple* subset, because that is the subset the generator will
 * emit to Android at all.
 *
 * One pass rather than repeated [String.replace], so a substituted value that itself contains
 * braces cannot be rescanned and expanded again.
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
            // An unresolved placeholder is left verbatim: a visible `{date}` is a bug report,
            // where silently dropping it is a mistranslation nobody notices.
            if (match == null) out.append(this, open, close + 1) else out.append(match.second.toString())
            index = close + 1
        }
    }
    return out.toString()
}

/**
 * A date-and-time formatter in the reader's own locale and zone.
 *
 * Session timestamps are read as evidence — "was that before or after I lost the phone?" — so
 * they are rendered absolutely rather than as "3 days ago", which is unusable for exactly that
 * question.
 */
@Composable
internal fun formattedInstant(instant: Instant): String {
    val locale = Locale.getDefault()
    val zone = ZoneId.systemDefault()
    val formatter =
        remember(locale, zone) {
            DateTimeFormatter.ofLocalizedDateTime(FormatStyle.MEDIUM).withLocale(locale).withZone(zone)
        }
    return formatter.format(instant)
}

/** [instant] as a date alone, for a row where the time of day adds nothing. */
@Composable
internal fun formattedDate(instant: Instant): String {
    val locale = Locale.getDefault()
    val zone = ZoneId.systemDefault()
    val formatter =
        remember(locale, zone) {
            DateTimeFormatter.ofLocalizedDate(FormatStyle.MEDIUM).withLocale(locale).withZone(zone)
        }
    return formatter.format(instant)
}

/**
 * What a failure says to the user.
 *
 * A coded refusal is resolved by name rather than through a hand-written table over the
 * catalogue's seventy-odd codes: the key-to-resource mapping is mechanical, and a table is a
 * list that silently stops covering codes added later. An unresolved code falls back to the
 * generic message rather than rendering a raw key at the user.
 */
@Composable
internal fun securityFailureMessage(failure: SecurityFailure): String =
    when (failure) {
        is SecurityFailure.TotpCode -> stringResource(R.string.android_security_totp_code_rejected)
        is SecurityFailure.Coded -> localizedErrorMessage(failure.code)
        is SecurityFailure.Unclassified -> stringResource(R.string.common_error_generic)
    }

@Composable
private fun localizedErrorMessage(code: ErrorCode): String {
    val context = LocalContext.current
    val resolved = remember(context, code) { context.resolveErrorString(code) }
    return resolved ?: stringResource(R.string.common_error_generic)
}

private fun Context.resolveErrorString(code: ErrorCode): String? {
    val name = code.value.replace('.', '_').replace('-', '_')
    val id = resources.getIdentifier(name, "string", packageName)
    return if (id == 0) null else getString(id)
}

/**
 * What one cohort group is called.
 *
 * The copy asserts rather than litigates. "A device you have used before" is a statement the
 * durable cohort map entitles the client to make, and there is nothing offered beside it for
 * the user to disagree with — because a user cannot adjudicate a hash, and the value is
 * advisory anyway.
 */
@Composable
internal fun cohortHeadline(group: SessionCohortGroup): String =
    when {
        group.isCurrent -> stringResource(R.string.android_security_cohort_this_device)
        group.seenBefore ->
            stringResource(R.string.android_security_cohort_seen_before)
                .withArguments("date" to (group.lastSeen?.let { formattedDate(it) } ?: UNKNOWN))

        else -> stringResource(R.string.android_security_cohort_ungrouped)
    }

/** The line under a cohort heading, when the map knows when it was first seen. */
@Composable
internal fun cohortSupporting(group: SessionCohortGroup): String? =
    when {
        group.cohortHash == null -> stringResource(R.string.android_security_cohort_ungrouped_body)
        group.firstSeen == null -> null
        else ->
            stringResource(R.string.android_security_cohort_first_seen)
                .withArguments("date" to formattedDate(group.firstSeen))
    }

/** Stands in for a date the durable map does not carry. */
private const val UNKNOWN = "—"

/** Joins two already-localized fragments of one row. */
internal const val SEPARATOR: String = " · "
