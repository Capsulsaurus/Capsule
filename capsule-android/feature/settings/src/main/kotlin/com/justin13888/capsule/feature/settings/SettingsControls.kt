package com.justin13888.capsule.feature.settings

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.selection.selectable
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleCheckbox
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader

/** Test handle for one toggle: `settings.toggle:<id>`. */
fun settingsToggleTag(id: String): String = "settings.toggle:$id"

/** Test handle for one option of a choice: `settings.choice:<group>:<option>`. */
fun settingsChoiceTag(
    group: String,
    option: String,
): String = "settings.choice:$group:$option"

/**
 * One option of a single-choice group.
 *
 * A value type rather than a slot, because the group needs to know its options before it
 * composes them in order to announce the right position and count to a screen reader.
 */
@Immutable
class SettingsChoice<T>(
    /** The value this option selects. */
    val value: T,
    /** Already-localized label. */
    val label: String,
    /** A stable key for the test handle. */
    val key: String,
    /** Already-localized supporting line, where the label alone is not enough. */
    val supporting: String? = null,
    /** Whether this option can be chosen right now. */
    val enabled: Boolean = true,
)

/**
 * A setting that is on or off.
 *
 * The row owns the toggle rather than the other way round: the checkbox is handed a `null`
 * change handler so it stays out of the semantics tree, leaving exactly one tap target and
 * one announcement per row instead of a control the screen reader reads twice.
 *
 * @param title already-localized name of the setting.
 * @param summary already-localized explanation of what turning it on actually does.
 */
@Composable
fun SettingsToggleRow(
    id: String,
    title: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
    summary: String? = null,
    enabled: Boolean = true,
) {
    CapsuleListItem(
        headline = title,
        modifier = modifier.fillMaxWidth().testTag(settingsToggleTag(id)),
        supporting = summary,
        trailing = {
            CapsuleCheckbox(checked = checked, onCheckedChange = null, enabled = enabled)
        },
        onClick = if (enabled) ({ onCheckedChange(!checked) }) else null,
    )
}

/**
 * A setting with a closed set of values, exactly one of which is chosen.
 *
 * Rows with radio-button semantics rather than a chip row: a chip row announces each chip as
 * an independent toggle, which is a lie about a group where choosing one un-chooses the rest.
 *
 * @param title already-localized name of the group.
 * @param summary already-localized explanation, including anything the choice does *not* do.
 */
@Composable
fun <T> SettingsChoiceGroup(
    group: String,
    title: String,
    options: List<SettingsChoice<T>>,
    selected: T,
    onSelect: (T) -> Unit,
    modifier: Modifier = Modifier,
    summary: String? = null,
) {
    Column(modifier = modifier.fillMaxWidth()) {
        CapsuleSectionHeader(title = title)
        if (summary != null) SettingsNote(summary)
        options.forEach { option ->
            val isSelected = option.value == selected
            CapsuleListItem(
                headline = option.label,
                modifier =
                    Modifier
                        .fillMaxWidth()
                        .testTag(settingsChoiceTag(group, option.key))
                        .selectable(
                            selected = isSelected,
                            enabled = option.enabled,
                            role = Role.RadioButton,
                            onClick = { onSelect(option.value) },
                        ),
                supporting = option.supporting,
                trailing = if (isSelected) ({ Icon(Icons.Filled.Check, contentDescription = null) }) else null,
            )
        }
    }
}

/**
 * A value the user cannot change here.
 *
 * Rendered as a row rather than as prose so that a read-only fact — the protocol range, which
 * connection a tier needs — sits in the same column as the settings around it and is not
 * mistaken for one.
 */
@Composable
fun SettingsValueRow(
    label: String,
    value: String,
    modifier: Modifier = Modifier,
    supporting: String? = null,
) {
    CapsuleListItem(
        headline = value,
        modifier = modifier.fillMaxWidth(),
        supporting = supporting,
        overline = label,
        contentDescription = label + SEPARATOR + value,
    )
}

/**
 * An explanatory line under a heading.
 *
 * Its own composable so every page's explanation sits at the same inset and in the same
 * style. The copy is the point of these pages — most of these settings change what happens
 * to the user's files, and a bare label is not enough to decide on.
 */
@Composable
fun SettingsNote(
    text: String,
    modifier: Modifier = Modifier,
) {
    Text(
        text = text,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = modifier.fillMaxWidth().padding(horizontal = SettingsInset, vertical = SettingsNoteSpacing),
    )
}

/** The horizontal inset every settings page's prose lines up on with its rows. */
internal val SettingsInset = 16.dp

private val SettingsNoteSpacing = 4.dp
