package com.justin13888.capsule.feature.importer

import android.content.Context
import android.text.format.Formatter
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.DefaultAlbumRule
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.ImportDecision
import com.justin13888.capsule.core.model.ImportFileState
import com.justin13888.capsule.core.model.SourceKind

/**
 * Fills the `{name}` placeholders of a catalog message.
 *
 * The catalog is ICU MessageFormat and the Android generator copies simple placeholders
 * through verbatim, so `stringResource` hands back a template and something has to
 * substitute. Deliberately the *simple* subset — no plural, no select — because the
 * generator refuses to emit those to Android in the first place.
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
 * [bytes] as the reader's locale spells a file size.
 *
 * The platform formatter rather than a catalog key: unit names, separators and rounding are
 * all locale-dependent, and a hand-rolled "MB" would be wrong in most of them.
 */
@Composable
internal fun formattedSize(bytes: Long): String {
    val context = LocalContext.current
    return remember(context, bytes) { Formatter.formatShortFileSize(context, bytes) }
}

/** The name a source row shows — the kind, since the locator is the supporting text. */
@Composable
internal fun sourceLabel(source: ImportSource): String =
    when (source.kind) {
        SourceKind.CameraRoll -> stringResource(R.string.android_importer_source_camera_roll)
        SourceKind.Screenshots -> stringResource(R.string.android_importer_source_screenshots)
        SourceKind.AppCollection -> stringResource(R.string.android_importer_source_app_collection)
        SourceKind.Folder -> stringResource(R.string.android_importer_source_folder)
        SourceKind.WatchedDir -> stringResource(R.string.android_importer_source_watched_dir)
        SourceKind.RemovableVolume -> stringResource(R.string.android_importer_source_removable_volume)
        // A token this build does not know still names *something* the user picked, and the
        // locator underneath it is the honest identity, so the row stays usable.
        null -> stringResource(R.string.android_importer_source_folder)
    }

/** The album name to draw, with the nameless default album named. */
@Composable
internal fun albumLabel(name: String?): String =
    if (name.isNullOrBlank()) {
        stringResource(R.string.albums_default_album)
    } else {
        name
    }

/** Which rule produced the plan's destination, so a surprising one is explainable. */
@Composable
internal fun destinationRuleLabel(rule: DefaultAlbumRule?): String? =
    when (rule) {
        DefaultAlbumRule.ExplicitPick -> stringResource(R.string.android_importer_plan_rule_explicit)
        DefaultAlbumRule.ScopeOverride -> stringResource(R.string.android_importer_plan_rule_scope)
        DefaultAlbumRule.SourceKindDefault -> stringResource(R.string.android_importer_plan_rule_source_kind)
        DefaultAlbumRule.OwnerPointer -> stringResource(R.string.android_importer_plan_rule_owner)
        DefaultAlbumRule.DerivedDefault -> stringResource(R.string.android_importer_plan_rule_derived)
        null -> null
    }

/** Why the planner passed a candidate over, or handed it back as a question. */
@Composable
internal fun decisionLabel(decision: ImportDecision): String? =
    when (decision) {
        is ImportDecision.SkipDuplicate -> stringResource(R.string.android_importer_skip_duplicate)
        ImportDecision.SkipUnsupported -> stringResource(R.string.android_importer_skip_unsupported)
        is ImportDecision.SkipError -> stringResource(R.string.android_importer_skip_error)
        is ImportDecision.Import -> null
    }

/** Where one file has got to. */
@Composable
internal fun fileStateLabel(state: ImportFileState): String =
    when (state) {
        ImportFileState.Queued -> stringResource(R.string.android_importer_state_queued)
        ImportFileState.Processing -> stringResource(R.string.android_importer_state_processing)
        ImportFileState.Encrypting -> stringResource(R.string.android_importer_state_encrypting)
        ImportFileState.Uploading -> stringResource(R.string.android_importer_state_uploading)
        ImportFileState.Done -> stringResource(R.string.android_importer_state_done)
        ImportFileState.Failed -> stringResource(R.string.android_importer_state_failed)
    }

/**
 * The English detail behind an `error.*` code, localized.
 *
 * Resolved by name rather than through a hand-written `when` over the catalogue's codes: the
 * mapping from key to resource is mechanical, and a hand-written table silently stops
 * covering codes added later. An unresolved code falls back to the generic message rather
 * than rendering a raw key — or, worse, an HTTP status — at the user.
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
