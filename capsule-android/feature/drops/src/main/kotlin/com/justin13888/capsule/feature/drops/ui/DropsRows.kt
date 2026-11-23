package com.justin13888.capsule.feature.drops.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.capsuleTouchTarget
import com.justin13888.capsule.core.i18n.R

private val RowPadding = 16.dp
private val ControlSpacing = 12.dp

/**
 * One option in a single-choice group.
 *
 * The row carries the `selectable` semantics and the radio is inert, so the target is the
 * full width rather than a 20dp circle and a screen reader announces one thing rather than a
 * label and an unrelated control.
 *
 * @param label already-localized option text.
 * @param supporting already-localized detail under the label.
 */
@Composable
internal fun DropsRadioRow(
    label: String,
    selected: Boolean,
    onSelect: () -> Unit,
    modifier: Modifier = Modifier,
    supporting: String? = null,
) {
    Row(
        modifier =
            modifier
                .fillMaxWidth()
                .capsuleTouchTarget()
                .selectable(selected = selected, role = Role.RadioButton, onClick = onSelect)
                .padding(horizontal = RowPadding, vertical = ControlSpacing / 2),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        RadioButton(selected = selected, onClick = null)
        Spacer(Modifier.width(ControlSpacing))
        LabelWithSupport(label, supporting)
    }
}

/**
 * One on/off cap.
 *
 * Same shape as [DropsRadioRow] and for the same reason: the row is the control, and the
 * switch beside it only reports the state.
 *
 * @param label already-localized option text.
 * @param supporting already-localized detail under the label.
 */
@Composable
internal fun DropsSwitchRow(
    label: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
    supporting: String? = null,
) {
    Row(
        modifier =
            modifier
                .fillMaxWidth()
                .capsuleTouchTarget()
                .toggleable(value = checked, role = Role.Switch, onValueChange = onCheckedChange)
                .padding(horizontal = RowPadding, vertical = ControlSpacing / 2),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) { LabelWithSupport(label, supporting) }
        Spacer(Modifier.width(ControlSpacing))
        Switch(checked = checked, onCheckedChange = null)
    }
}

@Composable
private fun LabelWithSupport(
    label: String,
    supporting: String?,
) {
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

/** The shared back affordance, so every drops screen announces it the same way. */
@Composable
internal fun BackButton(onBack: () -> Unit) {
    IconButton(onClick = onBack) {
        Icon(
            imageVector = Icons.AutoMirrored.Filled.ArrowBack,
            contentDescription = stringResource(R.string.common_back),
        )
    }
}
