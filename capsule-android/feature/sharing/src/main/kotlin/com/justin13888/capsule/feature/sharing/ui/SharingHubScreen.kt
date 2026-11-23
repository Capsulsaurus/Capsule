package com.justin13888.capsule.feature.sharing.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Inbox
import androidx.compose.material.icons.filled.Link
import androidx.compose.material.icons.filled.Upload
import androidx.compose.material3.Icon
import androidx.compose.material3.Scaffold
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
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
import com.justin13888.capsule.core.designsystem.component.CapsuleDivider
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.feature.sharing.SharingUiState
import com.justin13888.capsule.feature.sharing.failureMessage
import com.justin13888.capsule.feature.sharing.failureTitle
import com.justin13888.capsule.feature.sharing.withArguments

/** Test tag on the hub's share-links row. */
const val SHARING_HUB_SHARE_LINKS_TAG: String = "sharing-hub-share-links"

/** Test tag on the hub's upload-links row. */
const val SHARING_HUB_UPLOAD_LINKS_TAG: String = "sharing-hub-upload-links"

/** Test tag on the hub's drop-inbox row. */
const val SHARING_HUB_DROP_INBOX_TAG: String = "sharing-hub-drop-inbox"

private val ContentSpacing = 8.dp
private val ContentPadding = 16.dp

/**
 * Everything the hub can be asked to do.
 *
 * Bundled rather than passed as six separate parameters so the screen keeps a signature a
 * preview and a test can call without ceremony, and so adding a seventh destination later is
 * a change to one type rather than to every call site.
 */
@Immutable
class SharingHubActions(
    /** Opens the full list of issued share links. */
    val openShareLinks: () -> Unit,
    /** Opens the list of provisioned upload links. */
    val openUploadLinks: () -> Unit,
    /** Opens the inbox of guest deposits awaiting review. */
    val openDropInbox: () -> Unit,
    /** Starts minting a share link. */
    val createShareLink: () -> Unit,
    /** Starts provisioning an upload link. */
    val createUploadLink: () -> Unit,
    /** Re-reads everything after a failure. */
    val retry: () -> Unit,
)

/**
 * The sharing hub: what this library hands out, and what is waiting to come in.
 *
 * The three rows are kept apart rather than merged into one "links" list. A share link
 * exposes what is already here and an upload link accepts bytes from someone who can see
 * nothing, and a single list would quietly teach the user that the two are the same kind of
 * risk. The drop inbox sits beneath them because it is the consequence of the second one.
 */
@Composable
fun SharingHubScreen(
    state: SharingUiState,
    actions: SharingHubActions,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier.fillMaxSize(),
        topBar = {
            CapsuleTopAppBar(
                title = stringResource(R.string.nav_sharing),
                subtitle = stringResource(R.string.android_sharing_hub_subtitle),
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

                state.failure != null && !state.isDegraded && state.uploadLinkCount == 0 ->
                    HubFailure(state, actions.retry)

                state.isEmpty -> HubEmpty(actions)
                else -> HubContent(state, actions)
            }
        }
    }
}

@Composable
private fun HubFailure(
    state: SharingUiState,
    onRetry: () -> Unit,
) {
    val failure = state.failure ?: return
    CapsuleErrorState(
        title = failureTitle(failure),
        body = failureMessage(failure),
        action = {
            CapsuleButton(onClick = onRetry, label = stringResource(R.string.android_sharing_retry))
        },
    )
}

@Composable
private fun HubEmpty(actions: SharingHubActions) {
    CapsuleEmptyState(
        title = stringResource(R.string.android_sharing_empty_title),
        body = stringResource(R.string.android_sharing_empty_body),
        icon = Icons.Filled.Link,
        action = {
            CapsuleButton(
                onClick = actions.createShareLink,
                label = stringResource(R.string.android_destination_share_link_create),
            )
        },
    )
}

@Composable
private fun HubContent(
    state: SharingUiState,
    actions: SharingHubActions,
) {
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(vertical = ContentSpacing),
    ) {
        if (state.isDegraded) {
            item { DegradedBanner(state, actions.retry) }
        }
        item {
            CapsuleListItem(
                headline = stringResource(R.string.android_destination_share_links),
                supporting =
                    stringResource(R.string.android_sharing_share_links_count)
                        .withArguments("count" to state.shareLinks.size),
                leading = { Icon(imageVector = Icons.Filled.Link, contentDescription = null) },
                onClick = actions.openShareLinks,
                modifier = Modifier.testTag(SHARING_HUB_SHARE_LINKS_TAG),
            )
        }
        item { CapsuleDivider(inset = true) }
        item {
            CapsuleListItem(
                headline = stringResource(R.string.android_destination_upload_links),
                supporting =
                    stringResource(R.string.android_sharing_upload_links_count)
                        .withArguments("count" to state.uploadLinkCount),
                leading = { Icon(imageVector = Icons.Filled.Upload, contentDescription = null) },
                onClick = actions.openUploadLinks,
                modifier = Modifier.testTag(SHARING_HUB_UPLOAD_LINKS_TAG),
            )
        }
        item { CapsuleDivider(inset = true) }
        item { DropInboxRow(state.pendingDropCount, actions.openDropInbox) }
        item { HubCreateActions(actions) }
    }
}

/** The inbox row, which is the one row that can be shouting. */
@Composable
private fun DropInboxRow(
    pending: Int,
    onOpen: () -> Unit,
) {
    CapsuleListItem(
        headline = stringResource(R.string.android_destination_drop_inbox),
        supporting = dropInboxSummary(pending),
        leading = { Icon(imageVector = Icons.Filled.Inbox, contentDescription = null) },
        trailing = {
            if (pending > 0) {
                CapsuleBadge(label = pending.toString(), tone = CapsuleBadgeTone.Quarantine)
            }
        },
        onClick = onOpen,
        modifier = Modifier.testTag(SHARING_HUB_DROP_INBOX_TAG),
    )
}

/** The two ways to mint something, kept in the order of increasing consequence. */
@Composable
private fun HubCreateActions(actions: SharingHubActions) {
    Column(
        modifier = Modifier.fillMaxWidth().padding(ContentPadding),
        verticalArrangement = Arrangement.spacedBy(ContentSpacing),
    ) {
        CapsuleButton(
            onClick = actions.createShareLink,
            label = stringResource(R.string.android_destination_share_link_create),
            modifier = Modifier.fillMaxWidth(),
        )
        CapsuleButton(
            onClick = actions.createUploadLink,
            label = stringResource(R.string.android_destination_upload_link_create),
            emphasis = CapsuleButtonEmphasis.Secondary,
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

/** A failed refresh that still left readable content behind it. */
@Composable
private fun DegradedBanner(
    state: SharingUiState,
    onRetry: () -> Unit,
) {
    val failure = state.failure ?: return
    CapsuleBanner(
        title = failureTitle(failure),
        body = failureMessage(failure),
        severity = CapsuleBannerSeverity.Degraded,
        modifier = Modifier.fillMaxWidth().padding(horizontal = ContentPadding),
        action = {
            CapsuleButton(
                onClick = onRetry,
                label = stringResource(R.string.android_sharing_retry),
                emphasis = CapsuleButtonEmphasis.Tertiary,
            )
        },
    )
}

/** What the inbox row says, which is a different sentence when nothing is waiting. */
@Composable
private fun dropInboxSummary(pending: Int): String =
    if (pending == 0) {
        stringResource(R.string.android_sharing_drops_none)
    } else {
        stringResource(R.string.android_sharing_drops_waiting).withArguments("count" to pending)
    }
