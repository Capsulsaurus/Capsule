package com.justin13888.capsule.feature.albums.ui.common

import androidx.compose.material3.AlertDialog
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.i18n.R

/**
 * The one dialog behind both "new album" and "rename album".
 *
 * Shared because the two differ only in their title and their starting text; giving them
 * separate implementations is how the create field ends up trimming whitespace and the
 * rename field does not.
 *
 * @param title already-localized heading.
 * @param initialName what the field starts with; empty for a new album.
 * @param onConfirm receives the trimmed name. Never called with a blank one — an album whose
 *   name is only spaces is indistinguishable from the nameless default album on screen.
 */
@Composable
internal fun AlbumNameDialog(
    title: String,
    initialName: String,
    onConfirm: (String) -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
    testTag: String = "albums.name-dialog",
) {
    var name by remember(initialName) { mutableStateOf(initialName) }
    val label = stringResource(R.string.albums_name_field)
    val confirm = stringResource(R.string.ios_common_create)
    val cancel = stringResource(R.string.common_cancel)
    AlertDialog(
        onDismissRequest = onDismiss,
        modifier = modifier.testTag(testTag),
        title = { Text(text = title) },
        text = {
            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                label = { Text(text = label) },
                singleLine = true,
                modifier = Modifier.testTag("$testTag:field"),
            )
        },
        confirmButton = {
            TextButton(
                onClick = { onConfirm(name.trim()) },
                enabled = name.isNotBlank(),
                modifier = Modifier.testTag("$testTag:confirm"),
            ) { Text(text = confirm) }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(text = cancel) }
        },
    )
}
