package com.justin13888.capsule.feature.storage

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AssetType
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.HeldRepresentations
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.SyncScope
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
 * substitute. Deliberately the *simple* subset only, because that is the subset the
 * generator is willing to emit to Android at all.
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
            // An unresolved placeholder is left verbatim: a visible `{size}` is a bug report,
            // where silently dropping it is a mistranslation nobody notices.
            if (match == null) out.append(this, open, close + 1) else out.append(match.second.toString())
            index = close + 1
        }
    }
    return out.toString()
}

/** Which unit row a byte count is rendered in, and how many of that unit it is. */
internal data class ByteMagnitude(
    /** The catalogue row naming the unit. */
    val unitRes: Int,
    /** The count in that unit. */
    val value: Double,
)

/**
 * Picks the unit a size reads best in.
 *
 * Binary steps, because every budget and limit in this feature is expressed in powers of two
 * and a screen that showed `4 GB` next to a slider set to 4 GiB would be describing two
 * different numbers.
 */
internal fun byteMagnitude(bytes: Long): ByteMagnitude {
    val safe = bytes.coerceAtLeast(0L)
    val units =
        listOf(
            R.string.android_storage_bytes_tb to TIB,
            R.string.android_storage_bytes_gb to GIB,
            R.string.android_storage_bytes_mb to MIB,
            R.string.android_storage_bytes_kb to KIB,
        )
    val match = units.firstOrNull { safe >= it.second }
    return if (match == null) {
        ByteMagnitude(R.string.android_storage_bytes_b, safe.toDouble())
    } else {
        ByteMagnitude(match.first, safe.toDouble() / match.second)
    }
}

/**
 * [bytes] in the unit it reads best in, in the reader's own locale.
 *
 * One decimal below a thousand of the chosen unit and none above it: "1.4 GB" is useful and
 * "1,412.7 MB" is noise a user has to convert in their head.
 */
@Composable
internal fun formattedBytes(bytes: Long): String {
    val magnitude = byteMagnitude(bytes)
    val locale = Locale.getDefault()
    val value = remember(magnitude, locale) { formatMagnitude(magnitude.value, locale) }
    return stringResource(magnitude.unitRes).withArguments("value" to value)
}

private fun formatMagnitude(
    value: Double,
    locale: Locale,
): String {
    val pattern = if (value < 10.0) "%.1f" else "%.0f"
    return String.format(locale, pattern, value)
}

/**
 * A date-only formatter in the reader's own locale and zone.
 *
 * Storage rows are scanned rather than read — the user is looking for something large, not
 * something specific — so a date is enough handle and a full timestamp would be twice the
 * text for no extra decision.
 */
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

/** What kind of thing one consumer row is. */
@Composable
internal fun assetKindLabel(type: AssetType?): String =
    when (type) {
        AssetType.Photo -> stringResource(R.string.android_storage_kind_photo)
        AssetType.Video -> stringResource(R.string.android_storage_kind_video)
        // A token outside this build's closed set is named as an item rather than hidden: it
        // still occupies room on the device, and a row the user cannot see is room they
        // cannot reclaim.
        null -> stringResource(R.string.android_storage_kind_other)
    }

/** The heaviest rung this device holds for an asset, said plainly. */
@Composable
internal fun heldLabel(held: HeldRepresentations): String =
    when (held.best) {
        RepresentationTier.Original -> stringResource(R.string.android_storage_held_original)
        RepresentationTier.Preview -> stringResource(R.string.android_storage_held_preview)
        else -> stringResource(R.string.android_storage_held_thumbnail)
    }

/** The name of one sync scope. */
@Composable
internal fun syncScopeLabel(scope: SyncScope): String =
    when (scope) {
        SyncScope.MetadataOnly -> stringResource(R.string.android_storage_scope_metadata)
        SyncScope.MetadataAndThumbnails -> stringResource(R.string.android_storage_scope_thumbnails)
        SyncScope.MetadataThumbnailsAndOriginal -> stringResource(R.string.android_storage_scope_originals)
    }

/** What one sync scope costs and gives. */
@Composable
internal fun syncScopeBody(scope: SyncScope): String =
    when (scope) {
        SyncScope.MetadataOnly -> stringResource(R.string.android_storage_scope_metadata_body)
        SyncScope.MetadataAndThumbnails -> stringResource(R.string.android_storage_scope_thumbnails_body)
        SyncScope.MetadataThumbnailsAndOriginal -> stringResource(R.string.android_storage_scope_originals_body)
    }

/**
 * The English detail behind an `error.*` code, localized.
 *
 * Resolved by name rather than through a hand-written table over the catalogue's seventy-odd
 * codes: the key-to-resource mapping is mechanical, and a table is a list that silently stops
 * covering codes added later. An unresolved code falls back to the generic message rather
 * than rendering a raw key at the user.
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

private const val KIB = 1024.0
private const val MIB = KIB * 1024
private const val GIB = MIB * 1024
private const val TIB = GIB * 1024
