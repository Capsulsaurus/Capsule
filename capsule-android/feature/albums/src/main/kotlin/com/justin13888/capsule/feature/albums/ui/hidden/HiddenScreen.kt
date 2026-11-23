package com.justin13888.capsule.feature.albums.ui.hidden

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
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
import com.justin13888.capsule.feature.albums.ui.common.AlbumsGallery
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags

/**
 * The Hidden view.
 *
 * Says what hiding *is* on the screen itself, because the alternative is a user who assumes a
 * hidden photo has been removed from their library or from a share they already sent. It has
 * not: hiding keeps the asset in its container, keeps it syncing, and leaves it reachable from
 * its stack or an existing share.
 */
@Composable
fun HiddenScreen(
    state: HiddenUiState,
    onBack: () -> Unit,
    onUnhide: (AssetId) -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(AlbumsTestTags.HIDDEN)) {
        CapsuleTopAppBar(
            title = stringResource(R.string.nav_hidden),
            navigationIcon = {
                IconButton(onClick = onBack) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                        contentDescription = stringResource(R.string.common_back),
                    )
                }
            },
        )
        CapsuleBanner(
            title = stringResource(R.string.albums_hidden_note),
            modifier = Modifier.padding(GUTTER),
            severity = CapsuleBannerSeverity.Info,
        )
        when (state) {
            HiddenUiState.Loading -> CapsuleLoadingState(modifier = Modifier.fillMaxSize())
            is HiddenUiState.Failed ->
                CapsuleErrorState(
                    title = stringResource(R.string.albums_error_load),
                    modifier = Modifier.fillMaxSize(),
                    action = { CapsuleButton(onClick = onRetry, label = stringResource(R.string.common_refresh)) },
                )

            is HiddenUiState.Content ->
                if (state.assets.isEmpty()) {
                    CapsuleEmptyState(
                        title = stringResource(R.string.albums_hidden_empty_title),
                        body = stringResource(R.string.albums_hidden_empty_body),
                        modifier = Modifier.fillMaxSize(),
                    )
                } else {
                    val unhide = stringResource(R.string.albums_hidden_unhide)
                    AlbumsGallery(
                        assets = state.assets,
                        contentDescription = stringResource(R.string.nav_hidden),
                        cellDescription = { unhide },
                        modifier = Modifier.fillMaxSize(),
                        onOpen = onUnhide,
                    )
                }
        }
    }
}

private val GUTTER = 16.dp
