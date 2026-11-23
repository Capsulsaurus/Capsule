package com.justin13888.capsule.feature.audit

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleChipState
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleFilterChipRow
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceId
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.toImmutableList

/** Test handle for the whole listing pane. */
const val AUDIT_LIST_TAG: String = "audit.list"

/** Test handle for the degraded banner that admits the history is partial. */
const val AUDIT_TRUNCATED_TAG: String = "audit.truncated"

/** Test handle for the error state. */
const val AUDIT_ERROR_TAG: String = "audit.error"

/** Test handle for the loading state. */
const val AUDIT_LOADING_TAG: String = "audit.loading"

/** Test handle for the empty state, whether the library is new or the filter is too narrow. */
const val AUDIT_EMPTY_TAG: String = "audit.empty"

/** Test handle for one row: `audit.entry:<id>`. */
fun auditEntryTag(entryId: String): String = "audit.entry:$entryId"

private val ScreenInset = 16.dp

/**
 * The library audit log: every lifecycle and moderation action, newest first.
 *
 * Stateless, so every state it can be in is reachable from a preview and a test without a
 * view model. The two halves of the log share one list rather than two tabs on purpose — the
 * moderation design's "no silent operations" rule is only worth something if an operator's
 * action turns up in the place the user already looks.
 *
 * @param state everything to draw, already filtered.
 * @param onSelectEntry opens the record behind a row.
 * @param onToggleCategory includes or excludes one origin.
 * @param onToggleThisDevice narrows to records signed by this device, or widens back.
 * @param onClearFilters puts every entry back in view.
 * @param onRetry re-reads the log after a failure.
 */
@Composable
fun AuditLogScreen(
    state: AuditUiState,
    onSelectEntry: (String) -> Unit,
    onToggleCategory: (AuditCategory) -> Unit,
    onToggleThisDevice: () -> Unit,
    onClearFilters: () -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(AUDIT_LIST_TAG)) {
        CapsuleTopAppBar(
            title = stringResource(R.string.android_destination_audit_log),
            subtitle = stringResource(R.string.android_audit_summary),
        )
        CapsuleFilterChipRow(
            chips = auditFilterChips(state, onToggleCategory, onToggleThisDevice),
            modifier = Modifier.fillMaxWidth().padding(horizontal = ScreenInset),
        )
        when {
            state.isLoading ->
                CapsuleLoadingState(
                    modifier = Modifier.fillMaxWidth().testTag(AUDIT_LOADING_TAG),
                    title = stringResource(R.string.android_audit_loading),
                    contentDescription = stringResource(R.string.android_audit_loading),
                )

            state.failure != null -> AuditFailureState(state.failure, onRetry)
            state.isEmpty -> AuditEmptyState()
            state.hasNoMatches -> AuditNoMatchesState(onClearFilters)
            else -> AuditEntryList(state, onSelectEntry)
        }
    }
}

/**
 * The filter row.
 *
 * The device chip appears only when the signed directory actually names this device: a chip
 * that could never match anything is worse than an absent one.
 */
@Composable
private fun auditFilterChips(
    state: AuditUiState,
    onToggleCategory: (AuditCategory) -> Unit,
    onToggleThisDevice: () -> Unit,
): ImmutableList<CapsuleChipState> {
    val chips =
        AuditCategory.entries.map { category ->
            CapsuleChipState(
                label = auditCategoryLabel(category),
                selected = category in state.filter.categories,
                onSelectedChange = { onToggleCategory(category) },
            )
        }
    val deviceChip =
        state.thisDevice?.let {
            CapsuleChipState(
                label = stringResource(R.string.android_audit_filter_this_device),
                selected = state.filter.onlyDevice != null,
                onSelectedChange = { onToggleThisDevice() },
            )
        }
    return (chips + listOfNotNull(deviceChip)).toImmutableList()
}

@Composable
private fun AuditFailureState(
    failure: AuditFailure,
    onRetry: () -> Unit,
) {
    CapsuleErrorState(
        title = stringResource(R.string.android_audit_error_title),
        modifier = Modifier.fillMaxWidth().testTag(AUDIT_ERROR_TAG),
        // The localized `error.*` code, never a status: the code is the contract and a status
        // is a transport detail the user can do nothing with.
        body = localizedErrorMessage(failure.code),
        action = {
            CapsuleButton(
                onClick = onRetry,
                label = stringResource(R.string.android_audit_retry),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

@Composable
private fun AuditEmptyState() {
    CapsuleEmptyState(
        title = stringResource(R.string.android_audit_empty_title),
        modifier = Modifier.fillMaxWidth().testTag(AUDIT_EMPTY_TAG),
        body = stringResource(R.string.android_audit_empty_body),
    )
}

@Composable
private fun AuditNoMatchesState(onClearFilters: () -> Unit) {
    CapsuleEmptyState(
        title = stringResource(R.string.android_audit_no_matches_title),
        modifier = Modifier.fillMaxWidth().testTag(AUDIT_EMPTY_TAG),
        body = stringResource(R.string.android_audit_no_matches_body),
        action = {
            CapsuleButton(
                onClick = onClearFilters,
                label = stringResource(R.string.android_audit_clear_filters),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

@Composable
private fun AuditEntryList(
    state: AuditUiState,
    onSelectEntry: (String) -> Unit,
) {
    LazyColumn(modifier = Modifier.fillMaxSize()) {
        if (state.log.truncated) {
            item(key = TRUNCATED_KEY) { TruncatedBanner(state.log.scannedAssets) }
        }
        items(items = state.visible, key = AuditEntry::id) { entry ->
            AuditRow(
                entry = entry,
                devices = state.devices,
                selected = entry.id == state.selectedId,
                onClick = { onSelectEntry(entry.id) },
            )
        }
        item(key = NOTE_KEY) {
            Text(
                text = stringResource(R.string.android_audit_moderation_note),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(ScreenInset),
            )
        }
    }
}

@Composable
private fun TruncatedBanner(scannedAssets: Int) {
    CapsuleBanner(
        title = stringResource(R.string.android_audit_truncated_title),
        modifier = Modifier.fillMaxWidth().padding(ScreenInset).testTag(AUDIT_TRUNCATED_TAG),
        body = stringResource(R.string.android_audit_truncated_body).withArguments("count" to scannedAssets),
        severity = CapsuleBannerSeverity.Degraded,
    )
}

/**
 * One row of the log.
 *
 * The announced description is assembled from the three already-localized fragments rather
 * than left to the row's own concatenation, so a screen reader says "Moved to trash, this
 * device, 14 March" instead of running the timestamp into the action.
 */
@Composable
private fun AuditRow(
    entry: AuditEntry,
    devices: Map<DeviceId, Device>,
    selected: Boolean,
    onClick: () -> Unit,
) {
    val action = auditEntryLabel(entry)
    val signer = auditSignerLabel(entry, devices)
    val at = formattedInstant(entry.at)
    CapsuleListItem(
        headline = action,
        modifier =
            Modifier
                .testTag(auditEntryTag(entry.id))
                .semantics { this.selected = selected },
        supporting = signer,
        overline = at,
        onClick = onClick,
        contentDescription = listOf(action, signer, at).joinToString(SEPARATOR),
    )
}

private const val TRUNCATED_KEY = "audit.truncated-banner"
private const val NOTE_KEY = "audit.moderation-note"
