package com.justin13888.capsule.core.designsystem.component

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ErrorOutline
import androidx.compose.material.icons.filled.PhotoLibrary
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp

private val StatePadding = 32.dp
private val StateSpacing = 12.dp
private val StateIconSize = 48.dp

/**
 * Nothing here, and that is fine.
 *
 * Empty is the state a new install spends its first minutes in, so it is the first thing
 * a user reads about this product. It gets a real layout rather than a centred string.
 *
 * @param title already-localized headline.
 * @param body already-localized explanation of why it is empty and what to do about it.
 * @param action a slot for one [CapsuleButton].
 */
@Composable
fun CapsuleEmptyState(
    title: String,
    modifier: Modifier = Modifier,
    body: String? = null,
    icon: ImageVector = Icons.Filled.PhotoLibrary,
    action: (@Composable () -> Unit)? = null,
) {
    CapsuleStateScaffold(
        title = title,
        body = body,
        icon = icon,
        modifier = modifier,
        action = action,
    )
}

/**
 * Something failed and the user has to decide what happens next.
 *
 * Takes the same shape as [CapsuleEmptyState] on purpose: the two are swapped for each
 * other constantly as a screen loads, and a layout that jumps between them reads as a
 * second failure.
 *
 * @param title already-localized headline.
 * @param body already-localized detail. Pass the server's localized message here, not
 *   its error code.
 * @param action a slot for one [CapsuleButton] — normally a retry.
 */
@Composable
fun CapsuleErrorState(
    title: String,
    modifier: Modifier = Modifier,
    body: String? = null,
    icon: ImageVector = Icons.Filled.ErrorOutline,
    action: (@Composable () -> Unit)? = null,
) {
    CapsuleStateScaffold(
        title = title,
        body = body,
        icon = icon,
        modifier = modifier,
        tint = MaterialTheme.colorScheme.error,
        action = action,
    )
}

/**
 * Working on it.
 *
 * @param title already-localized text describing what is loading. Optional, because a
 *   spinner that appears for 200ms with a label under it flashes text at the user, and
 *   a caller that knows the wait is short should pass nothing.
 * @param contentDescription announced label for the indicator itself.
 */
@Composable
fun CapsuleLoadingState(
    modifier: Modifier = Modifier,
    title: String? = null,
    contentDescription: String? = null,
) {
    Column(
        modifier = modifier.fillMaxWidth().padding(StatePadding),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(StateSpacing, Alignment.CenterVertically),
    ) {
        CapsuleLoadingIndicator(contentDescription = contentDescription)
        if (title != null) {
            Text(
                text = title,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
        }
    }
}

@Composable
private fun CapsuleStateScaffold(
    title: String,
    body: String?,
    icon: ImageVector,
    modifier: Modifier = Modifier,
    tint: Color = MaterialTheme.colorScheme.onSurfaceVariant,
    action: (@Composable () -> Unit)? = null,
) {
    Column(
        modifier = modifier.fillMaxWidth().padding(StatePadding),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(StateSpacing, Alignment.CenterVertically),
    ) {
        Icon(
            imageVector = icon,
            contentDescription = null,
            tint = tint,
            modifier = Modifier.padding(bottom = StateSpacing).size(StateIconSize),
        )
        Text(
            text = title,
            style = MaterialTheme.typography.headlineSmallEmphasized,
            textAlign = TextAlign.Center,
        )
        if (body != null) {
            Text(
                text = body,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
        }
        action?.invoke()
    }
}
