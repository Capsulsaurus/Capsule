package com.justin13888.capsule.feature.importer

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleSheet
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AlbumId
import kotlinx.collections.immutable.ImmutableList

/** Test handle for the one-time destination question. */
const val SCOPE_PROMPT_TAG: String = "importer.scope-prompt"

/**
 * The destination question a source asks once.
 *
 * A modal sheet rather than an inline choice on the plan screen, because this is not a
 * property of *this* import: the answer is remembered and every later import from the same
 * folder — including the automatic ones — is filed by it. A decision with that reach is worth
 * interrupting for, and worth stating the consequence of in the body copy.
 */
@Composable
fun ScopeMappingSheet(
    prompt: ScopeMappingPrompt,
    albums: ImmutableList<ImportAlbumChoice>,
    onChoose: (AlbumId) -> Unit,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleSheet(onDismissRequest = onDismiss, modifier = modifier.testTag(SCOPE_PROMPT_TAG)) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(SheetPadding),
            verticalArrangement = Arrangement.spacedBy(SheetSpacing),
        ) {
            Text(
                text =
                    stringResource(R.string.android_importer_scope_prompt_title)
                        .withArguments("source" to prompt.source.locator),
                style = MaterialTheme.typography.headlineSmallEmphasized,
            )
            Text(
                text = stringResource(R.string.android_importer_scope_prompt_body),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            LazyColumn(modifier = Modifier.heightIn(max = AlbumListMaxHeight)) {
                items(albums, key = { it.id.value }) { album ->
                    val chosen = album.id == prompt.chosen
                    CapsuleListItem(
                        headline = albumLabel(album.name),
                        modifier = Modifier.semantics { selected = chosen },
                        supporting =
                            if (album.isDefault) {
                                stringResource(R.string.albums_default_album_supporting)
                            } else {
                                null
                            },
                        onClick = { onChoose(album.id) },
                    )
                }
            }
            CapsuleButton(
                onClick = onConfirm,
                label = stringResource(R.string.android_importer_scope_prompt_confirm),
                modifier = Modifier.fillMaxWidth(),
                emphasis = CapsuleButtonEmphasis.Primary,
                enabled = prompt.chosen != null,
            )
            CapsuleButton(
                onClick = onDismiss,
                label = stringResource(R.string.common_cancel),
                modifier = Modifier.fillMaxWidth(),
                emphasis = CapsuleButtonEmphasis.Tertiary,
            )
        }
    }
}

/** The sheet's inner inset. */
private val SheetPadding = 24.dp

/** The gap between the sheet's stacked blocks. */
private val SheetSpacing = 12.dp

/** How tall the album list may grow before it scrolls inside the sheet. */
private val AlbumListMaxHeight = 320.dp
