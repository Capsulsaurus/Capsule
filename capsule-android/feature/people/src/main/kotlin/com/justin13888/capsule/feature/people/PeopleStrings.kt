package com.justin13888.capsule.feature.people

import androidx.annotation.StringRes
import androidx.compose.runtime.Composable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.i18n.R

/**
 * Fills the `{name}` placeholders of a catalog message.
 *
 * The catalog is ICU MessageFormat and the Android generator copies simple placeholders
 * through verbatim, so `stringResource` hands back a template that something has to
 * substitute into. Deliberately the *simple* subset — no plural, no select, no number
 * skeletons — which is why every message this module added uses named placeholders only.
 *
 * Single-pass rather than repeated [String.replace] so a substituted value that itself
 * contains braces — a name somebody typed — cannot be rescanned and expanded a second time.
 * `core:ui` holds an identical helper for its own messages; it is `internal` there, and a
 * shared copy would have to live in a module this one must not edit.
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

/** What one smart album is called. */
@Composable
@ReadOnlyComposable
internal fun smartAlbumLabel(kind: SmartAlbumKind): String = stringResource(kind.titleRes)

/** The catalog key naming an album, kept beside the enum so a new kind cannot miss one. */
@get:StringRes
private val SmartAlbumKind.titleRes: Int
    get() =
        when (this) {
            SmartAlbumKind.Trips -> R.string.people_smart_album_trips
            SmartAlbumKind.Pets -> R.string.people_smart_album_pets
            SmartAlbumKind.Food -> R.string.people_smart_album_food
            SmartAlbumKind.Nature -> R.string.people_smart_album_nature
            SmartAlbumKind.BestOfYear -> R.string.people_smart_album_best_of_year
        }

/**
 * The headline for a failure.
 *
 * Offline borrows the shell's own wording rather than inventing a second phrasing for the
 * same condition — the user has already been told once, at the top of the window.
 */
@Composable
@ReadOnlyComposable
internal fun failureTitle(failure: PeopleFailure): String =
    when (failure) {
        PeopleFailure.Offline -> stringResource(R.string.android_shell_offline_title)
        else -> stringResource(R.string.people_error_title)
    }

/**
 * The detail for a failure, localized from the stable `error.*` code the repository carried.
 *
 * Never an HTTP status: a status says nothing the user can act on, and two deployments may
 * spell the same condition differently.
 */
@Composable
@ReadOnlyComposable
internal fun failureBody(failure: PeopleFailure): String =
    stringResource(
        when (failure) {
            PeopleFailure.Offline -> R.string.android_shell_offline_body
            PeopleFailure.Suspended -> R.string.error_moderation_account_suspended
            PeopleFailure.QuotaLocked -> R.string.error_quota_grace_locked
            PeopleFailure.Unsupported -> R.string.error_protocol_version_unsupported
            PeopleFailure.Missing -> R.string.common_error_generic
            PeopleFailure.Unknown -> R.string.common_error_generic
        },
    )
