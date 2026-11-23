package com.justin13888.capsule.feature.albums.ui.common

import androidx.compose.foundation.layout.Box
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import com.justin13888.capsule.core.designsystem.component.capsuleTouchTarget

/**
 * A one-of-N choice over a closed set.
 *
 * A plain button plus a menu rather than an exposed dropdown text field, and the reason is
 * the grammar: every picker in the builder chooses from a *closed* set, so a text field —
 * which invites typing something that is not in the set — would be the wrong affordance for
 * a surface whose whole point is that out-of-set values are structurally impossible.
 *
 * @param label what the current choice reads as.
 * @param options the closed set, in the order it is offered.
 * @param optionLabel the already-localized name of one option.
 */
@Composable
internal fun <T> PickerButton(
    label: String,
    options: List<T>,
    optionLabel: @Composable (T) -> String,
    onSelect: (T) -> Unit,
    testTag: String,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
) {
    var expanded by remember { mutableStateOf(false) }
    Box(modifier = modifier) {
        TextButton(
            onClick = { expanded = true },
            enabled = enabled,
            modifier = Modifier.capsuleTouchTarget().testTag(testTag),
        ) { Text(text = label) }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            options.forEach { option ->
                val text = optionLabel(option)
                DropdownMenuItem(
                    text = { Text(text = text) },
                    onClick = {
                        expanded = false
                        onSelect(option)
                    },
                    modifier = Modifier.testTag("$testTag:$text"),
                )
            }
        }
    }
}
