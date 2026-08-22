package com.justin13888.capsule.core.designsystem.preview

import android.content.res.Configuration
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.theme.CapsuleContrast
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme

/**
 * Every width class, in both colour schemes, from one annotation.
 *
 * The widths are the Material window size-class breakpoints: 412dp is a phone, 700dp a
 * folded tablet or a split-screen pane, 900dp a tablet in landscape. This module's whole
 * value is that a component looks right everywhere, and the cheapest way to keep that
 * true is to make it impossible to preview a component in only one of those places.
 *
 * Dynamic colour is off in every preview: the wallpaper is not part of the design, and a
 * preview that silently recolours itself from the host machine is not reviewable.
 */
@Preview(name = "Compact light", widthDp = 412, uiMode = Configuration.UI_MODE_NIGHT_NO)
@Preview(name = "Compact dark", widthDp = 412, uiMode = Configuration.UI_MODE_NIGHT_YES)
@Preview(name = "Medium light", widthDp = 700, uiMode = Configuration.UI_MODE_NIGHT_NO)
@Preview(name = "Medium dark", widthDp = 700, uiMode = Configuration.UI_MODE_NIGHT_YES)
@Preview(name = "Expanded light", widthDp = 900, uiMode = Configuration.UI_MODE_NIGHT_NO)
@Preview(name = "Expanded dark", widthDp = 900, uiMode = Configuration.UI_MODE_NIGHT_YES)
annotation class CapsulePreviews

/**
 * Both raised contrast levels, light and dark.
 *
 * Separate from [CapsulePreviews] because it answers a different question. The width
 * previews ask "does this lay out"; these ask "is this still readable when the user has
 * told the system they need more separation" — a question no amount of resizing surfaces.
 */
@Preview(name = "Medium contrast light", uiMode = Configuration.UI_MODE_NIGHT_NO)
@Preview(name = "Medium contrast dark", uiMode = Configuration.UI_MODE_NIGHT_YES)
annotation class CapsuleMediumContrastPreviews

/** See [CapsuleMediumContrastPreviews]; the same idea at the highest contrast level. */
@Preview(name = "High contrast light", uiMode = Configuration.UI_MODE_NIGHT_NO)
@Preview(name = "High contrast dark", uiMode = Configuration.UI_MODE_NIGHT_YES)
annotation class CapsuleHighContrastPreviews

/** The padding a preview puts around its content so nothing is clipped by the frame. */
private val PreviewPadding = 16.dp

/**
 * Puts preview content inside a real [CapsuleTheme] on a real surface.
 *
 * A preview that renders a component on the tooling's default white background is
 * lying about a design whose entire premise is a near-achromatic surface family.
 */
@Composable
fun CapsulePreviewSurface(
    darkTheme: Boolean = isSystemInDarkTheme(),
    contrast: CapsuleContrast = CapsuleContrast.Standard,
    content: @Composable () -> Unit,
) {
    CapsuleTheme(darkTheme = darkTheme, dynamicColor = false, contrast = contrast) {
        Surface {
            Box(modifier = Modifier.padding(PreviewPadding)) { content() }
        }
    }
}
