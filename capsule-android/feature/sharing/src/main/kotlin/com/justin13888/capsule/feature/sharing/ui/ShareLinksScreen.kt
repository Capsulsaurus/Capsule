package com.justin13888.capsule.feature.sharing.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Link
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.ShareLinkId
import com.justin13888.capsule.feature.sharing.ShareLinkRow
import com.justin13888.capsule.feature.sharing.SharingUiState
import com.justin13888.capsule.feature.sharing.failureMessage
import com.justin13888.capsule.feature.sharing.failureTitle
import com.justin13888.capsule.feature.sharing.formattedDate
import com.justin13888.capsule.feature.sharing.withArguments

/** Test tag prefix on a share-link row; the link's opaque id is appended. */
const val SHARE_LINK_ROW_TAG: String = "share-link-row-"

private val ListPadding = 16.dp

/**
 * Every share link this library has issued and not revoked.
 *
 * Expired and revoked links are absent rather than greyed out. To a visitor those two and a
 * link that never existed answer identically, and a list that ranked them would be inventing
 * a distinction the server refuses to make — while giving the user a row to "un-expire" that
 * nothing can honour.
 */
@Composable
fun ShareLinksScreen(
    state: SharingUiState,
    onOpenLink: (ShareLinkId) -> Unit,
    onCreate: () -> Unit,
    onRetry: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier.fillMaxSize(),
        topBar = {
            CapsuleTopAppBar(
                title = stringResource(R.string.android_destination_share_links),
                navigationIcon = { BackButton(onBack) },
            )
        },
    ) { insets ->
        Column(modifier = Modifier.padding(insets).fillMaxSize()) {
            when {
                state.isLoading ->
                    CapsuleLoadingState(
                        title = stringResource(R.string.android_sharing_loading),
                        contentDescription = stringResource(R.string.android_sharing_loading),
                    )

                state.failure != null && state.shareLinks.isEmpty() ->
                    CapsuleErrorState(
                        title = failureTitle(state.failure),
                        body = failureMessage(state.failure),
                        action = {
                            CapsuleButton(
                                onClick = onRetry,
                                label = stringResource(R.string.android_sharing_retry),
                            )
                        },
                    )

                state.shareLinks.isEmpty() ->
                    CapsuleEmptyState(
                        title = stringResource(R.string.android_sharing_links_empty_title),
                        body = stringResource(R.string.android_sharing_links_empty_body),
                        icon = Icons.Filled.Link,
                        action = {
                            CapsuleButton(
                                onClick = onCreate,
                                label = stringResource(R.string.android_destination_share_link_create),
                            )
                        },
                    )

                else -> ShareLinkList(state, onOpenLink, onCreate, onRetry)
            }
        }
    }
}

/** The shared back affordance, so every sharing screen announces it the same way. */
@Composable
internal fun BackButton(onBack: () -> Unit) {
    IconButton(onClick = onBack) {
        Icon(
            imageVector = Icons.AutoMirrored.Filled.ArrowBack,
            contentDescription = stringResource(R.string.common_back),
        )
    }
}

@Composable
private fun ShareLinkList(
    state: SharingUiState,
    onOpenLink: (ShareLinkId) -> Unit,
    onCreate: () -> Unit,
    onRetry: () -> Unit,
) {
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(bottom = ListPadding),
    ) {
        if (state.isDegraded) {
            item {
                val failure = state.failure
                if (failure != null) {
                    CapsuleBanner(
                        title = failureTitle(failure),
                        body = failureMessage(failure),
                        severity = CapsuleBannerSeverity.Degraded,
                        modifier = Modifier.fillMaxWidth().padding(ListPadding),
                        action = {
                            CapsuleButton(
                                onClick = onRetry,
                                label = stringResource(R.string.android_sharing_retry),
                                emphasis = CapsuleButtonEmphasis.Tertiary,
                            )
                        },
                    )
                }
            }
        }
        items(items = state.shareLinks, key = { it.link.id.value }) { row ->
            ShareLinkListItem(row = row, onClick = { onOpenLink(row.link.id) })
        }
        item {
            CapsuleButton(
                onClick = onCreate,
                label = stringResource(R.string.android_destination_share_link_create),
                modifier = Modifier.fillMaxWidth().padding(ListPadding),
            )
        }
    }
}

@Composable
private fun ShareLinkListItem(
    row: ShareLinkRow,
    onClick: () -> Unit,
) {
    val scopeLabel = shareScopeLabel(row)
    val expiry =
        row.link.expiresAt
            ?.let { stringResource(R.string.share_expires).withArguments("date" to formattedDate(it)) }
            ?: stringResource(R.string.android_sharing_no_expiry)
    val passphrase =
        if (row.link.requiresPassphrase) {
            stringResource(R.string.android_sharing_passphrase_required)
        } else {
            null
        }
    CapsuleListItem(
        headline = scopeLabel,
        supporting = listOfNotNull(expiry, passphrase).joinToString(SEPARATOR_TEXT),
        leading = {
            Icon(
                imageVector = if (row.link.requiresPassphrase) Icons.Filled.Lock else Icons.Filled.Link,
                contentDescription = null,
            )
        },
        onClick = onClick,
        modifier = Modifier.testTag(SHARE_LINK_ROW_TAG + row.link.opaqueId),
    )
}

/** What one link says it shares — the album's own name where there is one. */
@Composable
internal fun shareScopeLabel(row: ShareLinkRow): String =
    when {
        !row.isAlbumScope -> stringResource(R.string.android_sharing_scope_asset)
        row.albumName != null -> row.albumName
        else -> stringResource(R.string.android_sharing_scope_album)
    }

/** Joins two already-localized fragments of one row. */
private const val SEPARATOR_TEXT: String = " · "
