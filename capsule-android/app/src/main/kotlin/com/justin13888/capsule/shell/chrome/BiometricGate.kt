package com.justin13888.capsule.shell.chrome

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Lock
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.GatedView
import com.justin13888.capsule.security.LocalAuthGate
import com.justin13888.capsule.security.LocalAuthPrompt
import kotlinx.coroutines.launch

/** Test handle for the locked cover, so a test can assert the gate is holding. */
const val BIOMETRIC_GATE_TAG: String = "shell.biometric-gate"

/**
 * Covers [content] until the user has proved who they are, for the five minutes after.
 *
 * Wraps rather than intercepts. The gated destinations (Trash, Hidden) are ordinary entries
 * in the entry provider, and the cover composes *instead of* their content rather than over
 * it, so the screen behind is never composed, never loads thumbnails, and therefore never
 * leaves decrypted previews in a cache for a snooper who backs out and comes in again.
 *
 * The prompt is raised on an explicit press, not automatically on arrival. An automatic
 * prompt at every recomposition is how gates train users to authenticate reflexively, which
 * is the habit this gate exists to interrupt.
 *
 * @param view which of the two gated views this is; grants never cross between them (SR1).
 * @param gate the grant store holding the grace window.
 * @param prompt the platform authenticator. Injected so tests can satisfy or refuse it.
 * @param content the real screen, composed only once the gate is open.
 */
@Composable
fun BiometricGate(
    view: GatedView,
    gate: LocalAuthGate,
    prompt: LocalAuthPrompt,
    modifier: Modifier = Modifier,
    content: @Composable () -> Unit,
) {
    var lastAttemptFailed by remember(view) { mutableStateOf(false) }
    // Read straight through on every recomposition rather than caching in a `remember`: the
    // grant expires against a wall clock, and a cached "open" would hold the view open past
    // its window until something unrelated happened to invalidate the cache. The grant map is
    // snapshot state, so revoking it — on backgrounding, say — recomposes this at once.
    if (gate.isOpen(view)) {
        content()
        return
    }

    val scope = rememberCoroutineScope()
    Box(
        modifier = modifier.fillMaxSize().testTag(BIOMETRIC_GATE_TAG),
        contentAlignment = Alignment.Center,
    ) {
        CapsuleEmptyState(
            title = stringResource(R.string.android_shell_gate_title),
            body =
                stringResource(
                    if (lastAttemptFailed) R.string.android_shell_gate_failed else R.string.android_shell_gate_body,
                ),
            icon = Icons.Filled.Lock,
        ) {
            CapsuleButton(
                onClick = {
                    scope.launch {
                        val authenticated = prompt.authenticate(view)
                        if (authenticated) gate.grant(view)
                        lastAttemptFailed = !authenticated
                    }
                },
                label = stringResource(R.string.android_shell_gate_unlock),
            )
        }
    }
}
