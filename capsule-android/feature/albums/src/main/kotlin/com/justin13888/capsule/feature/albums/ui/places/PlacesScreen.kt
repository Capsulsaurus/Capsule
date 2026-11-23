package com.justin13888.capsule.feature.albums.ui.places

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.feature.albums.model.PlaceCluster
import com.justin13888.capsule.feature.albums.ui.common.AlbumsGallery
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.common.withArgs

/**
 * Where the library was taken, on a map that never leaves the device.
 *
 * Tapping a pin fills the grid below it rather than opening a new screen, so comparing two
 * places is one tap each instead of a round trip — which is what people actually do here.
 */
@Composable
fun PlacesScreen(
    state: PlacesUiState,
    onBack: () -> Unit,
    onSelect: (PlaceCluster?) -> Unit,
    onOpenAsset: (AssetId) -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(AlbumsTestTags.PLACES)) {
        CapsuleTopAppBar(
            title = stringResource(R.string.android_destination_places),
            navigationIcon = {
                IconButton(onClick = onBack) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                        contentDescription = stringResource(R.string.common_back),
                    )
                }
            },
        )
        when (state) {
            PlacesUiState.Loading -> CapsuleLoadingState(modifier = Modifier.fillMaxSize())
            is PlacesUiState.Failed ->
                CapsuleErrorState(
                    title = stringResource(R.string.albums_error_load),
                    modifier = Modifier.fillMaxSize(),
                    action = { CapsuleButton(onClick = onRetry, label = stringResource(R.string.common_refresh)) },
                )

            is PlacesUiState.Content -> PlacesBody(state, onSelect, onOpenAsset)
        }
    }
}

@Composable
private fun PlacesBody(
    state: PlacesUiState.Content,
    onSelect: (PlaceCluster?) -> Unit,
    onOpenAsset: (AssetId) -> Unit,
) {
    val bounds = state.bounds
    if (bounds == null || state.clusters.isEmpty()) {
        CapsuleEmptyState(
            title = stringResource(R.string.albums_places_empty_title),
            body = stringResource(R.string.albums_places_empty_body),
            modifier = Modifier.fillMaxSize(),
        )
        return
    }
    ProceduralMap(
        clusters = state.clusters,
        bounds = bounds,
        selected = state.selected,
        onSelect = onSelect,
        modifier = Modifier.testTag(AlbumsTestTags.PLACES_MAP),
    )
    if (state.hasSecondDatum) {
        CapsuleBanner(
            title = stringResource(R.string.albums_places_gcj02_note),
            modifier = Modifier.padding(GUTTER).testTag("${AlbumsTestTags.PLACES}:datum"),
            severity = CapsuleBannerSeverity.Degraded,
        )
    }
    val selected = state.selected
    if (selected == null) {
        Text(
            text = stringResource(R.string.albums_places_empty_body),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(GUTTER),
        )
        return
    }
    val label = stringResource(R.string.albums_places_cluster_count).withArgs("count" to selected.count)
    Text(
        text = label,
        style = MaterialTheme.typography.titleMedium,
        modifier = Modifier.padding(horizontal = GUTTER).testTag("${AlbumsTestTags.PLACES}:selected"),
    )
    val announced =
        stringResource(R.string.albums_places_cluster_a11y).withArgs(
            "count" to selected.count,
            "latitude" to selected.latitude,
            "longitude" to selected.longitude,
        )
    AlbumsGallery(
        assets = state.selectedAssets,
        contentDescription = announced,
        cellDescription = { announced },
        modifier = Modifier.fillMaxSize(),
        onOpen = onOpenAsset,
    )
}

private val GUTTER = 16.dp
