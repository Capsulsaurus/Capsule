package com.justin13888.capsule.core.designsystem.component

import androidx.compose.material3.ContainedLoadingIndicator
import androidx.compose.material3.LinearWavyProgressIndicator
import androidx.compose.material3.LoadingIndicator
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import com.justin13888.capsule.core.designsystem.theme.LocalReduceMotion

/**
 * "Something is happening, and we cannot say how much of it is left."
 *
 * The expressive indeterminate indicator morphs through a sequence of shapes rather than
 * spinning. That motion is informational — it is the only signal that the app has not
 * hung — so it keeps running even under [LocalReduceMotion], which suppresses decoration
 * and not feedback.
 *
 * @param contained draws the indicator on its own container. Use it when the indicator
 *   sits over content rather than over a plain surface.
 * @param contentDescription announced label. Supply one whenever the indicator is the
 *   only thing on screen; a bare progress announcement leaves a screen-reader user with
 *   no idea what is loading.
 */
@Composable
fun CapsuleLoadingIndicator(
    modifier: Modifier = Modifier,
    contained: Boolean = false,
    contentDescription: String? = null,
) {
    val indicatorModifier = modifier.capsuleDescription(contentDescription)
    if (contained) {
        ContainedLoadingIndicator(modifier = indicatorModifier)
    } else {
        LoadingIndicator(modifier = indicatorModifier)
    }
}

/**
 * "This much of it is done."
 *
 * Determinate progress is everywhere in this app — an encrypted upload, a re-index, a
 * verification pass over a terabyte — and the wave is what distinguishes a transfer that
 * is moving from one that has stalled at 61%. Under [LocalReduceMotion] the wave
 * flattens to a plain bar: the number still moves, so no information is lost.
 *
 * @param progress 0f..1f, read as a lambda so a fast-moving transfer redraws without
 *   recomposing the screen around it.
 */
@Composable
fun CapsuleProgress(
    progress: () -> Float,
    modifier: Modifier = Modifier,
    contentDescription: String? = null,
) {
    val reduceMotion = LocalReduceMotion.current
    LinearWavyProgressIndicator(
        progress = progress,
        modifier = modifier.capsuleDescription(contentDescription),
        amplitude = { if (reduceMotion) 0f else 1f },
    )
}
