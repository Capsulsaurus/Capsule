package com.justin13888.capsule.feature.albums.ui.common

import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.i18n.R

/**
 * A confirmation in front of an irreversible-feeling step.
 *
 * "Feeling" is the operative word: deleting an album moves its photos back to the library
 * and emptying the trash only touches items already past their signed retention window, so
 * neither destroys anything the user cannot see. The dialog exists because both *read* as
 * destructive, and a user who is unsure stops trusting the rest of the surface.
 */
@Composable
internal fun ConfirmDialog(
    title: String,
    body: String,
    confirmLabel: String,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
    testTag: String = "albums.confirm-dialog",
) {
    val cancel = stringResource(R.string.common_cancel)
    AlertDialog(
        onDismissRequest = onDismiss,
        modifier = modifier.testTag(testTag),
        title = { Text(text = title) },
        text = { Text(text = body) },
        confirmButton = {
            TextButton(onClick = onConfirm, modifier = Modifier.testTag("$testTag:confirm")) {
                Text(text = confirmLabel)
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text(text = cancel) } },
    )
}
