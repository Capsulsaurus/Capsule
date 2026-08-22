package com.justin13888.capsule

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.security.LocalAuthGate
import com.justin13888.capsule.shell.CapsuleAppShell
import com.justin13888.capsule.shell.CapsuleShellState
import org.koin.compose.koinInject

/**
 * The theme, then the shell, then everything else.
 *
 * The theme sits *outside* the shell rather than inside it so that the protocol-mismatch hard
 * block is themed too. That screen replaces the shell entirely, and an unthemed update prompt
 * on a dark-mode device would look like the app had crashed into a system dialog — which is
 * the one impression a screen asking the user to trust an update cannot afford to give.
 *
 * [CapsuleTheme] is left on its own defaults here: it already follows the system for dark
 * mode, Material You and contrast. The explicit user override lands with the appearance
 * settings page, and will be read from the settings repository at this level.
 *
 * @param onUpdateApp opens the store listing when the server has refused this build's
 *   protocol version. Supplied by the activity, which is the only layer that can start one.
 */
@Composable
fun CapsuleApp(
    onUpdateApp: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleTheme {
        CapsuleAppShell(
            shellState = koinInject<CapsuleShellState>(),
            gate = koinInject<LocalAuthGate>(),
            modifier = modifier,
            onUpdateApp = onUpdateApp,
        )
    }
}
