package com.justin13888.capsule.feature.viewer

import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.geometry.Size
import com.justin13888.capsule.feature.viewer.media.ZoomPan

/**
 * The gesture state the photo and its control strip share.
 *
 * It exists because those two are not always neighbours: in tabletop posture the photo sits
 * above a physical hinge and the controls below it, so the zoom buttons are a separate
 * composable from the surface they zoom. Something has to hold the magnification and the
 * measured frame in common, and a holder passed to both beats a composition local that any
 * unrelated screen could read.
 *
 * Everything it holds is transient by design. Zoom resets when the user swipes to another
 * photo, because arriving at the next frame already magnified into a corner is disorienting
 * and there is no reading of "keep my zoom" that survives a change of subject.
 */
@Stable
class ViewerStageState {
    /** The current magnification and translation. Always clamped to [frame]. */
    var zoom: ZoomPan by mutableStateOf(ZoomPan.Fitted)
        private set

    /** The surface's measured size, as the last layout pass reported it. */
    var frame: Size by mutableStateOf(Size.Zero)
        private set

    /** Records a new measured size and re-clamps the current translation to it. */
    fun measure(size: Size) {
        if (size == frame) return
        frame = size
        zoom = zoom.copy(offset = ZoomPan.clampOffset(zoom.offset, zoom.scale, size))
    }

    /** Applies a position a gesture produced. */
    fun apply(next: ZoomPan) {
        zoom = next
    }

    /** One press of the zoom-in control. */
    fun zoomIn() {
        zoom = zoom.stepped(ZoomPan.STEP_FACTOR, frame)
    }

    /** One press of the zoom-out control. */
    fun zoomOut() {
        zoom = zoom.stepped(1f / ZoomPan.STEP_FACTOR, frame)
    }

    /** Returns to the fitted, centred position. */
    fun fit() {
        zoom = ZoomPan.Fitted
    }
}

/**
 * A [ViewerStageState] that resets whenever [pageKey] changes.
 *
 * Keyed on the asset rather than on the pager index so that a listing which shifts underneath
 * the viewer — an import landing, a sibling being trashed — does not silently re-point the
 * zoom at a different photograph.
 */
@Composable
fun rememberViewerStageState(pageKey: Any?): ViewerStageState = remember(pageKey) { ViewerStageState() }
