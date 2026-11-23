package com.justin13888.capsule.feature.people

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleSheet
import com.justin13888.capsule.core.i18n.R
import kotlinx.collections.immutable.ImmutableList

/** Test handle for the sheet that names a grouping or renames a person. */
const val PEOPLE_NAME_SHEET_TAG: String = "people.nameSheet"

/** Test handle for the text box inside the naming sheet. */
const val PEOPLE_NAME_FIELD_TAG: String = "people.nameField"

/** Test handle for the sheet that merges two people. */
const val PEOPLE_MERGE_SHEET_TAG: String = "people.mergeSheet"

private val SheetPadding = 16.dp
private val SheetSpacing = 8.dp
private val SheetMaxHeight = 360.dp

/**
 * "Who is this?"
 *
 * Two ways out, because there are two situations. A grouping that is somebody already named
 * is one tap on their row — the common case once a library has a few people in it — and a
 * grouping that is somebody new is a name typed into the box. Renaming an existing person
 * shows only the box, since there is nobody to attach them to.
 *
 * The box is a raw Material text field, and the one place this module reaches past the design
 * system: `core:designsystem` has no text-field wrapper yet, and inventing a local one would
 * put a second answer to "what does a Capsule input look like" in a feature module.
 */
@Composable
fun PeopleNameSheet(
    target: NameTarget,
    people: ImmutableList<PersonCard>,
    onSubmit: (String) -> Unit,
    onAttach: (String) -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var name by rememberSaveable(target) { mutableStateOf(target.initialName) }
    CapsuleSheet(onDismissRequest = onDismiss, modifier = modifier.testTag(PEOPLE_NAME_SHEET_TAG)) {
        CapsuleSectionHeader(title = stringResource(R.string.people_name_title))
        OutlinedTextField(
            value = name,
            onValueChange = { name = it },
            modifier = Modifier.fillMaxWidth().padding(horizontal = SheetPadding).testTag(PEOPLE_NAME_FIELD_TAG),
            label = { Text(text = stringResource(R.string.people_name_label)) },
            singleLine = true,
        )
        Row(
            modifier = Modifier.fillMaxWidth().padding(SheetPadding),
            horizontalArrangement = Arrangement.spacedBy(SheetSpacing, Alignment.End),
        ) {
            CapsuleButton(
                onClick = onDismiss,
                label = stringResource(R.string.common_cancel),
                emphasis = CapsuleButtonEmphasis.Tertiary,
            )
            CapsuleButton(
                onClick = { onSubmit(name) },
                label = stringResource(R.string.people_name_save),
                enabled = name.isNotBlank(),
            )
        }
        if (target.clusterId != null && people.isNotEmpty()) {
            CapsuleSectionHeader(title = stringResource(R.string.people_hub_named))
            Column(
                modifier = Modifier.fillMaxWidth().heightIn(max = SheetMaxHeight).verticalScroll(rememberScrollState()),
            ) {
                people.forEach { person ->
                    CapsuleListItem(
                        headline = person.displayName,
                        supporting = photoCountLabel(person.photoCount),
                        onClick = { onAttach(person.personId) },
                        contentDescription = personDescription(person),
                    )
                }
            }
        }
    }
}

/**
 * "Who is this the same person as?"
 *
 * Framed as a merge rather than as a delete because that is what it is: every grouping the
 * source person holds moves onto the target and nothing is thrown away, so a mis-merge costs
 * a rename rather than a re-identification.
 */
@Composable
fun PeopleMergeSheet(
    source: PersonCard,
    candidates: ImmutableList<PersonCard>,
    onMergeInto: (String) -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val others = remember(candidates, source) { candidates.filterNot { it.personId == source.personId } }
    CapsuleSheet(onDismissRequest = onDismiss, modifier = modifier.testTag(PEOPLE_MERGE_SHEET_TAG)) {
        CapsuleSectionHeader(title = stringResource(R.string.people_merge_title))
        Text(
            text = stringResource(R.string.people_merge_body),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = SheetPadding),
        )
        if (others.isEmpty()) {
            Text(
                text = stringResource(R.string.people_merge_empty),
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.padding(SheetPadding),
            )
        }
        Column(
            modifier = Modifier.fillMaxWidth().heightIn(max = SheetMaxHeight).verticalScroll(rememberScrollState()),
        ) {
            others.forEach { person ->
                CapsuleListItem(
                    headline = person.displayName,
                    supporting = photoCountLabel(person.photoCount),
                    onClick = { onMergeInto(person.personId) },
                    contentDescription = personDescription(person),
                )
            }
        }
        Row(
            modifier = Modifier.fillMaxWidth().padding(SheetPadding),
            horizontalArrangement = Arrangement.spacedBy(SheetSpacing, Alignment.End),
        ) {
            CapsuleButton(
                onClick = onDismiss,
                label = stringResource(R.string.common_cancel),
                emphasis = CapsuleButtonEmphasis.Tertiary,
            )
        }
    }
}

/** "{count} photos", the count line under a person's name. */
@Composable
internal fun photoCountLabel(count: Int): String {
    val template = stringResource(R.string.people_person_count)
    return template.withArguments("count" to count)
}

/** The whole announcement for a person row, so a screen reader reads it as one thing. */
@Composable
internal fun personDescription(person: PersonCard): String =
    stringResource(R.string.people_a11y_person)
        .withArguments("name" to person.displayName, "count" to person.photoCount)
