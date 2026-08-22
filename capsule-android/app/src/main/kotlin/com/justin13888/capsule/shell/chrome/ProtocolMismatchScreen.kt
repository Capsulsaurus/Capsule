package com.justin13888.capsule.shell.chrome

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.SystemUpdateAlt
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.i18n.R

/** Test handle for the hard block, so a test can assert it replaced everything else. */
const val PROTOCOL_MISMATCH_TAG: String = "shell.protocol-mismatch"

/**
 * The full-window stop shown after the server answers `426 Upgrade Required`.
 *
 * This is a hard block by design, and the design docs are unusually blunt about why: the
 * client must honour a `426` "by stopping the request, never by silently downgrading", and
 * retrying the same payload after one is a forbidden client behaviour. A build outside the
 * server's accepted protocol window cannot write anything — no upload session is created, no
 * row is written — so an app that kept its normal chrome would be offering the user actions
 * that are all guaranteed to fail.
 *
 * It reads as an update prompt rather than an error for the same reason: the one refusal a
 * user can actually resolve, and the resolution is not inside this app.
 *
 * @param onUpdate opens the store listing. The shell wires this; the screen does not know how.
 */
@Composable
fun ProtocolMismatchScreen(
    onUpdate: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(modifier = modifier.fillMaxSize().testTag(PROTOCOL_MISMATCH_TAG)) {
        Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            CapsuleErrorState(
                title = stringResource(R.string.android_shell_protocol_block_title),
                body = stringResource(R.string.android_shell_protocol_block_body),
                icon = Icons.Filled.SystemUpdateAlt,
            ) {
                CapsuleButton(
                    onClick = onUpdate,
                    label = stringResource(R.string.android_shell_protocol_block_action),
                )
            }
        }
    }
}
