package com.justin13888.capsule.feature.drops.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.Upload
import androidx.compose.material3.Icon
import androidx.compose.material3.Scaffold
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBadge
import com.justin13888.capsule.core.designsystem.component.CapsuleBadgeTone
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
import com.justin13888.capsule.core.model.UploadLinkId
import com.justin13888.capsule.feature.drops.SEPARATOR
import com.justin13888.capsule.feature.drops.UploadLinkRow
import com.justin13888.capsule.feature.drops.UploadLinksUiState
import com.justin13888.capsule.feature.drops.failureMessage
import com.justin13888.capsule.feature.drops.failureTitle
import com.justin13888.capsule.feature.drops.formattedBytes
import com.justin13888.capsule.feature.drops.formattedDate
import com.justin13888.capsule.feature.drops.withArguments

/** Test tag prefix on an upload-link row; the link's opaque id is appended. */
const val UPLOAD_LINK_ROW_TAG: String = "upload-link-row-"

private val ListPadding = 16.dp

/**
 * Every upload link this library has provisioned.
 *
 * A row leads with what the link is *costing*: how much has arrived against how much it was
 * ever allowed to accept. That is the number a leaked link moves, and the only one that tells
 * the user whether to revoke it.
 */
@Composable
fun UploadLinksScreen(
    state: UploadLinksUiState,
    onOpenLink: (UploadLinkId) -> Unit,
    onCreate: () -> Unit,
    onRetry: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier.fillMaxSize(),
        topBar = {
            CapsuleTopAppBar(
                title = stringResource(R.string.android_destination_upload_links),
                navigationIcon = { BackButton(onBack) },
            )
        },
    ) { insets ->
        Column(modifier = Modifier.padding(insets).fillMaxSize()) {
            when {
                state.isLoading ->
                    CapsuleLoadingState(
                        title = stringResource(R.string.android_drops_links_loading),
                        contentDescription = stringResource(R.string.android_drops_links_loading),
                    )

                state.failure != null && state.links.isEmpty() ->
                    CapsuleErrorState(
                        title = failureTitle(state.failure),
                        body = failureMessage(state.failure),
                        action = {
                            CapsuleButton(onClick = onRetry, label = stringResource(R.string.android_drops_retry))
                        },
                    )

                state.isEmpty ->
                    CapsuleEmptyState(
                        title = stringResource(R.string.android_drops_links_empty_title),
                        body = stringResource(R.string.android_drops_links_empty_body),
                        icon = Icons.Filled.Upload,
                        action = {
                            CapsuleButton(
                                onClick = onCreate,
                                label = stringResource(R.string.android_destination_upload_link_create),
                            )
                        },
                    )

                else -> UploadLinkList(state, onOpenLink, onCreate, onRetry)
            }
        }
    }
}

@Composable
private fun UploadLinkList(
    state: UploadLinksUiState,
    onOpenLink: (UploadLinkId) -> Unit,
    onCreate: () -> Unit,
    onRetry: () -> Unit,
) {
    LazyColumn(modifier = Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = ListPadding)) {
        if (state.isDegraded) {
            item { DegradedBanner(state, onRetry) }
        }
        items(items = state.links, key = { it.link.id.value }) { row ->
            UploadLinkListItem(row = row, onClick = { onOpenLink(row.link.id) })
        }
        item {
            CapsuleButton(
                onClick = onCreate,
                label = stringResource(R.string.android_destination_upload_link_create),
                modifier = Modifier.fillMaxWidth().padding(ListPadding),
            )
        }
    }
}

@Composable
private fun DegradedBanner(
    state: UploadLinksUiState,
    onRetry: () -> Unit,
) {
    val failure = state.failure ?: return
    CapsuleBanner(
        title = failureTitle(failure),
        body = failureMessage(failure),
        severity = CapsuleBannerSeverity.Degraded,
        modifier = Modifier.fillMaxWidth().padding(ListPadding),
        action = {
            CapsuleButton(
                onClick = onRetry,
                label = stringResource(R.string.android_drops_retry),
                emphasis = CapsuleButtonEmphasis.Tertiary,
            )
        },
    )
}

@Composable
private fun UploadLinkListItem(
    row: UploadLinkRow,
    onClick: () -> Unit,
) {
    val expiry =
        row.link.caps.expiresAt
            ?.let { stringResource(R.string.share_expires).withArguments("date" to formattedDate(it)) }
            ?: stringResource(R.string.android_drops_cap_no_expiry)
    val passphrase =
        if (row.link.requiresPassphrase) stringResource(R.string.android_drops_passphrase_required) else null
    CapsuleListItem(
        headline = uploadLinkHeadline(row),
        supporting = listOfNotNull(expiry, passphrase).joinToString(SEPARATOR),
        leading = {
            Icon(
                imageVector = if (row.link.requiresPassphrase) Icons.Filled.Lock else Icons.Filled.Upload,
                contentDescription = null,
            )
        },
        trailing = {
            CapsuleBadge(
                label = uploadLinkStatus(row),
                tone = if (row.acceptsDeposits) CapsuleBadgeTone.Success else CapsuleBadgeTone.Degraded,
            )
        },
        onClick = onClick,
        modifier = Modifier.testTag(UPLOAD_LINK_ROW_TAG + row.link.opaqueId),
    )
}

/** What one link has taken in, stated against the ceiling it was given. */
@Composable
internal fun uploadLinkHeadline(row: UploadLinkRow): String {
    val used = formattedBytes(row.link.bytesReceived)
    val limit = row.link.caps.maxTotalBytes
    return if (limit == null) {
        stringResource(R.string.android_drops_cap_total_unbounded).withArguments("used" to used)
    } else {
        stringResource(R.string.android_drops_cap_total)
            .withArguments("used" to used, "limit" to formattedBytes(limit))
    }
}

/** Whether the link is still a door or only a record. */
@Composable
internal fun uploadLinkStatus(row: UploadLinkRow): String =
    if (row.acceptsDeposits) {
        stringResource(R.string.android_drops_link_open)
    } else {
        stringResource(R.string.android_drops_link_closed)
    }
