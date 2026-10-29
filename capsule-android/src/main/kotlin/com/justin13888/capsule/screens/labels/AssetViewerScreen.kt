package com.justin13888.capsule.screens.labels

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.DrawStyle
import androidx.compose.ui.graphics.drawscope.Fill
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.justin13888.capsule.R
import com.justin13888.capsule.i18n.RegionalFormat
import com.justin13888.capsule.screens.domain.AssetRegion
import com.justin13888.capsule.screens.domain.RegionShape

/** The sample photo's aspect ratio: landscape 3:2, the commonest camera frame. */
private const val PREVIEW_ASPECT = 1.5f

/**
 * A minimal asset viewer: the asset's preview with its labelled regions drawn over it,
 * and the region editor below. A region is drawn with drag and pinch, or with TalkBack's
 * adjust actions in [RegionDraft.STEP] steps, and labelled through [LabelList]. The same
 * layout and feature set as the Apple client's viewer editor, in Material 3.
 *
 * The preview is a stand-in surface until the app reads a real library; regions are in
 * fractions of the display orientation, so they land on the real preview unchanged.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AssetViewerScreen(
    viewModel: LabelEditorViewModel,
    navigateBack: () -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.app_viewer_sample_title)) },
                navigationIcon = {
                    IconButton(onClick = navigateBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.back))
                    }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            RegionCanvas(state, viewModel)
            Text(stringResource(R.string.app_labels_title_regions), style = MaterialTheme.typography.titleMedium)
            RegionPicker(state, viewModel)
            if (state.draft != null) {
                DraftControls(state, viewModel)
            } else {
                DrawControls(state, viewModel)
                if (state.selectedRegionId != null) {
                    LabelList(
                        labels = state.labels,
                        enabled = !state.isWriting,
                        onAdd = viewModel::addLabel,
                        onRemove = viewModel::removeLabel,
                    )
                }
            }
            LabelFailureText(state.failure)
        }
    }
}

/** The preview, the regions over it, and the draft taking drag, pinch and adjust actions. */
@Composable
private fun RegionCanvas(
    state: LabelEditorState,
    viewModel: LabelEditorViewModel,
) {
    val colors = MaterialTheme.colorScheme
    val current by rememberUpdatedState(state)
    val draftDescription = stringResource(R.string.app_labels_regions_draft)
    val actions = adjustActions()
    Box(
        Modifier
            .fillMaxWidth()
            .aspectRatio(PREVIEW_ASPECT)
            .background(Brush.linearGradient(listOf(colors.secondaryContainer, colors.tertiaryContainer))),
    ) {
        val sampleDescription = stringResource(R.string.app_viewer_sample_description)
        Canvas(
            Modifier
                .fillMaxSize()
                .semantics { contentDescription = sampleDescription }
                .pointerInput(Unit) {
                    detectTapGestures { tap ->
                        if (current.draft != null) return@detectTapGestures
                        val hit = current.regions.lastOrNull { contains(it.shape, tap, size.width.toFloat(), size.height.toFloat()) }
                        viewModel.selectRegion(hit?.id)
                    }
                },
        ) {
            state.regions.forEach { region ->
                val selected = region.id == state.selectedRegionId
                drawShape(region.shape, if (selected) colors.primary else colors.onSurface, Stroke(if (selected) 6f else 3f))
            }
        }
        state.draft?.let { draft ->
            Canvas(
                Modifier
                    .fillMaxSize()
                    // Only while drawing, so a drag over the preview otherwise scrolls the page.
                    .pointerInput(Unit) {
                        detectTransformGestures { _, pan, zoom, _ ->
                            val drawn = current.draft ?: return@detectTransformGestures
                            viewModel.updateDraft(
                                drawn.moved(pan.x.toDouble() / size.width, pan.y.toDouble() / size.height).scaled(zoom.toDouble()),
                            )
                        }
                    }.semantics {
                        contentDescription = draftDescription
                        customActions =
                            actions.map { (label, step) ->
                                CustomAccessibilityAction(label) {
                                    current.draft?.let { viewModel.updateDraft(step(it)) }
                                    true
                                }
                            }
                    },
            ) {
                drawShape(draft.shape, colors.primary.copy(alpha = 0.3f), Fill)
                drawShape(draft.shape, colors.primary, Stroke(4f))
            }
        }
    }
}

/** TalkBack's adjust actions for a draft: one [RegionDraft.STEP] each way, larger and smaller. */
@Composable
private fun adjustActions(): List<Pair<String, (RegionDraft) -> RegionDraft>> {
    val step = RegionDraft.STEP
    return listOf(
        stringResource(R.string.app_labels_regions_adjust_move_left) to { it: RegionDraft -> it.moved(-step, 0.0) },
        stringResource(R.string.app_labels_regions_adjust_move_right) to { it: RegionDraft -> it.moved(step, 0.0) },
        stringResource(R.string.app_labels_regions_adjust_move_up) to { it: RegionDraft -> it.moved(0.0, -step) },
        stringResource(R.string.app_labels_regions_adjust_move_down) to { it: RegionDraft -> it.moved(0.0, step) },
        stringResource(R.string.app_labels_regions_adjust_grow) to { it: RegionDraft -> it.resized(step, step) },
        stringResource(R.string.app_labels_regions_adjust_shrink) to { it: RegionDraft -> it.resized(-step, -step) },
    )
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun RegionPicker(
    state: LabelEditorState,
    viewModel: LabelEditorViewModel,
) {
    if (state.regions.isEmpty()) {
        Text(stringResource(R.string.app_labels_regions_empty), style = MaterialTheme.typography.bodyMedium)
        return
    }
    val number = RegionalFormat.number(LocalContext.current)
    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        state.regions.forEachIndexed { index, region: AssetRegion ->
            FilterChip(
                selected = region.id == state.selectedRegionId,
                enabled = !state.isWriting && state.draft == null,
                onClick = { viewModel.selectRegion(region.id) },
                label = { Text(stringResource(R.string.app_labels_regions_item, number.format(index + 1))) },
            )
        }
    }
}

@Composable
private fun DrawControls(
    state: LabelEditorState,
    viewModel: LabelEditorViewModel,
) {
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedButton(onClick = { viewModel.startDrawing(RegionDraft.Kind.CIRCLE) }, enabled = !state.isWriting) {
            Text(stringResource(R.string.app_labels_regions_draw_circle))
        }
        OutlinedButton(onClick = { viewModel.startDrawing(RegionDraft.Kind.RECT) }, enabled = !state.isWriting) {
            Text(stringResource(R.string.app_labels_regions_draw_rect))
        }
        if (state.selectedRegionId != null) {
            TextButton(onClick = viewModel::deleteSelectedRegion, enabled = !state.isWriting) {
                Text(stringResource(R.string.app_labels_regions_delete), color = MaterialTheme.colorScheme.error)
            }
        }
    }
}

@Composable
private fun DraftControls(
    state: LabelEditorState,
    viewModel: LabelEditorViewModel,
) {
    var firstLabel by rememberSaveable { mutableStateOf("") }
    OutlinedTextField(
        value = firstLabel,
        onValueChange = { firstLabel = it },
        label = { Text(stringResource(R.string.app_labels_regions_first_label)) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth(),
    )
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Button(
            // Cleared only once stored, so a refused label keeps what was typed.
            onClick = { viewModel.finishDrawing(firstLabel) { firstLabel = "" } },
            enabled = !state.isWriting,
        ) {
            Text(stringResource(R.string.app_labels_regions_save))
        }
        TextButton(onClick = viewModel::cancelDrawing) {
            Text(stringResource(R.string.app_labels_regions_cancel))
        }
    }
}

/** Draw [shape] (fractions of the image) on a canvas of this size; a radius is of the shorter side. */
private fun DrawScope.drawShape(
    shape: RegionShape,
    color: Color,
    style: DrawStyle,
) {
    when (shape) {
        is RegionShape.Circle ->
            drawCircle(
                color = color,
                radius = shape.radius * minOf(size.width, size.height),
                center = Offset(shape.centerX * size.width, shape.centerY * size.height),
                style = style,
            )
        is RegionShape.Rect ->
            drawRect(
                color = color,
                topLeft = Offset(shape.left * size.width, shape.top * size.height),
                size = Size(shape.width * size.width, shape.height * size.height),
                style = style,
            )
    }
}

/** Whether a tap at [point] on a canvas of [width] x [height] pixels lands inside [shape]. */
internal fun contains(
    shape: RegionShape,
    point: Offset,
    width: Float,
    height: Float,
): Boolean {
    if (width <= 0f || height <= 0f) return false
    return when (shape) {
        is RegionShape.Circle -> {
            val radius = shape.radius * minOf(width, height)
            val dx = point.x - shape.centerX * width
            val dy = point.y - shape.centerY * height
            dx * dx + dy * dy <= radius * radius
        }
        is RegionShape.Rect -> {
            val x = point.x / width
            val y = point.y / height
            x >= shape.left && x <= shape.left + shape.width && y >= shape.top && y <= shape.top + shape.height
        }
    }
}
