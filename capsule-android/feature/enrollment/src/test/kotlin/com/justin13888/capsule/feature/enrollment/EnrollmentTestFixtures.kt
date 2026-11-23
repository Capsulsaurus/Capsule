package com.justin13888.capsule.feature.enrollment

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag

/** Test handle for the stand-in the scanner is given where there is no camera. */
const val STUB_VIEWFINDER_TAG: String = "test.viewfinder"

/**
 * Something inert to fill the scanner's viewfinder slot.
 *
 * The slot exists precisely so this is possible: the real one binds a CameraX use case to a
 * lifecycle, which cannot resolve on a JVM, and everything the screen actually decides — the
 * permission primer, the reading state, the typed fallback — lives around it rather than
 * inside it.
 */
@Composable
fun StubViewfinder(modifier: Modifier = Modifier) {
    Box(modifier = modifier.fillMaxSize().testTag(STUB_VIEWFINDER_TAG))
}
