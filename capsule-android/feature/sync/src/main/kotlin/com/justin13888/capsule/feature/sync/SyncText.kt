package com.justin13888.capsule.feature.sync

import android.content.Context
import android.text.format.Formatter
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.QuarantineSurface
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.SyncScope
import com.justin13888.capsule.core.model.UploadPolicy
import com.justin13888.capsule.core.model.UploadTier
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
 * substitute. Deliberately the *simple* subset — no plural, no select — because the generator
 * refuses to emit those to Android in the first place.
 *
 * One pass rather than repeated [String.replace], so a substituted value that itself contains
 * braces cannot be rescanned and expanded a second time.
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
 * [instant] as the reader's locale and zone spell a date and time.
 *
 * Absolute rather than relative. "Last synced 3 days ago" is the wrong shape for a screen
 * whose entire job is to answer "is my library safe" — the user wants the date they can
 * compare against what they remember doing.
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

/** [bytes] as the reader's locale spells a file size. */
@Composable
internal fun formattedSize(bytes: Long): String {
    val context = LocalContext.current
    return remember(context, bytes) { Formatter.formatShortFileSize(context, bytes) }
}

/** What the current connection class allows, said in terms of consequence rather than name. */
@Composable
internal fun connectionLabel(connection: ConnectionClass): String =
    when (connection) {
        ConnectionClass.Unmetered -> stringResource(R.string.android_sync_connection_unmetered)
        ConnectionClass.Metered -> stringResource(R.string.android_sync_connection_metered)
        ConnectionClass.Constrained -> stringResource(R.string.android_sync_connection_constrained)
        ConnectionClass.Adverse -> stringResource(R.string.android_sync_connection_adverse)
        ConnectionClass.Offline -> stringResource(R.string.android_sync_connection_offline)
    }

/** How much of each new asset this library fetches. */
@Composable
internal fun scopeLabel(scope: SyncScope): String =
    when (scope) {
        SyncScope.MetadataOnly -> stringResource(R.string.android_sync_scope_metadata_only)
        SyncScope.MetadataAndThumbnails -> stringResource(R.string.android_sync_scope_metadata_thumbnails)
        SyncScope.MetadataThumbnailsAndOriginal ->
            stringResource(R.string.android_sync_scope_metadata_thumbnails_original)
    }

/** Whether upload sessions open eagerly or in tier order. */
@Composable
internal fun policyLabel(policy: UploadPolicy): String =
    when (policy) {
        UploadPolicy.Full -> stringResource(R.string.android_sync_policy_full)
        UploadPolicy.Staged -> stringResource(R.string.android_sync_policy_staged)
    }

/** One rung of the upload ladder, named as the design names it. */
@Composable
internal fun tierLabel(tier: UploadTier): String =
    when (tier) {
        UploadTier.Index -> stringResource(R.string.android_sync_tier_index)
        UploadTier.Preview -> stringResource(R.string.android_sync_tier_preview)
        UploadTier.Original -> stringResource(R.string.android_sync_tier_original)
    }

/** What one rung of the upload ladder carries, and what it waits for. */
@Composable
internal fun tierBody(tier: UploadTier): String =
    when (tier) {
        UploadTier.Index -> stringResource(R.string.android_sync_tier_index_body)
        UploadTier.Preview -> stringResource(R.string.android_sync_tier_preview_body)
        UploadTier.Original -> stringResource(R.string.android_sync_tier_original_body)
    }

/** One rung of the representation ladder. */
@Composable
internal fun rungLabel(tier: RepresentationTier): String =
    when (tier) {
        RepresentationTier.Lqip -> stringResource(R.string.android_sync_tier_lqip)
        RepresentationTier.Thumbnail -> stringResource(R.string.android_sync_tier_thumbnail)
        RepresentationTier.Preview -> stringResource(R.string.android_sync_tier_preview_rung)
        RepresentationTier.Original -> stringResource(R.string.android_sync_tier_original_rung)
    }

/** The heading one quarantine surface gets. */
@Composable
internal fun quarantineTitle(surface: QuarantineSurface?): String =
    when (surface) {
        QuarantineSurface.StaleRevival -> stringResource(R.string.android_sync_quarantine_title_stale_revival)
        QuarantineSurface.VerifyAssetReject -> stringResource(R.string.android_sync_quarantine_title_verify_reject)
        QuarantineSurface.MalformedSidecar -> stringResource(R.string.android_sync_quarantine_title_malformed)
        QuarantineSurface.RestoreChainConflict ->
            stringResource(R.string.android_sync_quarantine_title_restore_conflict)
        QuarantineSurface.OrphanedOriginal -> stringResource(R.string.android_sync_quarantine_title_orphaned)
        QuarantineSurface.FederationSoftFail -> stringResource(R.string.android_sync_quarantine_title_federation)
        QuarantineSurface.AlbumUpgradeStranded -> stringResource(R.string.android_sync_quarantine_title_album_upgrade)
        QuarantineSurface.PendingDropReview -> stringResource(R.string.android_sync_quarantine_title_drop_review)
        null -> stringResource(R.string.android_sync_quarantine_title_unknown)
    }

/** Why one item was held, in terms of what it would have cost to apply it. */
@Composable
internal fun quarantineReason(surface: QuarantineSurface?): String =
    when (surface) {
        QuarantineSurface.StaleRevival -> stringResource(R.string.android_sync_quarantine_reason_stale_revival)
        QuarantineSurface.VerifyAssetReject -> stringResource(R.string.android_sync_quarantine_reason_verify_reject)
        QuarantineSurface.MalformedSidecar -> stringResource(R.string.android_sync_quarantine_reason_malformed)
        QuarantineSurface.RestoreChainConflict ->
            stringResource(R.string.android_sync_quarantine_reason_restore_conflict)
        QuarantineSurface.OrphanedOriginal -> stringResource(R.string.android_sync_quarantine_reason_orphaned)
        QuarantineSurface.FederationSoftFail -> stringResource(R.string.android_sync_quarantine_reason_federation)
        QuarantineSurface.AlbumUpgradeStranded ->
            stringResource(R.string.android_sync_quarantine_reason_album_upgrade)
        QuarantineSurface.PendingDropReview -> stringResource(R.string.android_sync_quarantine_reason_drop_review)
        null -> stringResource(R.string.android_sync_quarantine_reason_unknown)
    }

/** The one thing the user can do about a held item. */
@Composable
internal fun quarantineActionLabel(action: QuarantineAction): String =
    when (action) {
        QuarantineAction.KeepMine -> stringResource(R.string.android_sync_quarantine_action_keep_mine)
        QuarantineAction.Inspect -> stringResource(R.string.android_sync_quarantine_action_inspect)
        QuarantineAction.ChooseHistory -> stringResource(R.string.android_sync_quarantine_action_choose_history)
        QuarantineAction.OpenInbox -> stringResource(R.string.android_sync_quarantine_action_open_inbox)
        QuarantineAction.Discard -> stringResource(R.string.android_sync_quarantine_action_discard)
    }

/**
 * The English detail behind an `error.*` code, localized.
 *
 * Resolved by name rather than through a hand-written `when` over the catalogue's codes: the
 * mapping from key to resource is mechanical, and a hand-written table silently stops covering
 * codes added later. An unresolved code falls back to the generic message rather than
 * rendering a raw key — or an HTTP status — at the user.
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
