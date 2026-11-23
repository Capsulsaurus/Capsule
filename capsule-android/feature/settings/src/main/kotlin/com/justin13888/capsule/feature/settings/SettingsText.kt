package com.justin13888.capsule.feature.settings

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.ImportScope
import com.justin13888.capsule.core.model.SourceKind
import com.justin13888.capsule.core.model.SyncScope
import com.justin13888.capsule.core.model.UploadPolicy
import com.justin13888.capsule.core.model.UploadTier
import com.justin13888.capsule.core.model.knownOrNull
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

/** A whole number of days, as the catalogue phrases it. */
@Composable
internal fun daysLabel(days: Long): String =
    stringResource(R.string.android_settings_duration_days)
        .withArguments("count" to days)

/** A whole number of minutes, as the catalogue phrases it. */
@Composable
internal fun minutesLabel(minutes: Long): String =
    stringResource(R.string.android_settings_duration_minutes).withArguments("count" to minutes)

/** One rung of the grid ladder, named by how many photos fit across a row. */
@Composable
internal fun gridDensityLabel(assetsAcross: Int): String =
    stringResource(R.string.android_settings_appearance_density_option).withArguments("count" to assetsAcross)

/** A date, in the reader's own locale and zone. */
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

/** The region the device reports, spelled in the reader's own language. */
@Composable
internal fun currentRegionLabel(): String {
    val locale = Locale.getDefault()
    return remember(locale) { locale.getDisplayCountry(locale).ifBlank { locale.toLanguageTag() } }
}

/** How much of each new asset a scope fetches. */
@Composable
internal fun syncScopeLabel(scope: SyncScope): String =
    stringResource(
        when (scope) {
            SyncScope.MetadataOnly -> R.string.android_settings_sync_scope_metadata
            SyncScope.MetadataAndThumbnails -> R.string.android_settings_sync_scope_thumbnails
            SyncScope.MetadataThumbnailsAndOriginal -> R.string.android_settings_sync_scope_original
        },
    )

/** Whether upload sessions open eagerly or in tier order. */
@Composable
internal fun uploadPolicyLabel(policy: UploadPolicy): String =
    stringResource(
        when (policy) {
            UploadPolicy.Full -> R.string.android_settings_sync_upload_full
            UploadPolicy.Staged -> R.string.android_settings_sync_upload_staged
        },
    )

/** One rung of the upload ladder. */
@Composable
internal fun uploadTierLabel(tier: UploadTier): String =
    stringResource(
        when (tier) {
            UploadTier.Index -> R.string.android_settings_sync_tier_index
            UploadTier.Preview -> R.string.android_settings_sync_tier_preview
            UploadTier.Original -> R.string.android_settings_sync_tier_original
        },
    )

/** The quality of a network path, in the words a user would use for it. */
@Composable
internal fun connectionLabel(connection: ConnectionClass): String =
    stringResource(
        when (connection) {
            ConnectionClass.Unmetered -> R.string.android_settings_connection_unmetered
            ConnectionClass.Metered -> R.string.android_settings_connection_metered
            ConnectionClass.Constrained -> R.string.android_settings_connection_constrained
            ConnectionClass.Adverse -> R.string.android_settings_connection_adverse
            ConnectionClass.Offline -> R.string.android_settings_connection_offline
        },
    )

/**
 * What kind of place an import source is.
 *
 * A token outside the closed set is named as unrecognised rather than hidden: the row still
 * has a locator the user can read, and dropping it would make a folder they import from
 * vanish from the list of folders they import from.
 */
@Composable
internal fun sourceKindLabel(scope: ImportScope): String =
    stringResource(
        when (scope.sourceKind.knownOrNull) {
            SourceKind.CameraRoll -> R.string.android_settings_library_source_camera_roll
            SourceKind.Screenshots -> R.string.android_settings_library_source_screenshots
            SourceKind.AppCollection -> R.string.android_settings_library_source_app_collection
            SourceKind.Folder -> R.string.android_settings_library_source_folder
            SourceKind.WatchedDir -> R.string.android_settings_library_source_watched_dir
            SourceKind.RemovableVolume -> R.string.android_settings_library_source_removable_volume
            null -> R.string.android_settings_library_source_unknown
        },
    )

/**
 * An album's name, with catalogue copy standing in for the default album's absence of one.
 *
 * The default album is nameless in the data by design — it is surfaced as the library's
 * primary view rather than as a row called "Default" — so the name has to come from the
 * catalogue rather than from a placeholder written into the record.
 */
@Composable
internal fun albumLabel(album: AlbumChoice): String =
    album.name.ifBlank {
        stringResource(R.string.android_settings_library_default_album)
    }

/**
 * The English detail behind an `error.*` code, localized.
 *
 * Resolved by name rather than through a hand-written `when` over the catalogue's seventy-odd
 * codes: the mapping from key to resource is mechanical, and a hand-written table is a list
 * that silently stops covering codes added later. An unresolved code falls back to the
 * generic message rather than rendering the raw key at the user.
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

/** Joins two already-localized fragments of one row. */
internal const val SEPARATOR: String = " · "
