package com.justin13888.capsule.shell

import androidx.annotation.StringRes
import androidx.compose.material3.SnackbarHostState
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import com.justin13888.capsule.core.data.ProtocolError
import com.justin13888.capsule.core.model.ConnectionClass

/**
 * The app-wide conditions the shell draws above every destination.
 *
 * Deliberately a single mutable holder rather than a flow per condition. These are *global
 * chrome* — they are true of the whole app, not of a screen — and a repository that discovers
 * one (the transport gets a `426`, the connectivity monitor drops to offline) needs one place
 * to say so. It is a singleton in the Koin graph for exactly that reason.
 *
 * Not a `ViewModel`: nothing here survives a process death usefully. A protocol refusal is
 * re-discovered on the next request, and connectivity is re-observed at once; restoring a
 * stale copy of either would be worse than starting blank.
 */
@Stable
class CapsuleShellState {
    /**
     * The current network path.
     *
     * Starts at [ConnectionClass.Unmetered] rather than offline so that a cold start never
     * flashes an offline banner before the first observation lands. Local Gallery NFR3 forbids
     * a blocking connectivity probe on startup, so there is a window where this is a guess,
     * and the optimistic guess is the one that does not lie to the user.
     */
    var connection: ConnectionClass by mutableStateOf(ConnectionClass.Unmetered)
        private set

    /**
     * The protocol handshake refusal that has hard-blocked the app, if one has arrived.
     *
     * Nullable and one-way in practice: once the server has answered `426` this build cannot
     * talk to it, and nothing the user does inside the app clears it.
     */
    var protocolRefusal: ProtocolError? by mutableStateOf(null)
        private set

    /** Host for transient messages. Conditions get banners; events get this. */
    val snackbars: SnackbarHostState = SnackbarHostState()

    /** Latest string resource handed to [showMessage], for tests and for the host to render. */
    @get:StringRes
    var pendingMessage: Int? by mutableStateOf(null)
        private set

    /** Record a new connection class observation. */
    fun onConnectionChanged(connection: ConnectionClass) {
        this.connection = connection
    }

    /**
     * Hard-block the app on a protocol handshake refusal.
     *
     * The clients contract is explicit that a `426` is honoured "by stopping the request,
     * never by silently downgrading", and that the UI must surface an actionable update
     * prompt. Raising it here replaces all content; there is no degraded mode to fall back to.
     */
    fun onProtocolRefused(error: ProtocolError) {
        protocolRefusal = error
    }

    /** Queue a transient message by catalog key. */
    fun showMessage(
        @StringRes message: Int,
    ) {
        pendingMessage = message
    }

    /** Acknowledge the message once it has been shown. */
    fun onMessageShown() {
        pendingMessage = null
    }

    /** Whether the offline banner should be showing. */
    val isOffline: Boolean get() = !connection.isUsable
}
