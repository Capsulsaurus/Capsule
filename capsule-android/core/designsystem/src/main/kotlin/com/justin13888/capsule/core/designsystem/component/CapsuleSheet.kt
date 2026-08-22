package com.justin13888.capsule.core.designsystem.component

import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.SheetState
import androidx.compose.material3.SheetValue
import androidx.compose.material3.rememberBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

/**
 * A modal surface that slides up over the content.
 *
 * The app's default answer to "this needs more room than a menu". A sheet keeps the
 * photograph visible behind it, which matters when the decision being made is about that
 * photograph — an album to file it in, a share scope, an export preset.
 *
 * @param sheetState hoisted so a caller can await the hide animation before committing a
 *   destructive action; dismissing a sheet and deleting the thing it described in the
 *   same frame is how a user ends up watching an item disappear with no undo in reach.
 */
@Composable
fun CapsuleSheet(
    onDismissRequest: () -> Unit,
    modifier: Modifier = Modifier,
    sheetState: SheetState = rememberBottomSheetState(initialValue = SheetValue.Hidden),
    content: @Composable ColumnScope.() -> Unit,
) {
    ModalBottomSheet(
        onDismissRequest = onDismissRequest,
        modifier = modifier,
        sheetState = sheetState,
        shape = MaterialTheme.shapes.extraLarge,
        content = content,
    )
}
