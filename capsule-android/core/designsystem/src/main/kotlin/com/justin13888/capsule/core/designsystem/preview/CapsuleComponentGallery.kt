package com.justin13888.capsule.core.designsystem.preview

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Share
import androidx.compose.material.icons.filled.Upload
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBadge
import com.justin13888.capsule.core.designsystem.component.CapsuleBadgeTone
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonGroup
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonGroupItem
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleCheckbox
import com.justin13888.capsule.core.designsystem.component.CapsuleChipState
import com.justin13888.capsule.core.designsystem.component.CapsuleDivider
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleFilterChipRow
import com.justin13888.capsule.core.designsystem.component.CapsuleFloatingToolbar
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingIndicator
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleProgress
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleSplitButton
import com.justin13888.capsule.core.designsystem.component.CapsuleToggleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleTooltip
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.designsystem.theme.CapsuleContrast
import com.justin13888.capsule.core.i18n.R
import kotlinx.collections.immutable.persistentListOf

private val GallerySpacing = 12.dp

/**
 * Every component in the module, on one scrollable surface.
 *
 * This exists so the design system can be reviewed as a whole rather than component by
 * component. Most visual regressions in a system like this are relational — a banner that
 * no longer matches a card, a badge whose tone collides with a chip's selected state —
 * and they only show up when the pieces are next to each other.
 *
 * Every string is a real catalog key. Inventing preview copy would let a component ship
 * with a layout that only works for text nobody will ever see.
 */
@Composable
internal fun CapsuleComponentGallery(modifier: Modifier = Modifier) {
    Column(
        modifier = modifier.fillMaxWidth().verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.spacedBy(GallerySpacing),
    ) {
        CapsuleTopAppBar(
            title = stringResource(R.string.nav_photos),
            subtitle = stringResource(R.string.share_scope_album),
        )
        ActionsGallery()
        SelectionGallery()
        StatusGallery()
        ContentGallery()
        StateGallery()
    }
}

@Composable
private fun ActionsGallery() {
    CapsuleSectionHeader(title = stringResource(R.string.assets_move_to_album))
    FlowRow(
        horizontalArrangement = Arrangement.spacedBy(GallerySpacing),
        verticalArrangement = Arrangement.spacedBy(GallerySpacing),
    ) {
        CapsuleButton(
            onClick = {},
            label = stringResource(R.string.common_upload),
            emphasis = CapsuleButtonEmphasis.Primary,
            icon = Icons.Filled.Upload,
        )
        CapsuleButton(
            onClick = {},
            label = stringResource(R.string.common_share),
            emphasis = CapsuleButtonEmphasis.Secondary,
            icon = Icons.Filled.Share,
        )
        CapsuleButton(
            onClick = {},
            label = stringResource(R.string.common_cancel),
            emphasis = CapsuleButtonEmphasis.Tertiary,
        )
        CapsuleButton(
            onClick = {},
            label = stringResource(R.string.common_delete),
            emphasis = CapsuleButtonEmphasis.Destructive,
            icon = Icons.Filled.Delete,
        )
    }
}

@Composable
private fun SelectionGallery() {
    var expanded by remember { mutableStateOf(false) }
    var toggled by remember { mutableStateOf(true) }
    var checked by remember { mutableStateOf(true) }

    CapsuleButtonGroup(
        items =
            persistentListOf(
                CapsuleButtonGroupItem(stringResource(R.string.nav_favorites), onClick = {}),
                CapsuleButtonGroupItem(stringResource(R.string.nav_archive), onClick = {}),
                CapsuleButtonGroupItem(stringResource(R.string.nav_trash), onClick = {}),
            ),
        overflowDescription = stringResource(R.string.explore_view_all),
    )
    CapsuleSplitButton(
        onClick = {},
        label = stringResource(R.string.common_share),
        expanded = expanded,
        onExpandedChange = { expanded = it },
        menuDescription = stringResource(R.string.explore_view_all),
    )
    CapsuleToggleButton(
        checked = toggled,
        onCheckedChange = { toggled = it },
        label = stringResource(R.string.ios_timeline_select),
    )
    CapsuleFilterChipRow(
        chips =
            persistentListOf(
                CapsuleChipState(stringResource(R.string.storage_category_photos), true, {}),
                CapsuleChipState(stringResource(R.string.storage_category_videos), false, {}),
                CapsuleChipState(stringResource(R.string.nav_favorites), false, {}),
            ),
    )
    CapsuleCheckbox(checked = checked, onCheckedChange = { checked = it })
}

@Composable
private fun StatusGallery() {
    CapsuleSectionHeader(title = stringResource(R.string.storage_title))
    CapsuleBanner(
        title = stringResource(R.string.drop_file_pending),
        body = stringResource(R.string.library_trash_description),
        severity = CapsuleBannerSeverity.Info,
    )
    CapsuleBanner(
        title = stringResource(R.string.drop_file_failed),
        body = stringResource(R.string.common_error_generic),
        severity = CapsuleBannerSeverity.Error,
        action = {
            CapsuleButton(
                onClick = {},
                label = stringResource(R.string.common_refresh),
                emphasis = CapsuleButtonEmphasis.Tertiary,
            )
        },
    )
    CapsuleBanner(
        title = stringResource(R.string.drop_file_done),
        severity = CapsuleBannerSeverity.Degraded,
    )
    FlowRow(horizontalArrangement = Arrangement.spacedBy(GallerySpacing)) {
        CapsuleBadgeTone.entries.forEach { tone ->
            CapsuleBadge(
                label = tone.ordinal.toString(),
                tone = tone,
                contentDescription = stringResource(R.string.common_loading),
            )
        }
    }
    CapsuleProgress(progress = { PREVIEW_PROGRESS })
    CapsuleLoadingIndicator(contentDescription = stringResource(R.string.common_loading))
}

@Composable
private fun ContentGallery() {
    CapsuleSectionHeader(title = stringResource(R.string.nav_albums))
    CapsuleCard(selected = true) {
        CapsuleListItem(
            headline = stringResource(R.string.label_title),
            supporting = stringResource(R.string.library_trash_description),
            overline = stringResource(R.string.nav_library),
            trailing = {
                CapsuleTooltip(text = stringResource(R.string.common_close)) {
                    IconButton(onClick = {}) {
                        Icon(
                            imageVector = Icons.Filled.Delete,
                            contentDescription = stringResource(R.string.common_delete),
                        )
                    }
                }
            },
        )
    }
    CapsuleDivider(inset = true)
    CapsuleFloatingToolbar(expanded = true) {
        IconButton(onClick = {}) {
            Icon(
                imageVector = Icons.Filled.Share,
                contentDescription = stringResource(R.string.common_share),
            )
        }
        IconButton(onClick = {}) {
            Icon(
                imageVector = Icons.Filled.Delete,
                contentDescription = stringResource(R.string.common_delete),
            )
        }
    }
}

@Composable
private fun StateGallery() {
    CapsuleEmptyState(
        title = stringResource(R.string.assets_empty),
        body = stringResource(R.string.library_archive_description),
        action = { CapsuleButton(onClick = {}, label = stringResource(R.string.common_upload)) },
    )
    CapsuleErrorState(
        title = stringResource(R.string.common_error_generic),
        action = {
            CapsuleButton(
                onClick = {},
                label = stringResource(R.string.common_refresh),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
    CapsuleLoadingState(
        title = stringResource(R.string.common_loading),
        contentDescription = stringResource(R.string.common_loading),
    )
}

/** A progress value chosen so the wave has somewhere to go in both directions. */
private const val PREVIEW_PROGRESS = 0.62f

@CapsulePreviews
@Composable
internal fun CapsuleComponentGalleryPreview() {
    CapsulePreviewSurface {
        CapsuleComponentGallery(modifier = Modifier.padding(bottom = GallerySpacing))
    }
}

@CapsuleMediumContrastPreviews
@Composable
internal fun CapsuleComponentGalleryMediumContrastPreview() {
    CapsulePreviewSurface(contrast = CapsuleContrast.Medium) { CapsuleComponentGallery() }
}

@CapsuleHighContrastPreviews
@Composable
internal fun CapsuleComponentGalleryHighContrastPreview() {
    CapsulePreviewSurface(contrast = CapsuleContrast.High) { CapsuleComponentGallery() }
}
