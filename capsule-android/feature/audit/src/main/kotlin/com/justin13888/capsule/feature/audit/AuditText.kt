package com.justin13888.capsule.feature.audit

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.LifecycleAction
import com.justin13888.capsule.core.model.ModerationEventKind
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

/**
 * A date-and-time formatter in the reader's own locale and zone.
 *
 * Audit timestamps are read as evidence — "was that before or after I lost the phone?" — so
 * they are rendered absolutely rather than as "3 days ago", which is unusable for exactly
 * that question.
 */
@Composable
internal fun rememberAuditTimeFormatter(): DateTimeFormatter {
    val locale = Locale.getDefault()
    val zone = ZoneId.systemDefault()
    return remember(locale, zone) {
        DateTimeFormatter.ofLocalizedDateTime(FormatStyle.MEDIUM).withLocale(locale).withZone(zone)
    }
}

/** [instant] as the reader's locale spells it. */
@Composable
internal fun formattedInstant(instant: Instant): String = rememberAuditTimeFormatter().format(instant)

/**
 * What one entry says happened.
 *
 * A token this build does not know is named as such rather than hidden. The closed action set
 * is versioned, so an unknown token means a newer client wrote it — and dropping the row
 * would tell the user nothing happened, which is the one thing the log must never say.
 */
@Composable
internal fun auditEntryLabel(entry: AuditEntry): String =
    when (entry) {
        is AuditEntry.Lifecycle -> entry.action?.let { stringResource(it.labelRes()) }
        is AuditEntry.Moderation -> entry.kind?.let { stringResource(it.labelRes()) }
    } ?: stringResource(R.string.android_audit_unknown_action)

/** The catalogue row naming one lifecycle action. */
private fun LifecycleAction.labelRes(): Int =
    when (this) {
        LifecycleAction.Create -> R.string.android_audit_action_create
        LifecycleAction.Replace -> R.string.android_audit_action_replace
        LifecycleAction.Delete -> R.string.android_audit_action_delete
        LifecycleAction.MetadataUpdate -> R.string.android_audit_action_metadata_update
        LifecycleAction.DerivativeAdd -> R.string.android_audit_action_derivative_add
        LifecycleAction.DerivativeReplace -> R.string.android_audit_action_derivative_replace
        LifecycleAction.TrashRestore -> R.string.android_audit_action_trash_restore
    }

/** The catalogue row naming one moderation event kind. */
private fun ModerationEventKind.labelRes(): Int =
    when (this) {
        ModerationEventKind.Takedown -> R.string.android_audit_moderation_takedown
        ModerationEventKind.TakedownLifted -> R.string.android_audit_moderation_takedown_lifted
        ModerationEventKind.LegalHold -> R.string.android_audit_moderation_legal_hold
        ModerationEventKind.Suspended -> R.string.android_audit_moderation_suspended
        ModerationEventKind.Unsuspended -> R.string.android_audit_moderation_unsuspended
    }

/** The filter chip's label for one origin. */
@Composable
internal fun auditCategoryLabel(category: AuditCategory): String =
    when (category) {
        AuditCategory.Lifecycle -> stringResource(R.string.android_audit_filter_lifecycle)
        AuditCategory.Moderation -> stringResource(R.string.android_audit_filter_moderation)
    }

/**
 * Who signed an entry, named rather than identified.
 *
 * A revoked device is still named: its directory row is never pruned, because every record
 * it signed is bound by its key, so "retired device" is a true and useful statement where a
 * blank would look like tampering.
 */
@Composable
internal fun auditSignerLabel(
    entry: AuditEntry,
    devices: Map<DeviceId, Device>,
): String {
    val signer = entry.signedBy ?: return stringResource(R.string.android_audit_actor_operator)
    val device = devices[signer] ?: return stringResource(R.string.android_audit_device_unknown)
    val name = device.displayName
    val base =
        when {
            device.isThisDevice -> stringResource(R.string.android_audit_device_this)
            !name.isNullOrBlank() -> name
            else -> stringResource(R.string.android_audit_device_unknown)
        }
    return if (device.isActive) base else base + SEPARATOR + stringResource(R.string.android_audit_device_revoked)
}

/**
 * The English detail behind an `error.*` code, localized.
 *
 * Resolved by name rather than through a hand-written `when` over the catalogue's seventy-odd
 * codes: the mapping from key to resource is mechanical (dots become underscores), and a
 * hand-written table is a list that silently stops covering codes added later. An unresolved
 * code falls back to the generic message rather than rendering the raw key at the user.
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
