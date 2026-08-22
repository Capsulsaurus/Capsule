package com.justin13888.capsule.core.designsystem.component

import androidx.compose.foundation.layout.BoxScope
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import com.justin13888.capsule.core.designsystem.theme.CapsuleSemanticColor
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme

/**
 * A small state marker attached to something else.
 *
 * @param label already-localized text, usually a count. `null` draws the dot form, which
 *   is what a thumbnail overlay wants — a number on a photograph is unreadable at grid
 *   size and steals attention at any size.
 * @param contentDescription announced label. Effectively mandatory for the dot form,
 *   which has nothing for a screen reader to read.
 */
@Composable
fun CapsuleBadge(
    label: String? = null,
    modifier: Modifier = Modifier,
    tone: CapsuleBadgeTone = CapsuleBadgeTone.Neutral,
    contentDescription: String? = null,
) {
    val semantic = tone.semanticColor()
    Badge(
        modifier = modifier.capsuleDescription(contentDescription),
        containerColor = semantic?.color ?: MaterialTheme.colorScheme.secondary,
        contentColor = semantic?.onColor ?: MaterialTheme.colorScheme.onSecondary,
        content = label?.let { { Text(text = it) } },
    )
}

/**
 * Anchors a [CapsuleBadge] to the top-end corner of [content].
 *
 * A thin pass-through, but it keeps features from reaching for `BadgedBox` and pairing a
 * Material badge with a Capsule one on the same screen.
 */
@Composable
fun CapsuleBadgedBox(
    badge: @Composable BoxScope.() -> Unit,
    modifier: Modifier = Modifier,
    content: @Composable BoxScope.() -> Unit,
) {
    BadgedBox(badge = badge, modifier = modifier, content = content)
}

/**
 * What a badge is reporting.
 *
 * Named after states rather than colours so that a feature says what it means and the
 * design system decides what that looks like — the whole reason the semantic colours
 * exist. A badge is the smallest thing on screen; if its meaning has to be inferred from
 * a hue, it has failed.
 */
enum class CapsuleBadgeTone {
    /** A plain count. Carries no state. */
    Neutral,

    /** Part of the current selection. */
    Selected,

    /** Queued, or uploaded but not yet acknowledged by the server. */
    SyncPending,

    /** Sync gave up on this item. */
    SyncFailed,

    /** Held by moderation and not visible to anyone it was shared with. */
    Quarantine,

    /** Present, but only as a derivative — the original is not reachable. */
    Degraded,

    /** Stored, verified, and durable. */
    Success,
}

@Composable
private fun CapsuleBadgeTone.semanticColor(): CapsuleSemanticColor? {
    val colors = CapsuleTheme.colors
    return when (this) {
        CapsuleBadgeTone.Neutral -> null
        CapsuleBadgeTone.Selected -> colors.selected
        CapsuleBadgeTone.SyncPending -> colors.syncPending
        CapsuleBadgeTone.SyncFailed -> colors.syncFailed
        CapsuleBadgeTone.Quarantine -> colors.quarantine
        CapsuleBadgeTone.Degraded -> colors.degraded
        CapsuleBadgeTone.Success -> colors.success
    }
}
