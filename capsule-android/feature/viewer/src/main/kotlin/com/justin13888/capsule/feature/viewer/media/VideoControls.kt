package com.justin13888.capsule.feature.viewer.media

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Replay
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.feature.viewer.ViewerFormat
import com.justin13888.capsule.feature.viewer.withArguments

/** Test handle for the transport strip, so a test can tell a clip from a still. */
const val VIDEO_CONTROLS_TAG: String = "viewer.video-controls"

/**
 * Play, pause and seek for a clip.
 *
 * There is no decoder behind this and the copy never pretends otherwise: the strip reports a
 * position from [PlaybackState] and nothing else. That is enough to build and test every
 * behaviour a transport control owes — pausing at the end rather than looping, rewinding when
 * play is pressed on a finished clip, seeking without starting playback — none of which need
 * a frame to be produced.
 *
 * @param state where the clip is.
 * @param onToggle starts, pauses, or restarts.
 * @param onSeek moves to a fraction of the clip.
 */
@Composable
fun VideoControls(
    state: PlaybackState,
    onToggle: () -> Unit,
    onSeek: (Float) -> Unit,
    modifier: Modifier = Modifier,
) {
    val positionLabel =
        stringResource(R.string.viewer_video_position).withArguments(
            "position" to ViewerFormat.clock(state.position),
            "duration" to ViewerFormat.clock(state.duration),
        )
    Column(
        modifier = modifier.fillMaxWidth().testTag(VIDEO_CONTROLS_TAG),
        verticalArrangement = Arrangement.spacedBy(ControlSpacing),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(ControlSpacing),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            CapsuleButton(
                onClick = onToggle,
                label = stringResource(state.transportLabel()),
                emphasis = CapsuleButtonEmphasis.Secondary,
                icon = state.transportIcon(),
            )
            Text(text = positionLabel, style = MaterialTheme.typography.labelLarge)
        }
        val scrubberLabel = stringResource(R.string.viewer_video_scrubber)
        Slider(
            value = state.progress,
            onValueChange = onSeek,
            modifier =
                Modifier
                    .fillMaxWidth()
                    .semantics { contentDescription = scrubberLabel },
        )
    }
}

/** Which of the three transport verbs applies right now. */
private fun PlaybackState.transportLabel(): Int =
    when {
        isPlaying -> R.string.viewer_video_pause
        isFinished -> R.string.viewer_video_replay
        else -> R.string.viewer_video_play
    }

/** The icon matching [transportLabel]. */
private fun PlaybackState.transportIcon() =
    when {
        isPlaying -> Icons.Filled.Pause
        isFinished -> Icons.Filled.Replay
        else -> Icons.Filled.PlayArrow
    }

private val ControlSpacing = 8.dp
