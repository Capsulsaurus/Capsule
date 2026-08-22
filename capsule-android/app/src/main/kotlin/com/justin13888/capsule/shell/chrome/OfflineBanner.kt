package com.justin13888.capsule.shell.chrome

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CloudOff
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.i18n.R

/**
 * The "you are offline" strip above the content.
 *
 * A banner, not a snackbar, and not a blocking screen. Offline is a *condition* that lasts,
 * and Local Gallery FR4 requires degradation to be visible but never structural: the library
 * stays fully readable with no network, so the shell says so once, in place, and gets out of
 * the way. A snackbar would scroll away before the user worked out why an upload had not
 * moved; a modal would be a lie about what the app can still do.
 *
 * Carries no action on purpose. There is nothing the user can press that creates a network.
 */
@Composable
fun OfflineBanner(modifier: Modifier = Modifier) {
    CapsuleBanner(
        title = stringResource(R.string.android_shell_offline_title),
        modifier = modifier,
        body = stringResource(R.string.android_shell_offline_body),
        severity = CapsuleBannerSeverity.Degraded,
        icon = Icons.Filled.CloudOff,
    )
}
