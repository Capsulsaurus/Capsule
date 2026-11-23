package com.justin13888.capsule.feature.sharing.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ContentCopy
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.feature.sharing.ShareLinkDetailUiState
import com.justin13888.capsule.feature.sharing.ShareLinkRow
import com.justin13888.capsule.feature.sharing.failureMessage
import com.justin13888.capsule.feature.sharing.failureTitle
import com.justin13888.capsule.feature.sharing.formattedDate
import com.justin13888.capsule.feature.sharing.qr.QrCode
import com.justin13888.capsule.feature.sharing.withArguments

/** Test tag on the copy-link button. */
const val SHARE_DETAIL_COPY_TAG: String = "share-detail-copy"

/** Test tag on the revoke button. */
const val SHARE_DETAIL_REVOKE_TAG: String = "share-detail-revoke"

/** Test tag on the rendered URL, fragment included. */
const val SHARE_DETAIL_URL_TAG: String = "share-detail-url"

private val DetailPadding = 16.dp
private val DetailSpacing = 12.dp

/**
 * One share link: what it points at, the URL that opens it, and the way to end it.
 *
 * @param justCopied whether the copy just happened, so the screen can confirm it. Held by the
 *   caller rather than by this screen, because the copy itself is a platform effect the
 *   screen must not own if it is to stay previewable.
 */
@Composable
fun ShareLinkDetailScreen(
    state: ShareLinkDetailUiState,
    justCopied: Boolean,
    onCopy: () -> Unit,
    onRevoke: () -> Unit,
    onRetry: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier.fillMaxSize(),
        topBar = {
            CapsuleTopAppBar(
                title = stringResource(R.string.android_destination_share_link_detail),
                subtitle = state.row?.let { shareScopeLabel(it) },
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

                state.failure != null && state.row == null ->
                    CapsuleErrorState(
                        title = failureTitle(state.failure),
                        body = failureMessage(state.failure),
                        action = {
                            CapsuleButton(onClick = onRetry, label = stringResource(R.string.android_sharing_retry))
                        },
                    )

                state.isRevoked ->
                    CapsuleEmptyState(
                        title = stringResource(R.string.android_sharing_revoked_title),
                        body = stringResource(R.string.android_sharing_revoked_body),
                        icon = Icons.Filled.LinkOff,
                        action = { CapsuleButton(onClick = onBack, label = stringResource(R.string.common_back)) },
                    )

                state.isMissing ->
                    CapsuleEmptyState(
                        title = stringResource(R.string.android_sharing_detail_missing_title),
                        body = stringResource(R.string.android_sharing_detail_missing_body),
                        icon = Icons.Filled.LinkOff,
                        action = { CapsuleButton(onClick = onBack, label = stringResource(R.string.common_back)) },
                    )

                else -> DetailContent(state, justCopied, onCopy, onRevoke)
            }
        }
    }
}

@Composable
private fun DetailContent(
    state: ShareLinkDetailUiState,
    justCopied: Boolean,
    onCopy: () -> Unit,
    onRevoke: () -> Unit,
) {
    val row = state.row ?: return
    var confirming by remember { mutableStateOf(false) }
    Column(
        modifier =
            Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(bottom = DetailPadding),
        verticalArrangement = Arrangement.spacedBy(DetailSpacing),
    ) {
        LinkFacts(row)
        if (state.url == null) SecretUnavailable() else LinkUrl(state.url, justCopied, onCopy)
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
            label = stringResource(R.string.android_sharing_revoke),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            enabled = state.canRevoke,
            icon = Icons.Filled.LinkOff,
            modifier = Modifier.fillMaxWidth().padding(horizontal = DetailPadding).testTag(SHARE_DETAIL_REVOKE_TAG),
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

@Composable
private fun LinkFacts(row: ShareLinkRow) {
    Column(modifier = Modifier.fillMaxWidth().padding(horizontal = DetailPadding)) {
        Text(text = shareScopeLabel(row), style = MaterialTheme.typography.titleMedium)
        Text(
            text =
                stringResource(R.string.android_sharing_created)
                    .withArguments("date" to formattedDate(row.link.createdAt)),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            text =
                row.link.expiresAt
                    ?.let { stringResource(R.string.share_expires).withArguments("date" to formattedDate(it)) }
                    ?: stringResource(R.string.android_sharing_no_expiry),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (row.link.requiresPassphrase) {
            Text(
                text = stringResource(R.string.android_sharing_passphrase_required),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/**
 * The URL, the warning about its fragment, the copy action, and the QR of the same string.
 *
 * All four render the *complete* URL. The part after `#` is the key that decrypts the share
 * and the server never sees it, so a copy that stops at the `#` produces a link the recipient
 * cannot open — and nothing about it looks broken until they try.
 */
@Composable
private fun LinkUrl(
    url: String,
    justCopied: Boolean,
    onCopy: () -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxWidth().padding(horizontal = DetailPadding),
        verticalArrangement = Arrangement.spacedBy(DetailSpacing),
    ) {
        CapsuleCard(modifier = Modifier.fillMaxWidth()) {
            SelectionContainer {
                Text(
                    text = url,
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.padding(DetailPadding).testTag(SHARE_DETAIL_URL_TAG),
                )
            }
        }
        Text(
            text = stringResource(R.string.android_sharing_copy_warning),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        CapsuleButton(
            onClick = onCopy,
            label =
                if (justCopied) {
                    stringResource(R.string.android_sharing_copied)
                } else {
                    stringResource(R.string.android_sharing_copy_link)
                },
            icon = Icons.Filled.ContentCopy,
            modifier = Modifier.fillMaxWidth().testTag(SHARE_DETAIL_COPY_TAG),
        )
        CapsuleSectionHeader(title = stringResource(R.string.android_sharing_qr_title))
        val code = remember(url) { QrCode.encode(url) }
        if (code != null) {
            Column(
                modifier = Modifier.fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                QrCodeImage(
                    code = code,
                    contentDescription = stringResource(R.string.android_sharing_qr_description),
                )
            }
        }
    }
}

/** The link exists, but its one-and-only secret was never on this device. */
@Composable
private fun SecretUnavailable() {
    CapsuleBanner(
        title = stringResource(R.string.android_sharing_secret_unavailable_title),
        body = stringResource(R.string.android_sharing_secret_unavailable_body),
        severity = CapsuleBannerSeverity.Degraded,
        modifier = Modifier.fillMaxWidth().padding(horizontal = DetailPadding),
    )
}

@Composable
private fun RevokeConfirmation(
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(text = stringResource(R.string.android_sharing_revoke_confirm_title)) },
        text = { Text(text = stringResource(R.string.android_sharing_revoke_confirm_body)) },
        confirmButton = {
            TextButton(onClick = onConfirm) {
                Text(text = stringResource(R.string.android_sharing_revoke_confirm_action))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text(text = stringResource(R.string.common_cancel))
            }
        },
    )
}
