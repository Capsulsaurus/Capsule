package com.justin13888.capsule

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import com.justin13888.capsule.feature.settings.CapsuleAppearanceHost
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
 * The theme comes from [CapsuleAppearanceHost] rather than from [CapsuleTheme]'s own
 * defaults. It still follows the system for dark mode, Material You and contrast when the
 * user has expressed no preference — but it reads the appearance settings, so a change made
 * on the settings page recolours the whole app rather than only the page that made it.
 *
 * @param onUpdateApp opens the store listing when the server has refused this build's
 *   protocol version. Supplied by the activity, which is the only layer that can start one.
 */
@Composable
fun CapsuleApp(
    onUpdateApp: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleAppearanceHost {
        CapsuleAppShell(
            shellState = koinInject<CapsuleShellState>(),
            gate = koinInject<LocalAuthGate>(),
            modifier = modifier,
            onUpdateApp = onUpdateApp,
        )
    }
}
