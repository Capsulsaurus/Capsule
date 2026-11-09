package com.justin13888.capsule.workspace

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.justin13888.capsule.App
import com.justin13888.capsule.R
import com.justin13888.capsule.ScreenPorts

/**
 * The app over this device's library: a wait while it unlocks, a failure with a retry
 * when it does not (and software keys beside it when the keystore refused the device
 * signing key), and [App] over [ScreenPorts.rustBacked] once it is open, its
 * transfer view following whichever session [WorkspaceGraph.sessions] holds.
 */
@Composable
fun LibraryHost(graph: WorkspaceGraph) {
    val library by graph.library.state.collectAsStateWithLifecycle()
    val signedIn by graph.sessions.current.collectAsStateWithLifecycle()
    when (val state = library) {
        is LibraryState.Ready -> {
            val opened = state.library
            val session = signedIn?.session
            val ports =
                remember(opened, session) {
                    ScreenPorts.rustBacked(opened.workspace, session, sampleAsset = opened.sampleAsset)
                }
            App(ports, sessions = graph.sessions)
        }
        LibraryState.Opening ->
            Status {
                CircularProgressIndicator()
                Text(stringResource(R.string.app_auth_common_loading))
            }
        is LibraryState.Failed ->
            if (state.hardwareRefused) {
                HardwareKeyRecovery(graph.library)
            } else {
                Status {
                    Text(
                        stringResource(if (state.locked) R.string.app_auth_common_defect_title else R.string.app_auth_common_error_title),
                        style = MaterialTheme.typography.titleMedium,
                    )
                    // A locked library stays locked: trying again cannot bring its secret back.
                    if (!state.locked) {
                        Button(onClick = graph.library::retry) { Text(stringResource(R.string.app_auth_common_retry)) }
                    }
                }
            }
    }
}

/**
 * The keystore refused the device signing key. A refusal is rare and sometimes transient, so
 * Retry comes first; software keys come second, with what they cost said plainly, because a
 * deviation the person was not told about is a downgrade rather than a choice.
 */
@Composable
private fun HardwareKeyRecovery(library: LibraryHolder) {
    Status {
        Text(
            stringResource(R.string.app_enrollment_failure_hardware_title),
            style = MaterialTheme.typography.titleMedium,
            textAlign = TextAlign.Center,
        )
        Text(
            stringResource(R.string.app_enrollment_failure_hardware_description),
            style = MaterialTheme.typography.bodyMedium,
            textAlign = TextAlign.Center,
        )
        Button(onClick = library::retry) { Text(stringResource(R.string.app_enrollment_failure_hardware_retry)) }
        OutlinedButton(onClick = library::continueWithSoftwareKeys) {
            Text(stringResource(R.string.app_enrollment_failure_hardware_software_keys))
        }
        Text(
            stringResource(R.string.app_enrollment_failure_hardware_deviation_note),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
    }
}

@Composable
private fun Status(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme()) {
        Surface(Modifier.fillMaxSize()) {
            Column(
                Modifier.fillMaxSize().padding(24.dp),
                verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) { content() }
        }
    }
}
