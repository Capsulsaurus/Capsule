package com.justin13888.capsule.feature.viewer.provenance

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.History
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
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.LifecycleAction
import com.justin13888.capsule.core.model.ProvenanceRecord
import com.justin13888.capsule.core.model.Wire
import com.justin13888.capsule.feature.viewer.ViewerFormat
import com.justin13888.capsule.feature.viewer.localizedMessage
import com.justin13888.capsule.feature.viewer.withArguments

/** Test handle for the provenance list. */
const val PROVENANCE_TAG: String = "viewer.provenance"

/**
 * The signed chain behind one asset.
 *
 * Rendered as an ordinary list rather than as a timeline graphic on purpose: what matters is
 * that every link names the one before it, and a decorative connector would suggest the
 * ordering is the app's rather than the chain's. Each row shows the hash it follows, because
 * that link is the thing that makes a replayed stale manifest detectable at all.
 */
@Composable
fun ProvenanceScreen(
    state: ProvenanceUiState,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    when (state) {
        ProvenanceUiState.Loading ->
            CapsuleLoadingState(
                modifier = modifier.fillMaxSize().testTag(PROVENANCE_TAG),
                contentDescription = stringResource(R.string.common_loading),
            )

        ProvenanceUiState.Empty ->
            CapsuleEmptyState(
                title = stringResource(R.string.viewer_provenance_empty),
                modifier = modifier.fillMaxSize().testTag(PROVENANCE_TAG),
                icon = Icons.Filled.History,
            )

        is ProvenanceUiState.Failed ->
            CapsuleErrorState(
                title = stringResource(R.string.viewer_error_title),
                modifier = modifier.fillMaxSize().testTag(PROVENANCE_TAG),
                body = state.failure.localizedMessage(),
                action = {
                    CapsuleButton(onClick = onRetry, label = stringResource(R.string.viewer_action_retry))
                },
            )

        is ProvenanceUiState.Ready -> ChainList(state, modifier)
    }
}

/** The chain itself, oldest first, with the append-only note under it. */
@Composable
private fun ChainList(
    state: ProvenanceUiState.Ready,
    modifier: Modifier,
) {
    LazyColumn(modifier = modifier.fillMaxSize().testTag(PROVENANCE_TAG)) {
        items(state.entries, key = { it.record.recordHash }) { entry -> ChainRow(entry) }
        item {
            CapsuleBanner(
                title = stringResource(R.string.viewer_provenance_note),
                modifier = Modifier.fillMaxWidth().padding(ListPadding),
                severity = CapsuleBannerSeverity.Info,
            )
        }
    }
}

/** One link: what happened, who signed it, and which head it followed. */
@Composable
private fun ChainRow(entry: ProvenanceEntry) {
    val record = entry.record
    val attribution =
        stringResource(R.string.viewer_provenance_attribution).withArguments(
            "device" to (entry.deviceLabel ?: ViewerFormat.shortHash(record.byDevice.value)),
            "time" to ViewerFormat.timestamp(record.at),
        )
    CapsuleListItem(
        headline = actionLabel(record.action),
        modifier = Modifier.fillMaxWidth(),
        supporting = listOfNotNull(chainLink(record), retentionNote(record)).joinToString(separator = "\n"),
        overline = attribution,
    )
}

/**
 * The head this write followed.
 *
 * Absent exactly on a `create`, which is the one action with no prior link — and saying so
 * explicitly beats leaving the row blank, because a blank line reads as missing data rather
 * than as the beginning of the chain.
 */
@Composable
private fun chainLink(record: ProvenanceRecord): String =
    record.priorProvenanceHash?.let {
        stringResource(R.string.viewer_provenance_follows).withArguments("hash" to ViewerFormat.shortHash(it))
    } ?: stringResource(R.string.viewer_provenance_root)

/**
 * The signed floor on a purge, present on a delete.
 *
 * Shown because it is the guarantee: the deadline travels inside the delete manifest, so
 * nothing downstream — including a hostile server editing its own configuration — can bring
 * it forward.
 */
@Composable
private fun retentionNote(record: ProvenanceRecord): String? =
    record.retentionUntil?.let {
        stringResource(R.string.viewer_provenance_retention).withArguments("date" to ViewerFormat.date(it))
    }

/**
 * What a lifecycle action is called.
 *
 * An action outside the closed set is rendered with its raw token rather than hidden. The set
 * is closed and a value outside it is a structural error — but it is also *someone's history*,
 * and dropping the row would leave a gap in an append-only chain.
 */
@Composable
private fun actionLabel(action: Wire<LifecycleAction>): String =
    when (action) {
        is Wire.Unknown ->
            stringResource(R.string.viewer_provenance_action_unknown).withArguments("token" to action.raw)

        is Wire.Known -> stringResource(action.value.labelRes())
    }

@StringRes
private fun LifecycleAction.labelRes(): Int =
    when (this) {
        LifecycleAction.Create -> R.string.viewer_provenance_action_create
        LifecycleAction.Replace -> R.string.viewer_provenance_action_replace
        LifecycleAction.Delete -> R.string.viewer_provenance_action_delete
        LifecycleAction.MetadataUpdate -> R.string.viewer_provenance_action_metadata_update
        LifecycleAction.DerivativeAdd -> R.string.viewer_provenance_action_derivative_add
        LifecycleAction.DerivativeReplace -> R.string.viewer_provenance_action_derivative_replace
        LifecycleAction.TrashRestore -> R.string.viewer_provenance_action_trash_restore
    }

private val ListPadding = 16.dp
