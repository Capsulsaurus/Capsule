package com.justin13888.capsule.feature.search

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
 * contains braces — a tag, a camera model someone typed — cannot be rescanned and expanded
 * a second time. `core:ui` holds an identical helper for its own catalog messages; it is
 * `internal` there, and a shared copy would have to live in a module this one must not edit.
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

/** What one facet dimension is called. */
@Composable
@ReadOnlyComposable
internal fun facetKindLabel(kind: FacetKind): String = stringResource(kind.titleRes)

/** The catalog key naming a dimension, kept beside the enum so a new kind cannot miss one. */
@get:StringRes
private val FacetKind.titleRes: Int
    get() =
        when (this) {
            FacetKind.Date -> R.string.search_facet_date
            FacetKind.MediaKind -> R.string.search_facet_media
            FacetKind.Rating -> R.string.search_facet_rating
            FacetKind.Cull -> R.string.search_facet_cull
            FacetKind.Tag -> R.string.search_facet_tag
            FacetKind.Person -> R.string.search_facet_person
            FacetKind.Place -> R.string.search_facet_place
            FacetKind.Camera -> R.string.search_facet_camera
            FacetKind.Album -> R.string.search_facet_album
        }

/**
 * What one facet value is called.
 *
 * [FacetLabel.Data] is returned untranslated by design: it is a tag the user wrote, a name
 * they typed, or a manufacturer's product name, and translating any of those would be
 * mangling the library's own content.
 */
@Composable
@ReadOnlyComposable
internal fun facetValueLabel(label: FacetLabel): String =
    when (label) {
        is FacetLabel.Data -> label.text
        is FacetLabel.Stars -> stringResource(R.string.search_facet_value_stars).withArguments("count" to label.stars)
        is FacetLabel.Term -> stringResource(label.term.res)
    }

@get:StringRes
private val FacetTerm.res: Int
    get() =
        when (this) {
            FacetTerm.Image -> R.string.search_facet_value_image
            FacetTerm.Video -> R.string.search_facet_value_video
            FacetTerm.Unrated -> R.string.search_facet_value_unrated
            FacetTerm.Picked -> R.string.search_facet_value_pick
            FacetTerm.Unreviewed -> R.string.search_facet_value_neutral
            FacetTerm.Rejected -> R.string.search_facet_value_reject
            FacetTerm.Located -> R.string.search_facet_value_located
            FacetTerm.Unlocated -> R.string.search_facet_value_unlocated
            FacetTerm.NoCamera -> R.string.search_facet_value_no_camera
            FacetTerm.NoAlbum -> R.string.search_facet_value_no_album
            FacetTerm.DefaultAlbum -> R.string.search_facet_value_default_album
        }

/**
 * The headline for a failure.
 *
 * Offline borrows the shell's own wording rather than inventing a second phrasing for the
 * same condition — the user has already been told once, at the top of the window.
 */
@Composable
@ReadOnlyComposable
internal fun failureTitle(failure: SearchFailure): String =
    when (failure) {
        SearchFailure.Offline -> stringResource(R.string.android_shell_offline_title)
        else -> stringResource(R.string.search_error_title)
    }

/**
 * The detail for a failure, localized from the stable `error.*` code the repository carried.
 *
 * Never an HTTP status: a status says nothing the user can act on, and two deployments may
 * spell the same condition differently.
 */
@Composable
@ReadOnlyComposable
internal fun failureBody(failure: SearchFailure): String =
    stringResource(
        when (failure) {
            SearchFailure.Offline -> R.string.android_shell_offline_body
            SearchFailure.Suspended -> R.string.error_moderation_account_suspended
            SearchFailure.QuotaLocked -> R.string.error_quota_grace_locked
            SearchFailure.Unsupported -> R.string.error_protocol_version_unsupported
            SearchFailure.Unknown -> R.string.common_error_generic
        },
    )
