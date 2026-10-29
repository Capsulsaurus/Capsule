package com.justin13888.capsule.workspace

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
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
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.justin13888.capsule.App
import com.justin13888.capsule.R
import com.justin13888.capsule.ScreenPorts

/**
 * The app over this device's library: a wait while it unlocks, a failure with a retry
 * when it does not, and [App] over [ScreenPorts.rustBacked] once it is open, its
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
