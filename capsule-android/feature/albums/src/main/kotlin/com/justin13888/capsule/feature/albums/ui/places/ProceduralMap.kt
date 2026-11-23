package com.justin13888.capsule.feature.albums.ui.places

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.feature.albums.data.PlaceClustering
import com.justin13888.capsule.feature.albums.model.MapBounds
import com.justin13888.capsule.feature.albums.model.PlaceCluster
import kotlin.math.abs
import kotlin.math.min
import kotlin.math.sqrt

/**
 * A map drawn from the data rather than fetched from anyone.
 *
 * No tiles, and deliberately no tile *provider*: a request for the tiles around a photograph
 * tells whoever serves them exactly where that photograph was taken, which is the single
 * most identifying thing a photo library holds. So the background is a graticule plus a
 * deterministic relief field, and the only real geography on screen is the user's own pins.
 *
 * It is honest about what it is. The graticule is labelled by position rather than dressed up
 * as coastline, so nobody mistakes it for a basemap and reads meaning into a shape that has
 * none.
 */
@Composable
internal fun ProceduralMap(
    clusters: List<PlaceCluster>,
    bounds: MapBounds,
    selected: PlaceCluster?,
    onSelect: (PlaceCluster?) -> Unit,
    modifier: Modifier = Modifier,
) {
    val description = stringResource(R.string.albums_places_map_a11y)
    val land = CapsuleTheme.colors.degraded.color
    val ink = CapsuleTheme.colors.selected.color
    Canvas(
        modifier =
            modifier
                .fillMaxWidth()
                .height(MAP_HEIGHT)
                .testTag("albums.places.map")
                .semantics { contentDescription = description }
                .pointerInput(clusters, bounds) {
                    val canvas = Size(size.width.toFloat(), size.height.toFloat())
                    detectTapGestures { tap -> onSelect(nearest(clusters, bounds, tap, canvas)) }
                },
    ) {
        drawRelief(land)
        drawGraticule(ink.copy(alpha = GRATICULE_ALPHA))
        clusters.forEach { cluster ->
            val (fx, fy) = PlaceClustering.project(cluster, bounds)
            val centre = Offset(fx * size.width, fy * size.height)
            val radius = pinRadius(cluster.count)
            drawCircle(color = ink.copy(alpha = PIN_FILL_ALPHA), radius = radius, center = centre)
            if (cluster == selected) {
                drawCircle(color = ink, radius = radius + SELECTION_RING, center = centre, style = RING)
            }
        }
    }
}

/**
 * A deterministic relief field standing in for terrain.
 *
 * Deterministic on purpose: the same window draws the same shape on every device and every
 * run, so a screenshot golden of this screen is stable and nobody has to wonder whether the
 * map "changed".
 */
private fun androidx.compose.ui.graphics.drawscope.DrawScope.drawRelief(colour: Color) {
    val step = size.width / RELIEF_COLUMNS
    for (column in 0 until RELIEF_COLUMNS) {
        for (row in 0 until RELIEF_ROWS) {
            val height = relief(column, row)
            if (height < RELIEF_THRESHOLD) continue
            drawRect(
                color = colour.copy(alpha = RELIEF_ALPHA * height),
                topLeft = Offset(column * step, row * (size.height / RELIEF_ROWS)),
                size = Size(step, size.height / RELIEF_ROWS),
            )
        }
    }
}

private fun androidx.compose.ui.graphics.drawscope.DrawScope.drawGraticule(colour: Color) {
    for (line in 1 until GRATICULE_DIVISIONS) {
        val x = size.width * line / GRATICULE_DIVISIONS
        val y = size.height * line / GRATICULE_DIVISIONS
        drawLine(colour, Offset(x, 0f), Offset(x, size.height), strokeWidth = GRATICULE_STROKE)
        drawLine(colour, Offset(0f, y), Offset(size.width, y), strokeWidth = GRATICULE_STROKE)
    }
}

/** A stable pseudo-elevation in `0..1` for one cell of the relief grid. */
private fun relief(
    column: Int,
    row: Int,
): Float {
    // A cheap integer hash rather than a noise library: it is stable, has no dependency, and
    // nothing on this canvas is claiming to be terrain in the first place.
    val mixed = (column * RELIEF_PRIME_X + row * RELIEF_PRIME_Y) % RELIEF_MODULUS
    return abs(mixed).toFloat() / RELIEF_MODULUS
}

/** The pin nearest a tap, or `null` when the tap missed every pin. */
private fun nearest(
    clusters: List<PlaceCluster>,
    bounds: MapBounds,
    tap: Offset,
    size: Size,
): PlaceCluster? =
    clusters
        .map { cluster ->
            val (fx, fy) = PlaceClustering.project(cluster, bounds)
            val dx = fx * size.width - tap.x
            val dy = fy * size.height - tap.y
            cluster to sqrt(dx * dx + dy * dy)
        }.filter { (_, distance) -> distance <= TAP_SLOP }
        .minByOrNull { (_, distance) -> distance }
        ?.first

/** Pin radius grows with the square root of the count, so area tracks the number. */
private fun pinRadius(count: Int): Float = min(MAX_PIN_RADIUS, MIN_PIN_RADIUS + sqrt(count.toFloat()) * PIN_GROWTH)

private val MAP_HEIGHT = 260.dp
private val RING =
    androidx.compose.ui.graphics.drawscope
        .Stroke(width = 2f)

private const val RELIEF_COLUMNS = 24
private const val RELIEF_ROWS = 14
private const val RELIEF_THRESHOLD = 0.55f
private const val RELIEF_ALPHA = 0.35f
private const val RELIEF_PRIME_X = 73_856_093
private const val RELIEF_PRIME_Y = 19_349_663
private const val RELIEF_MODULUS = 1_021
private const val GRATICULE_DIVISIONS = 8
private const val GRATICULE_STROKE = 1f
private const val GRATICULE_ALPHA = 0.25f
private const val PIN_FILL_ALPHA = 0.75f
private const val SELECTION_RING = 6f
private const val MIN_PIN_RADIUS = 6f
private const val MAX_PIN_RADIUS = 28f
private const val PIN_GROWTH = 2.5f
private const val TAP_SLOP = 48f
