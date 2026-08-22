package com.justin13888.capsule.core.ui.gallery

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CloudOff
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material.icons.filled.WarningAmber
import androidx.compose.material.icons.outlined.CloudUpload
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBadge
import com.justin13888.capsule.core.designsystem.component.CapsuleBadgeTone
import com.justin13888.capsule.core.designsystem.component.capsuleDescription
import com.justin13888.capsule.core.designsystem.theme.CapsuleShapes
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.i18n.R

/** What sync has to say about one asset. */
enum class GalleryCellSync {
    /** Nothing worth drawing — the common case, and the one that must cost nothing. */
    None,

    /** Queued, or uploaded but not yet acknowledged. */
    Pending,

    /** Sync gave up. */
    Failed,

    /** Stored and verified. */
    Synced,
}

/**
 * Everything drawn on top of a thumbnail that is not the thumbnail.
 *
 * One value rather than five parameters because the caller assembles it from a domain
 * object it owns and the gallery does not — `core:ui` must not depend on `core:model`, so
 * the chrome is described in terms the gallery can render and nothing more. Grouping also
 * means a cell that has no chrome at all passes one stable, equal instance and skips
 * recomposition entirely.
 *
 * @param stackCount how many frames a burst or stack holds, or `null` for a lone asset.
 * @param videoDuration an already-formatted running time such as `1:23`, or `null` for a still.
 * @param isFavourite whether to draw the heart.
 * @param sync what the sync marker should say.
 * @param isDegraded the original is not reachable and this is a derivative.
 */
@Immutable
data class GalleryCellMarkers(
    val stackCount: Int? = null,
    val videoDuration: String? = null,
    val isFavourite: Boolean = false,
    val sync: GalleryCellSync = GalleryCellSync.None,
    val isDegraded: Boolean = false,
) {
    /** True when there is nothing to draw, so the chrome can be skipped whole. */
    val isEmpty: Boolean
        get() =
            stackCount == null &&
                videoDuration == null &&
                !isFavourite &&
                sync == GalleryCellSync.None &&
                !isDegraded

    companion object {
        /** The shared empty instance. Passing this rather than a fresh copy skips recomposition. */
        val None: GalleryCellMarkers = GalleryCellMarkers()
    }
}

private val MarkerInset = 4.dp
private val MarkerIconSize = 16.dp

/** The scrim behind a corner marker, so a white icon survives a white photograph. */
private val MarkerScrim = Color.Black.copy(alpha = 0.45f)

/**
 * Draws the corner markers over a thumbnail.
 *
 * Everything is a corner rather than an overlay bar: at three-across a thumbnail is barely
 * a hundred dp wide, and anything that spans it is competing with the photograph rather
 * than annotating it. Each corner has one job — count top-end, duration bottom-end, state
 * bottom-start — so the eye learns where to look instead of reading the whole cell.
 *
 * Contains no clickable surface. The cell's own gestures belong to selection and to the
 * caller; a tap target inside a hundred-dp cell would be a mis-tap generator.
 */
@Composable
fun GalleryCellChrome(
    markers: GalleryCellMarkers,
    modifier: Modifier = Modifier,
) {
    if (markers.isEmpty) return
    Box(modifier = modifier.fillMaxSize()) {
        markers.stackCount?.let { count ->
            CapsuleBadge(
                label = stringResource(R.string.gallery_stack_count).withArguments("count" to count),
                modifier = Modifier.align(Alignment.TopEnd).padding(MarkerInset),
                tone = CapsuleBadgeTone.Neutral,
                contentDescription = stringResource(R.string.gallery_a11y_stack).withArguments("count" to count),
            )
        }
        markers.videoDuration?.let { duration ->
            Text(
                text = duration,
                style = CapsuleTheme.textStyles.metadataEmphasized,
                color = Color.White,
                modifier =
                    Modifier
                        .align(Alignment.BottomEnd)
                        .padding(MarkerInset)
                        .background(MarkerScrim, CapsuleShapes.extraSmall)
                        .padding(horizontal = MarkerInset, vertical = MarkerInset / 2)
                        .capsuleDescription(
                            stringResource(R.string.gallery_a11y_video_duration)
                                .withArguments("duration" to duration),
                        ),
            )
        }
        StateMarkers(
            markers = markers,
            modifier = Modifier.align(Alignment.BottomStart).padding(MarkerInset),
        )
    }
}

@Composable
private fun StateMarkers(
    markers: GalleryCellMarkers,
    modifier: Modifier = Modifier,
) {
    val colors = CapsuleTheme.colors
    Row(
        modifier = modifier,
        horizontalArrangement = Arrangement.spacedBy(MarkerInset),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (markers.isFavourite) {
            Icon(
                imageVector = Icons.Filled.Favorite,
                contentDescription = stringResource(R.string.gallery_a11y_favorite),
                tint = Color.White,
                modifier = Modifier.size(MarkerIconSize),
            )
        }
        when (markers.sync) {
            GalleryCellSync.None, GalleryCellSync.Synced -> Unit
            GalleryCellSync.Pending ->
                Icon(
                    imageVector = Icons.Outlined.CloudUpload,
                    contentDescription = stringResource(R.string.gallery_a11y_sync_pending),
                    tint = colors.syncPending.color,
                    modifier = Modifier.size(MarkerIconSize),
                )

            GalleryCellSync.Failed ->
                Icon(
                    imageVector = Icons.Filled.CloudOff,
                    contentDescription = stringResource(R.string.gallery_a11y_sync_failed),
                    tint = colors.syncFailed.color,
                    modifier = Modifier.size(MarkerIconSize),
                )
        }
        if (markers.isDegraded) {
            Icon(
                imageVector = Icons.Filled.WarningAmber,
                contentDescription = stringResource(R.string.gallery_a11y_degraded),
                tint = colors.degraded.color,
                modifier = Modifier.size(MarkerIconSize),
            )
        }
    }
}
