package com.justin13888.capsule.i18n

import android.content.res.Resources
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.R

/**
 * The entry to the in-app language picker: a button that opens the list of languages.
 * [onChosen] runs after the language changed, and re-renders the app in it.
 */
@Composable
fun LanguagePickerButton(
    picker: AppLanguagePicker,
    onChosen: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var open by rememberSaveable { mutableStateOf(false) }
    TextButton(onClick = { open = true }, modifier = modifier) {
        Text(stringResource(R.string.settings_language_language_label))
    }
    if (open) {
        LanguagePickerDialog(
            picker = picker,
            onDismiss = { open = false },
            onChosen = {
                open = false
                onChosen()
            },
        )
    }
}

@Composable
private fun LanguagePickerDialog(
    picker: AppLanguagePicker,
    onDismiss: () -> Unit,
    onChosen: () -> Unit,
) {
    val current = remember { picker.current() }
    // The system resources keep the device's own locale, whatever language the app was given.
    val systemName = remember { AppLanguagePicker.endonym(Resources.getSystem().configuration.locales[0]) }
    val systemLabel =
        if (systemName.isEmpty()) {
            stringResource(R.string.settings_language_system_unknown)
        } else {
            stringResource(R.string.settings_language_system_default, systemName)
        }
    val choose = { tag: String? -> if (picker.choose(tag)) onChosen() else onDismiss() }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.settings_language_language_label)) },
        text = {
            Column(Modifier.selectableGroup().verticalScroll(rememberScrollState())) {
                LanguageOption(label = systemLabel, selected = current == null) { choose(null) }
                picker.supported.forEach { tag ->
                    LanguageOption(label = AppLanguagePicker.endonym(tag), selected = current == tag) { choose(tag) }
                }
            }
        },
        confirmButton = {},
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.app_common_cancel)) }
        },
    )
}

@Composable
private fun LanguageOption(
    label: String,
    selected: Boolean,
    onClick: () -> Unit,
) {
    Row(
        modifier =
            Modifier
                .fillMaxWidth()
                .selectable(selected = selected, onClick = onClick, role = Role.RadioButton)
                .padding(vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        RadioButton(selected = selected, onClick = null)
        Text(text = label, modifier = Modifier.padding(start = 16.dp))
    }
}
