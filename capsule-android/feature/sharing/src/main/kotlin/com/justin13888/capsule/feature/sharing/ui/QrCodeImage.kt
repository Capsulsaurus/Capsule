package com.justin13888.capsule.feature.sharing.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.sizeIn
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.capsuleDescription
import com.justin13888.capsule.feature.sharing.qr.QrCode

/** How many light modules ring the symbol. Four is the standard's minimum. */
private const val QUIET_ZONE = 4

/** The smallest the symbol is allowed to render at and still scan across a table. */
private val MinimumSize = 180.dp

/** The largest it grows to, so it does not swallow a tablet's detail pane. */
private val MaximumSize = 320.dp

/**
 * Draws a QR symbol.
 *
 * Painted in fixed black on white rather than in theme colours, in both light and dark mode.
 * A scanner thresholds the image, so a symbol drawn in a dark theme's surface tones is a
 * symbol that fails to read on exactly the devices most likely to be pointed at it — the
 * contrast here is a functional requirement, not a styling choice.
 *
 * @param code the symbol to draw.
 * @param contentDescription already-localized announcement. A QR image is opaque to a screen
 *   reader, so the description has to say what the code is *for*; the URL itself is beside it
 *   as selectable text for anyone who cannot use the image at all.
 */
@Composable
fun QrCodeImage(
    code: QrCode,
    contentDescription: String,
    modifier: Modifier = Modifier,
) {
    val total = code.size + 2 * QUIET_ZONE
    Canvas(
        modifier =
            modifier
                .sizeIn(minWidth = MinimumSize, minHeight = MinimumSize)
                .size(MaximumSize)
                .aspectRatio(1f)
                .clip(MaterialTheme.shapes.medium)
                .capsuleDescription(contentDescription),
    ) {
        val module = size.minDimension / total
        drawRect(color = Color.White, size = Size(size.minDimension, size.minDimension))
        for (y in 0 until code.size) {
            for (x in 0 until code.size) {
                if (!code.isDark(x, y)) continue
                drawRect(
                    color = Color.Black,
                    topLeft = Offset((x + QUIET_ZONE) * module, (y + QUIET_ZONE) * module),
                    // A hair over one module, so neighbouring squares meet without a seam
                    // that anti-aliasing would otherwise turn into a light grid.
                    size = Size(module + 1f, module + 1f),
                )
            }
        }
    }
}
