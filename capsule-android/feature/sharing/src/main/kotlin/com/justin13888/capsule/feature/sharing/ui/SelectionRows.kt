package com.justin13888.capsule.feature.sharing.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.selection.selectable
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.capsuleTouchTarget

private val RowPadding = 16.dp
private val ControlSpacing = 12.dp

/**
 * One option in a single-choice group.
 *
 * The whole row carries the `selectable` semantics and the radio button is inert, which is
 * what makes the target the full width rather than a 20dp circle and stops a screen reader
 * announcing the label and the control as two separate things. A row built the other way
 * round passes a visual review and fails every accessibility one.
 *
 * @param label already-localized option text.
 * @param supporting already-localized detail under the label.
 */
@Composable
internal fun CapsuleRadioRow(
    label: String,
    selected: Boolean,
    onSelect: () -> Unit,
    modifier: Modifier = Modifier,
    supporting: String? = null,
    enabled: Boolean = true,
) {
    Row(
        modifier =
            modifier
                .fillMaxWidth()
                .capsuleTouchTarget()
                .selectable(selected = selected, enabled = enabled, role = Role.RadioButton, onClick = onSelect)
                .padding(horizontal = RowPadding, vertical = ControlSpacing / 2),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        RadioButton(selected = selected, onClick = null, enabled = enabled)
        Spacer(Modifier.width(ControlSpacing))
        Column {
            Text(text = label, style = MaterialTheme.typography.bodyLarge)
            if (supporting != null) {
                Text(
                    text = supporting,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}
