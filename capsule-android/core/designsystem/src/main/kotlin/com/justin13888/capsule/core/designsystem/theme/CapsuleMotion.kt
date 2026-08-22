package com.justin13888.capsule.core.designsystem.theme

import android.content.Context
import android.provider.Settings
import androidx.compose.runtime.staticCompositionLocalOf

/**
 * Whether decorative animation should be skipped.
 *
 * Read this before any animation that only expresses something already expressed — a
 * shape morph, a container transform, a parallax on a hero image. Animation that carries
 * information (a determinate progress bar, a drag) keeps running: honouring the
 * preference means removing decoration, not removing feedback.
 *
 * Defaults to `false` so a component outside [CapsuleTheme] still animates; the real
 * value comes from the system and is installed by [CapsuleTheme].
 */
val LocalReduceMotion = staticCompositionLocalOf { false }

/**
 * The system's animation preference.
 *
 * Android has no "reduce motion" switch of its own; the accessibility setting people
 * actually use is Developer Options' animation duration scale, which the platform also
 * drives to zero in battery saver. A scale of zero is the closest thing to an explicit
 * "stop animating" the OS offers, and every other value is treated as consent.
 */
internal fun Context.systemReduceMotion(): Boolean =
    Settings.Global.getFloat(contentResolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f) == 0f
