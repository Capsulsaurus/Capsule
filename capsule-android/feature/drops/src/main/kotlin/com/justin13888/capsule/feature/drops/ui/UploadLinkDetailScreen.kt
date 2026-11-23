package com.justin13888.capsule.feature.drops.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.LinkOff
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
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
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleProgress
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.feature.drops.CapUsage
import com.justin13888.capsule.feature.drops.UploadLinkDetailUiState
import com.justin13888.capsule.feature.drops.failureMessage
import com.justin13888.capsule.feature.drops.failureTitle
import com.justin13888.capsule.feature.drops.formattedBytes
import com.justin13888.capsule.feature.drops.formattedDate
import com.justin13888.capsule.feature.drops.withArguments

/** Test tag on the revoke button. */
const val UPLOAD_DETAIL_REVOKE_TAG: String = "upload-detail-revoke"

/** Test tag on the cumulative-bytes usage bar. */
const val UPLOAD_DETAIL_BYTES_TAG: String = "upload-detail-bytes"

/** Test tag on the file-count usage bar. */
const val UPLOAD_DETAIL_FILES_TAG: String = "upload-detail-files"

private val DetailPadding = 16.dp
private val DetailSpacing = 12.dp

/**
 * One upload link, read as what it has spent.
 *
 * There is no "copy link" here, and its absence is deliberate. An upload link's URL carries
 * the Drop Key's public half in its fragment; the server never receives it and the link
 * record does not carry it, so a copy button on this screen could only produce a URL a
 * guest's browser has nothing to encrypt to. What this screen can honestly show is every cap
 * and how much of it is gone.
 */
@Composable
fun UploadLinkDetailScreen(
    state: UploadLinkDetailUiState,
    onRevoke: () -> Unit,
    onRetry: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier.fillMaxSize(),
        topBar = {
            CapsuleTopAppBar(
                title = stringResource(R.string.android_destination_upload_link_detail),
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

                state.failure != null && state.link == null ->
                    CapsuleErrorState(
                        title = failureTitle(state.failure),
                        body = failureMessage(state.failure),
                        action = {
                            CapsuleButton(onClick = onRetry, label = stringResource(R.string.android_drops_retry))
                        },
                    )

                state.isRevoked ->
                    CapsuleEmptyState(
                        title = stringResource(R.string.android_drops_revoked_title),
                        body = stringResource(R.string.android_drops_revoked_body),
                        icon = Icons.Filled.LinkOff,
                        action = { CapsuleButton(onClick = onBack, label = stringResource(R.string.common_back)) },
                    )

                state.isMissing ->
                    CapsuleEmptyState(
                        title = stringResource(R.string.android_drops_detail_missing_title),
                        body = stringResource(R.string.android_drops_detail_missing_body),
                        icon = Icons.Filled.LinkOff,
                        action = { CapsuleButton(onClick = onBack, label = stringResource(R.string.common_back)) },
                    )

                else -> DetailContent(state, onRevoke)
            }
        }
    }
}

@Composable
private fun DetailContent(
    state: UploadLinkDetailUiState,
    onRevoke: () -> Unit,
) {
    val link = state.link ?: return
    var confirming by remember { mutableStateOf(false) }
    Column(
        modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(bottom = DetailPadding),
        verticalArrangement = Arrangement.spacedBy(DetailSpacing),
    ) {
        StatusBanner(state)
        CapsuleSectionHeader(title = stringResource(R.string.android_drops_usage_title))
        UsageBar(
            label = stringResource(R.string.android_drops_usage_bytes_label),
            usage = state.bytes,
            tag = UPLOAD_DETAIL_BYTES_TAG,
            format = { value -> formattedBytes(value) },
        )
        UsageBar(
            label = stringResource(R.string.android_drops_usage_files_label),
            usage = state.files,
            tag = UPLOAD_DETAIL_FILES_TAG,
            format = { value -> value.toString() },
        )
        Column(
            modifier = Modifier.fillMaxWidth().padding(horizontal = DetailPadding),
            verticalArrangement = Arrangement.spacedBy(DetailSpacing / 3),
        ) {
            Fact(
                link.caps.expiresAt
                    ?.let { stringResource(R.string.share_expires).withArguments("date" to formattedDate(it)) }
                    ?: stringResource(R.string.android_drops_cap_no_expiry),
            )
            Fact(
                link.caps.maxFileSize
                    ?.let {
                        stringResource(R.string.android_drops_cap_file_size)
                            .withArguments("limit" to formattedBytes(it))
                    } ?: stringResource(R.string.android_drops_cap_file_size_unbounded),
            )
            Fact(
                if (link.caps.singleUse) {
                    stringResource(R.string.android_drops_cap_single_use)
                } else {
                    stringResource(R.string.android_drops_cap_multi_use)
                },
            )
            if (link.requiresPassphrase) Fact(stringResource(R.string.android_drops_passphrase_required))
        }
        if (state.failure != null) {
            CapsuleBanner(
                title = failureTitle(state.failure),
                body = failureMessage(state.failure),
                severity = CapsuleBannerSeverity.Error,
                modifier = Modifier.fillMaxWidth().padding(horizontal = DetailPadding),
            )
        }
        CapsuleButton(
            onClick = { confirming = true },
            label = stringResource(R.string.android_drops_revoke),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            enabled = state.canRevoke,
            icon = Icons.Filled.LinkOff,
            modifier = Modifier.fillMaxWidth().padding(horizontal = DetailPadding).testTag(UPLOAD_DETAIL_REVOKE_TAG),
        )
    }
    if (confirming) {
        RevokeConfirmation(
            onConfirm = {
                confirming = false
                onRevoke()
            },
            onDismiss = { confirming = false },
        )
    }
}

/** Whether the door is open, and if not, why. */
@Composable
private fun StatusBanner(state: UploadLinkDetailUiState) {
    val closedBody =
        when {
            state.hasExpired -> stringResource(R.string.android_drops_closed_expired)
            state.bytes?.isExhausted == true || state.files?.isExhausted == true ->
                stringResource(R.string.android_drops_closed_full)

            else -> stringResource(R.string.android_drops_closed_generic)
        }
    CapsuleBanner(
        title =
            if (state.acceptsDeposits) {
                stringResource(R.string.android_drops_link_open)
            } else {
                stringResource(R.string.android_drops_link_closed)
            },
        body = if (state.acceptsDeposits) stringResource(R.string.android_drops_open_body) else closedBody,
        severity =
            if (state.acceptsDeposits) CapsuleBannerSeverity.Info else CapsuleBannerSeverity.Degraded,
        modifier = Modifier.fillMaxWidth().padding(horizontal = DetailPadding),
    )
}

/**
 * One cap, drawn as a bar when it has a ceiling and as a count when it does not.
 *
 * A bar for an unbounded cap would be a bar that never fills, which reads as "plenty of room"
 * rather than "no limit was ever set" — the opposite of what the owner paying for the bytes
 * needs to know.
 */
@Composable
private fun UsageBar(
    label: String,
    usage: CapUsage?,
    tag: String,
    format: @Composable (Long) -> String,
) {
    if (usage == null) return
    val limit = usage.limit
    val summary =
        if (limit == null) {
            stringResource(R.string.android_drops_usage_unbounded).withArguments("used" to format(usage.used))
        } else {
            stringResource(R.string.android_drops_usage_against_limit)
                .withArguments("used" to format(usage.used), "limit" to format(limit))
        }
    Column(
        modifier = Modifier.fillMaxWidth().padding(horizontal = DetailPadding),
        verticalArrangement = Arrangement.spacedBy(DetailSpacing / 3),
    ) {
        Text(text = label, style = MaterialTheme.typography.titleSmall)
        Text(
            text = summary,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (limit != null) {
            CapsuleProgress(
                progress = { usage.fraction },
                contentDescription = summary,
                modifier = Modifier.fillMaxWidth().testTag(tag),
            )
        }
    }
}

@Composable
private fun Fact(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun RevokeConfirmation(
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(text = stringResource(R.string.android_drops_revoke_confirm_title)) },
        text = { Text(text = stringResource(R.string.android_drops_revoke_confirm_body)) },
        confirmButton = {
            TextButton(onClick = onConfirm) {
                Text(text = stringResource(R.string.android_drops_revoke_confirm_action))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(text = stringResource(R.string.common_cancel)) }
        },
    )
}
