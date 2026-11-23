package com.justin13888.capsule.feature.albums.ui.smart

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleChip
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.SortDirection
import com.justin13888.capsule.core.model.SortKey
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.common.refusalText
import com.justin13888.capsule.feature.albums.ui.common.violationText
import com.justin13888.capsule.feature.albums.ui.common.withArgs

/**
 * The smart-album builder.
 *
 * Three things are on screen at once and all three are load-bearing: the rule, the live count
 * of what it matches, and the list of structural rejections blocking a save. The count is
 * what tells a user their rule is *wrong* rather than merely unfinished — a predicate that
 * reads correctly and matches nothing is the common mistake — and the rejection list is
 * complete rather than first-only, because a definition can arrive from another device and
 * cannot be fixed one error at a time.
 */
@Composable
@Suppress("LongParameterList")
fun SmartAlbumBuilderScreen(
    state: SmartAlbumBuilderUiState,
    actions: RuleTreeActions,
    onBack: () -> Unit,
    onName: (String) -> Unit,
    onSortKey: (SortKey) -> Unit,
    onSortDirection: (SortDirection) -> Unit,
    onSave: () -> Unit,
    onDismissRefusal: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(AlbumsTestTags.BUILDER)) {
        CapsuleTopAppBar(
            title = stringResource(R.string.android_destination_smart_album_builder),
            navigationIcon = {
                IconButton(onClick = onBack) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                        contentDescription = stringResource(R.string.common_back),
                    )
                }
            },
            actions = {
                CapsuleButton(
                    onClick = onSave,
                    label = stringResource(R.string.common_save_changes),
                    enabled = state.canSave,
                    modifier = Modifier.testTag("${AlbumsTestTags.BUILDER}:save"),
                )
            },
        )
        when {
            state.isLoading -> CapsuleLoadingState(modifier = Modifier.fillMaxSize())
            state.isForwardSchema -> ForwardSchemaNotice()
            else ->
                BuilderBody(
                    state = state,
                    actions = actions,
                    onName = onName,
                    onSortKey = onSortKey,
                    onSortDirection = onSortDirection,
                    onDismissRefusal = onDismissRefusal,
                )
        }
    }
}

@Composable
private fun ForwardSchemaNotice() {
    CapsuleBanner(
        title = stringResource(R.string.albums_smart_newer_version),
        modifier = Modifier.padding(GUTTER).testTag("${AlbumsTestTags.BUILDER}:forward-schema"),
        body = stringResource(R.string.albums_smart_newer_version_body),
        severity = CapsuleBannerSeverity.Degraded,
    )
}

@Composable
private fun BuilderBody(
    state: SmartAlbumBuilderUiState,
    actions: RuleTreeActions,
    onName: (String) -> Unit,
    onSortKey: (SortKey) -> Unit,
    onSortDirection: (SortDirection) -> Unit,
    onDismissRefusal: () -> Unit,
) {
    val rows = flattenRule(state.draft.root)
    LazyColumn(modifier = Modifier.fillMaxSize()) {
        item { NameField(state.name, onName) }
        item { MatchCount(state) }
        item { state.refusal?.let { refusal -> RefusalBanner(refusalText(refusal), onDismissRefusal) } }
        item { Violations(state) }
        item { CapsuleSectionHeader(title = stringResource(R.string.albums_builder_rule)) }
        items(rows, key = { it.node.id.value }) { row ->
            RuleNodeRow(row = row, position = rows.indexOf(row) + 1, actions = actions)
        }
        item { SortControls(state, onSortKey, onSortDirection) }
    }
}

@Composable
private fun NameField(
    name: String,
    onName: (String) -> Unit,
) {
    val label = stringResource(R.string.albums_builder_name)
    OutlinedTextField(
        value = name,
        onValueChange = onName,
        label = { Text(text = label) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth().padding(GUTTER).testTag("${AlbumsTestTags.BUILDER}:name"),
    )
}

/**
 * How many photographs the rule currently matches.
 *
 * Says out loud when the figure came from a sample rather than the whole library: an
 * approximate count the user believes is exact is worse than no count at all.
 */
@Composable
private fun MatchCount(state: SmartAlbumBuilderUiState) {
    val count = state.matchCount
    val text =
        when {
            state.isCounting || count == null -> stringResource(R.string.albums_builder_counting)
            count.isComplete ->
                stringResource(R.string.albums_builder_live_count).withArgs("count" to count.matches)

            else ->
                stringResource(R.string.albums_builder_live_count).withArgs("count" to count.matches) +
                    " " +
                    stringResource(R.string.albums_builder_sampled).withArgs("count" to count.sampled)
        }
    Text(
        text = text,
        style = MaterialTheme.typography.titleMedium,
        modifier = Modifier.padding(horizontal = GUTTER).testTag(AlbumsTestTags.BUILDER_COUNT),
    )
}

@Composable
private fun RefusalBanner(
    text: String,
    onDismiss: () -> Unit,
) {
    CapsuleBanner(
        title = text,
        modifier = Modifier.padding(GUTTER).testTag("${AlbumsTestTags.BUILDER}:refusal"),
        severity = CapsuleBannerSeverity.Warning,
        action = { CapsuleButton(onClick = onDismiss, label = stringResource(R.string.common_close)) },
    )
}

@Composable
private fun Violations(state: SmartAlbumBuilderUiState) {
    if (state.violations.isEmpty()) return
    Column(modifier = Modifier.padding(GUTTER).testTag(AlbumsTestTags.BUILDER_VIOLATIONS)) {
        Text(
            text = stringResource(R.string.albums_builder_blocked),
            style = MaterialTheme.typography.titleSmall,
            color = MaterialTheme.colorScheme.error,
        )
        state.violations.forEach { violation ->
            Text(
                text = violationText(violation),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }
    }
}

@Composable
private fun SortControls(
    state: SmartAlbumBuilderUiState,
    onSortKey: (SortKey) -> Unit,
    onSortDirection: (SortDirection) -> Unit,
) {
    Column(modifier = Modifier.padding(GUTTER)) {
        CapsuleSectionHeader(title = stringResource(R.string.albums_builder_sort))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(CHIP_SPACING)) {
            SortKey.entries.forEach { key ->
                CapsuleChip(
                    label = sortKeyLabel(key),
                    selected = key == state.sortKey,
                    onSelectedChange = { onSortKey(key) },
                    modifier = Modifier.testTag("${AlbumsTestTags.BUILDER}:sort:${key.wire}"),
                )
            }
        }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(CHIP_SPACING)) {
            SortDirection.entries.forEach { direction ->
                CapsuleChip(
                    label = sortDirectionLabel(direction),
                    selected = direction == state.sortDirection,
                    onSelectedChange = { onSortDirection(direction) },
                    modifier = Modifier.testTag("${AlbumsTestTags.BUILDER}:direction:${direction.wire}"),
                )
            }
        }
    }
}

@Composable
private fun sortKeyLabel(key: SortKey): String =
    stringResource(
        when (key) {
            SortKey.CaptureTimestamp -> R.string.albums_builder_sort_capture
            SortKey.ImportTimestamp -> R.string.albums_builder_sort_import
            SortKey.Rating -> R.string.albums_builder_sort_rating
        },
    )

@Composable
private fun sortDirectionLabel(direction: SortDirection): String =
    stringResource(
        when (direction) {
            SortDirection.Ascending -> R.string.albums_builder_sort_oldest
            SortDirection.Descending -> R.string.albums_builder_sort_newest
        },
    )

private val GUTTER = 16.dp
private val CHIP_SPACING = 8.dp
