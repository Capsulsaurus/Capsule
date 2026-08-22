package com.justin13888.capsule.core.ui.gallery

import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.snap
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBadge
import com.justin13888.capsule.core.designsystem.component.CapsuleBadgeTone
import com.justin13888.capsule.core.designsystem.component.capsuleDescription
import com.justin13888.capsule.core.designsystem.theme.CapsuleShapes
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.i18n.R

private val HeaderPadding = 16.dp
private val PillPadding = 10.dp
private val PillVerticalPadding = 4.dp

/**
 * The default day header: a date, a count, and an opaque bar to sit behind it.
 *
 * Offered rather than required — [JustifiedGallery] takes any composable for a header — but
 * it encodes two things a feature would otherwise have to rediscover. The pinned copy needs
 * an opaque background, because the in-content copy of the same header sits underneath it
 * and would otherwise show through. And it collapses while the grid is moving: a full-width
 * date bar is worth its space when you are reading and is in the way when you are
 * travelling, so at fling speed it shrinks to a pill that names the day and nothing else.
 *
 * @param info what the gallery is asking for; see [GalleryHeaderInfo].
 * @param label the already-formatted date. The gallery has no idea what a section means.
 * @param count how many assets the section holds; drawn as a badge, announced as text.
 */
@Composable
fun GallerySectionHeader(
    info: GalleryHeaderInfo,
    label: String,
    count: Int,
    modifier: Modifier = Modifier,
) {
    val reduceMotion = CapsuleTheme.reduceMotion
    val collapse by animateFloatAsState(
        targetValue = if (info.isCollapsed) 1f else 0f,
        animationSpec = if (reduceMotion) snap() else tween(durationMillis = COLLAPSE_MILLIS),
        label = "capsule-gallery-header-collapse",
    )
    val announced =
        stringResource(R.string.gallery_a11y_section)
            .withArguments("label" to label, "count" to count)

    Box(
        modifier =
            modifier
                .fillMaxWidth()
                .height(GalleryDefaults.HeaderHeight)
                .capsuleDescription(announced),
        contentAlignment = Alignment.CenterStart,
    ) {
        // Both forms are always composed and cross-faded rather than swapped. The header is
        // pinned, so a swap would relayout the one thing on screen that must not move.
        if (collapse < 1f) {
            ExpandedHeader(
                label = label,
                count = count,
                opaque = info.isPinned,
                alpha = 1f - collapse,
            )
        }
        if (collapse > 0f) {
            CollapsedHeader(label = label, alpha = collapse)
        }
    }
}

@Composable
private fun ExpandedHeader(
    label: String,
    count: Int,
    opaque: Boolean,
    alpha: Float,
) {
    val background = if (opaque) MaterialTheme.colorScheme.surface else Color.Transparent
    Row(
        modifier =
            Modifier
                .fillMaxWidth()
                .height(GalleryDefaults.HeaderHeight)
                .background(background.copy(alpha = background.alpha * alpha))
                .padding(horizontal = HeaderPadding),
        horizontalArrangement = Arrangement.spacedBy(HeaderPadding / 2),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.titleSmallEmphasized,
            color = MaterialTheme.colorScheme.onSurface.copy(alpha = alpha),
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f, fill = false),
        )
        CapsuleBadge(label = count.toString(), tone = CapsuleBadgeTone.Neutral)
    }
}

@Composable
private fun CollapsedHeader(
    label: String,
    alpha: Float,
) {
    Row(
        modifier =
            Modifier
                .padding(horizontal = HeaderPadding, vertical = PillVerticalPadding)
                .clip(CapsuleShapes.extraLarge)
                .background(MaterialTheme.colorScheme.surfaceVariant.copy(alpha = alpha))
                .padding(horizontal = PillPadding, vertical = PillVerticalPadding),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelLargeEmphasized,
            color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = alpha),
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

private const val COLLAPSE_MILLIS = 150
