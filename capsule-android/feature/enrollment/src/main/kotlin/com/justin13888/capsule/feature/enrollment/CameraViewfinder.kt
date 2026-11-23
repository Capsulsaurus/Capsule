package com.justin13888.capsule.feature.enrollment

import androidx.camera.compose.CameraXViewfinder
import androidx.camera.core.CameraSelector
import androidx.camera.core.Preview
import androidx.camera.core.SurfaceRequest
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.lifecycle.awaitInstance
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalInspectionMode
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.capsuleDescription
import com.justin13888.capsule.core.i18n.R

/** Test handle for whatever is filling the viewfinder. */
const val VIEWFINDER_TAG: String = "enrollment.viewfinder"

/**
 * A real CameraX preview.
 *
 * The preview is real and the decode over it is not, which is the honest split for this build:
 * a viewfinder is the whole ergonomic question of this screen — does the frame sit where a
 * user's hands put the other phone — and that cannot be answered by a placeholder. What the
 * frame *contains* is a fixture, and no barcode library is pulled in to read it.
 *
 * The use case is bound to the composition's lifecycle owner and unbound when this leaves,
 * so navigating away releases the camera rather than leaving it warm behind another screen.
 *
 * Renders a marked stand-in under [LocalInspectionMode], where there is no camera at all.
 */
@Composable
internal fun CameraViewfinder(modifier: Modifier = Modifier) {
    val description = stringResource(R.string.android_enrollment_scan_viewfinder_description)
    if (LocalInspectionMode.current) {
        SimulatedViewfinder(modifier)
        return
    }
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    var surfaceRequest by remember { mutableStateOf<SurfaceRequest?>(null) }
    var provider by remember { mutableStateOf<ProcessCameraProvider?>(null) }

    LaunchedEffect(lifecycleOwner) {
        val cameraProvider = ProcessCameraProvider.awaitInstance(context)
        val preview =
            Preview.Builder().build().apply {
                setSurfaceProvider { request -> surfaceRequest = request }
            }
        cameraProvider.unbindAll()
        cameraProvider.bindToLifecycle(lifecycleOwner, CameraSelector.DEFAULT_BACK_CAMERA, preview)
        provider = cameraProvider
    }
    DisposableEffect(provider) {
        onDispose { provider?.unbindAll() }
    }

    Box(
        modifier = modifier.fillMaxSize().testTag(VIEWFINDER_TAG).capsuleDescription(description),
        contentAlignment = Alignment.Center,
    ) {
        val request = surfaceRequest
        if (request == null) {
            CapsuleLoadingState(
                title = stringResource(R.string.android_enrollment_scan_reading),
                contentDescription = description,
            )
        } else {
            CameraXViewfinder(surfaceRequest = request, modifier = Modifier.fillMaxSize())
        }
    }
}

/**
 * What stands in for the camera where there is none.
 *
 * Labelled as simulated rather than dressed up as a preview: a fake viewfinder that looked
 * real would make a broken camera indistinguishable from a working one in a screenshot.
 */
@Composable
private fun SimulatedViewfinder(modifier: Modifier = Modifier) {
    Box(
        modifier =
            modifier
                .fillMaxSize()
                .background(MaterialTheme.colorScheme.surfaceVariant)
                .testTag(VIEWFINDER_TAG),
        contentAlignment = Alignment.Center,
    ) {
        CapsuleLoadingState(
            title = stringResource(R.string.android_enrollment_scan_simulated_viewfinder),
            contentDescription = stringResource(R.string.android_enrollment_scan_simulated_viewfinder),
        )
    }
}
