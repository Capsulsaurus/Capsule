package com.justin13888.capsule.feature.albums.ui.smart

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleBadge
import com.justin13888.capsule.core.designsystem.component.CapsuleBadgeTone
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.SmartAlbumId
import com.justin13888.capsule.core.model.SmartAlbumValidation
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.common.withArgs

/**
 * The list of saved searches, each with what it currently matches.
 *
 * A definition this build cannot evaluate keeps its row and gains a badge. Opening it in the
 * editor is refused rather than attempted: the editor has no controls for a grammar it does
 * not know, so it could only show the user a rule with pieces missing — and a save from that
 * screen would overwrite another device's work with a truncated copy.
 */
@Composable
fun SmartAlbumsScreen(
    state: SmartAlbumsUiState,
    onBack: () -> Unit,
    onCreate: () -> Unit,
    onEdit: (SmartAlbumId) -> Unit,
    onDelete: (SmartAlbumId) -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(AlbumsTestTags.SMART_LIST)) {
        CapsuleTopAppBar(
            title = stringResource(R.string.android_destination_smart_albums),
            navigationIcon = {
                IconButton(onClick = onBack) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                        contentDescription = stringResource(R.string.common_back),
                    )
                }
            },
            actions = {
                IconButton(onClick = onCreate, modifier = Modifier.testTag("${AlbumsTestTags.SMART_LIST}:create")) {
                    Icon(Icons.Filled.Add, contentDescription = stringResource(R.string.albums_smart_create))
                }
            },
        )
        when (state) {
            SmartAlbumsUiState.Loading -> CapsuleLoadingState(modifier = Modifier.fillMaxSize())
            is SmartAlbumsUiState.Failed ->
                CapsuleErrorState(
                    title = stringResource(R.string.albums_error_load),
                    modifier = Modifier.fillMaxSize(),
                    action = { CapsuleButton(onClick = onRetry, label = stringResource(R.string.common_refresh)) },
                )

            is SmartAlbumsUiState.Content ->
                if (state.rows.isEmpty()) {
                    EmptySmartAlbums(onCreate)
                } else {
                    LazyColumn(modifier = Modifier.fillMaxSize()) {
                        items(state.rows, key = { it.id.value }) { row -> SmartAlbumRowItem(row, onEdit, onDelete) }
                    }
                }
        }
    }
}

@Composable
private fun EmptySmartAlbums(onCreate: () -> Unit) {
    CapsuleEmptyState(
        title = stringResource(R.string.albums_smart_empty_title),
        body = stringResource(R.string.albums_smart_empty_body),
        modifier = Modifier.fillMaxSize(),
        action = { CapsuleButton(onClick = onCreate, label = stringResource(R.string.albums_smart_create)) },
    )
}

@Composable
private fun SmartAlbumRowItem(
    row: SmartAlbumRow,
    onEdit: (SmartAlbumId) -> Unit,
    onDelete: (SmartAlbumId) -> Unit,
) {
    val editable = row.validation is SmartAlbumValidation.Evaluable
    val forward = row.validation is SmartAlbumValidation.PreservedUnevaluated
    val supporting =
        when {
            forward -> stringResource(R.string.albums_smart_newer_version_body)
            row.matchCount != null ->
                stringResource(R.string.albums_smart_match_count).withArgs("count" to row.matchCount)

            else -> null
        }
    CapsuleListItem(
        headline = row.name,
        modifier = Modifier.testTag("${AlbumsTestTags.SMART_LIST}:${row.id.value}"),
        supporting = supporting,
        overline = if (forward) stringResource(R.string.albums_smart_newer_version) else null,
        onClick = if (editable) ({ onEdit(row.id) }) else null,
        trailing = {
            if (forward) {
                CapsuleBadge(
                    label = stringResource(R.string.albums_smart_newer_version),
                    tone = CapsuleBadgeTone.Neutral,
                )
            } else {
                IconButton(
                    onClick = { onDelete(row.id) },
                    modifier = Modifier.testTag("${AlbumsTestTags.SMART_LIST}:${row.id.value}:delete"),
                ) { Icon(Icons.Filled.Delete, contentDescription = stringResource(R.string.common_delete)) }
            }
        },
    )
}
