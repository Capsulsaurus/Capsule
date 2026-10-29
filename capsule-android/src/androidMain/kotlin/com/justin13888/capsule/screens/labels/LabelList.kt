package com.justin13888.capsule.screens.labels

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.InputChip
import androidx.compose.material3.InputChipDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.R
import com.justin13888.capsule.i18n.RegionalFormat
import com.justin13888.capsule.screens.domain.UserLabel

/**
 * A target's labels as removable chips, and a field that adds one. Used for a stack,
 * an album, and the selected region of an asset alike, since the label rule is the same
 * at every level.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun LabelList(
    labels: List<String>,
    enabled: Boolean,
    onAdd: (text: String, onStored: () -> Unit) -> Unit,
    onRemove: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    var typed by rememberSaveable { mutableStateOf("") }
    val submit = {
        // Cleared only once stored, so a refused label keeps what was typed.
        if (typed.isNotBlank()) onAdd(typed) { typed = "" }
    }
    Column(modifier, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        if (labels.isEmpty()) {
            Text(stringResource(R.string.app_labels_empty), style = MaterialTheme.typography.bodyMedium)
        }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            labels.forEach { label ->
                InputChip(
                    selected = false,
                    enabled = enabled,
                    onClick = { onRemove(label) },
                    label = { Text(label) },
                    trailingIcon = {
                        Icon(
                            Icons.Filled.Close,
                            contentDescription = stringResource(R.string.app_labels_remove, label),
                            modifier = Modifier.size(InputChipDefaults.IconSize),
                        )
                    },
                )
            }
        }
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedTextField(
                value = typed,
                // The byte bound is the store's to enforce, and its refusal names it.
                onValueChange = { typed = it },
                label = { Text(stringResource(R.string.app_labels_field)) },
                singleLine = true,
                enabled = enabled,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { submit() }),
                modifier = Modifier.weight(1f),
            )
            Button(onClick = submit, enabled = enabled && typed.isNotBlank()) {
                Text(stringResource(R.string.app_labels_add))
            }
        }
    }
}

/** The editor's refusal, worded for the user, or nothing when the last request succeeded. */
@Composable
fun LabelFailureText(failure: LabelFailure?) {
    val text =
        when (failure) {
            null -> return
            LabelFailure.NotFound -> stringResource(R.string.app_labels_failure_not_found)
            LabelFailure.LabelEmpty -> stringResource(R.string.app_labels_failure_label_empty)
            is LabelFailure.LabelTooLong -> {
                val number = RegionalFormat.number(LocalContext.current)
                stringResource(R.string.app_labels_failure_label_too_long, number.format(failure.bytes), number.format(UserLabel.MAX_BYTES))
            }
            is LabelFailure.ShapeRefused -> stringResource(R.string.app_labels_failure_shape_refused)
            LabelFailure.Unavailable -> stringResource(R.string.app_labels_failure_unavailable)
        }
    Text(text, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium)
}
