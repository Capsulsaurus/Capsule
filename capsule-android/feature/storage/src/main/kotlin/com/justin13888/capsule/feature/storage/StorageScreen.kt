package com.justin13888.capsule.feature.storage

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
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
import com.justin13888.capsule.core.designsystem.component.CapsuleFilterChipRow
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.SyncScope
import kotlinx.collections.immutable.toImmutableList

private val ScreenInset = 16.dp
private val BlockSpacing = 8.dp

/**
 * Every decision the local-storage screen hands back.
 *
 * One parameter object rather than seven lambdas, so the screen's signature stays readable
 * and a new decision is a field rather than another positional argument at every call site.
 */
@Immutable
class StorageActions(
    /** Chooses the ceiling the automatic sweep keeps the cache under. */
    val onBudgetChange: (CacheBudget) -> Unit,
    /** Runs an eviction down to that ceiling now. */
    val onReclaim: () -> Unit,
    /** Pins or unpins one asset for offline use. */
    val onPinnedChange: (AssetId, Boolean) -> Unit,
    /** Asks to release one device-owned original, behind the durability gate. */
    val onRelease: (AssetId) -> Unit,
    /** Changes how much of each new asset this device fetches. */
    val onSyncScopeChange: (SyncScope) -> Unit,
    /** Opens the account allowance. */
    val onOpenQuota: () -> Unit,
    /** Re-reads after a failure. */
    val onRetry: () -> Unit,
)

/**
 * What this device is holding, and what it is allowed to let go of.
 *
 * Stateless. The three sections are one scrolling column rather than tabs because they are
 * one decision made in three parts: how much room to give the cache, what is using it, and
 * how much to fetch next time.
 */
@Composable
fun StorageScreen(
    state: StorageUiState,
    actions: StorageActions,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(STORAGE_SCREEN_TAG)) {
        CapsuleTopAppBar(
            title = stringResource(R.string.storage_title),
            subtitle = stringResource(R.string.android_storage_subtitle),
        )
        when {
            state.isLoading ->
                CapsuleLoadingState(
                    modifier = Modifier.fillMaxWidth().testTag(STORAGE_LOADING_TAG),
                    title = stringResource(R.string.android_storage_loading),
                    contentDescription = stringResource(R.string.android_storage_loading),
                )

            state.failure != null -> StorageFailureState(state.failure, actions.onRetry)
            else -> StorageBody(state, actions)
        }
    }
}

@Composable
private fun StorageBody(
    state: StorageUiState,
    actions: StorageActions,
) {
    LazyColumn(modifier = Modifier.fillMaxSize()) {
        item(key = QUOTA_LINK_KEY) { QuotaLink(state, actions.onOpenQuota) }
        item(key = BUDGET_KEY) { CacheBudgetSection(state, actions) }
        item(key = SCOPE_HEADER_KEY) {
            CapsuleSectionHeader(title = stringResource(R.string.android_storage_scope_section))
        }
        items(items = SyncScope.entries, key = { scope -> SCOPE_KEY_PREFIX + scope.wire }) { scope ->
            SyncScopeRow(scope = scope, selected = scope == state.syncScope, onSelect = actions.onSyncScopeChange)
        }
        item(key = SCOPE_NOTE_KEY) { FootNote(stringResource(R.string.android_storage_scope_note)) }
        item(key = CONSUMERS_HEADER_KEY) {
            CapsuleSectionHeader(
                title = stringResource(R.string.android_storage_consumers_section),
                modifier = Modifier.testTag(STORAGE_CONSUMERS_TAG),
            )
        }
        if (state.hasNoConsumers) {
            item(key = CONSUMERS_EMPTY_KEY) {
                CapsuleEmptyState(
                    title = stringResource(R.string.android_storage_consumers_empty),
                    modifier = Modifier.fillMaxWidth().testTag(STORAGE_EMPTY_TAG),
                )
            }
        }
        items(items = state.consumers, key = { consumer -> consumer.id.value }) { consumer ->
            ConsumerRow(consumer = consumer, actions = actions)
        }
    }
}

/** A one-line reading of the account allowance, and the way into the detail behind it. */
@Composable
private fun QuotaLink(
    state: StorageUiState,
    onOpenQuota: () -> Unit,
) {
    val standing = state.standing
    val supporting =
        when {
            standing == null -> null
            standing.hardLimit == null ->
                stringResource(R.string.android_storage_quota_usage_unlimited)
                    .withArguments("used" to formattedBytes(standing.used))

            else ->
                stringResource(R.string.android_storage_quota_usage).withArguments(
                    "used" to formattedBytes(standing.used),
                    "total" to formattedBytes(standing.hardLimit),
                )
        }
    CapsuleListItem(
        headline = stringResource(R.string.android_storage_open_quota),
        modifier = Modifier.fillMaxWidth(),
        supporting = supporting,
        onClick = onOpenQuota,
    )
}

/** The cache ceiling, what is inside it now, and the button that enforces it. */
@Composable
private fun CacheBudgetSection(
    state: StorageUiState,
    actions: StorageActions,
) {
    Column(
        modifier = Modifier.fillMaxWidth().testTag(STORAGE_BUDGET_TAG),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        CapsuleSectionHeader(title = stringResource(R.string.android_storage_local_section))
        Text(
            text =
                stringResource(R.string.android_storage_cache_used).withArguments(
                    "used" to formattedBytes(state.cacheBytes),
                    "budget" to formattedBytes(state.budget.bytes),
                ),
            style = MaterialTheme.typography.bodyLarge,
            modifier = Modifier.padding(horizontal = ScreenInset),
        )
        CapsuleFilterChipRow(
            chips =
                CacheBudget.entries
                    .map { budget ->
                        CapsuleChipState(
                            label = formattedBytes(budget.bytes),
                            selected = budget == state.budget,
                            onSelectedChange = { actions.onBudgetChange(budget) },
                        )
                    }.toImmutableList(),
            modifier = Modifier.fillMaxWidth().padding(horizontal = ScreenInset),
        )
        CapsuleButton(
            onClick = actions.onReclaim,
            label =
                if (state.isReclaiming) {
                    stringResource(R.string.android_storage_reclaiming)
                } else {
                    stringResource(R.string.android_storage_reclaim)
                },
            modifier = Modifier.padding(horizontal = ScreenInset),
            emphasis = CapsuleButtonEmphasis.Secondary,
            enabled = !state.isReclaiming,
        )
        ReclaimResult(state.reclaim)
        FootNote(stringResource(R.string.android_storage_cache_budget_note))
    }
}

/** What the last sweep did — including when the honest answer is "nothing". */
@Composable
private fun ReclaimResult(outcome: ReclaimOutcome?) {
    if (outcome !is ReclaimOutcome.Freed) return
    val text =
        if (outcome.bytes <= 0L) {
            stringResource(R.string.android_storage_reclaim_none)
        } else {
            stringResource(R.string.android_storage_reclaimed).withArguments("size" to formattedBytes(outcome.bytes))
        }
    Text(
        text = text,
        style = MaterialTheme.typography.bodyMedium,
        modifier = Modifier.padding(horizontal = ScreenInset),
    )
}

/** One choice of how much of each new asset this device fetches. */
@Composable
private fun SyncScopeRow(
    scope: SyncScope,
    selected: Boolean,
    onSelect: (SyncScope) -> Unit,
) {
    CapsuleListItem(
        headline = syncScopeLabel(scope),
        modifier =
            Modifier
                .fillMaxWidth()
                .testTag(STORAGE_SCOPE_TAG + scope.wire)
                .semantics { this.selected = selected },
        supporting = syncScopeBody(scope),
        onClick = { onSelect(scope) },
    )
}

/**
 * One consumer, with the two things that can be done to it.
 *
 * Pinning and releasing are opposites and sit on the same row deliberately: "keep this
 * offline" and "stop being the only copy of this" are the same decision seen from either end,
 * and separating them into two screens would hide the trade the user is making.
 */
@Composable
private fun ConsumerRow(
    consumer: StorageConsumer,
    actions: StorageActions,
) {
    val kind = assetKindLabel(consumer.assetType)
    val date = formattedDate(consumer.capturedAt)
    Column(modifier = Modifier.fillMaxWidth().testTag(storageConsumerTag(consumer.id.value))) {
        CapsuleListItem(
            headline =
                stringResource(R.string.android_storage_consumer_headline)
                    .withArguments("kind" to kind, "size" to formattedBytes(consumer.bytes)),
            modifier = Modifier.fillMaxWidth(),
            supporting = consumerSupporting(consumer),
            overline = date,
        )
        ConsumerActions(consumer, kind, date, actions)
        ReleaseNotice(consumer, actions.onRelease)
    }
}

/** What the row says about itself under the headline. */
@Composable
private fun consumerSupporting(consumer: StorageConsumer): String {
    val held = heldLabel(consumer.held)
    return if (consumer.isPinned) held + SEPARATOR + stringResource(R.string.android_storage_pinned_note) else held
}

@Composable
private fun ConsumerActions(
    consumer: StorageConsumer,
    kind: String,
    date: String,
    actions: StorageActions,
) {
    Column(
        modifier = Modifier.padding(start = ScreenInset, end = ScreenInset, bottom = BlockSpacing),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        CapsuleButton(
            onClick = { actions.onPinnedChange(consumer.id, !consumer.isPinned) },
            label =
                if (consumer.isPinned) {
                    stringResource(R.string.android_storage_unpin)
                } else {
                    stringResource(R.string.android_storage_pin)
                },
            emphasis = CapsuleButtonEmphasis.Tertiary,
        )
        // Only an original goes through the gate. Anything below it is rebuildable or
        // re-fetchable by definition, so the budget sweep may take it without asking.
        if (consumer.holdsOriginal && consumer.release != ReleaseState.Released) {
            CapsuleButton(
                onClick = { actions.onRelease(consumer.id) },
                label =
                    if (consumer.release == ReleaseState.Checking) {
                        stringResource(R.string.android_storage_release_checking)
                    } else {
                        stringResource(R.string.android_storage_release)
                    },
                emphasis = CapsuleButtonEmphasis.Tertiary,
                enabled = consumer.release != ReleaseState.Checking,
                contentDescription =
                    stringResource(R.string.android_storage_release_description)
                        .withArguments("kind" to kind, "date" to date),
            )
        }
    }
}

/**
 * The verdict, when there is one to report.
 *
 * An unconfirmed verdict is a persistent notice rather than a transient message: the client
 * still holds bytes that may exist nowhere else, and a condition that scrolls away after four
 * seconds is a condition the user will meet again without understanding why.
 */
@Composable
private fun ReleaseNotice(
    consumer: StorageConsumer,
    onRelease: (AssetId) -> Unit,
) {
    when (consumer.release) {
        ReleaseState.Unconfirmed ->
            CapsuleBanner(
                title = stringResource(R.string.android_storage_release_unconfirmed),
                modifier =
                    Modifier
                        .fillMaxWidth()
                        .padding(horizontal = ScreenInset, vertical = BlockSpacing)
                        .testTag(storageUnconfirmedTag(consumer.id.value)),
                body = stringResource(R.string.android_storage_release_unconfirmed_body),
                severity = CapsuleBannerSeverity.Degraded,
                action = {
                    CapsuleButton(
                        onClick = { onRelease(consumer.id) },
                        label = stringResource(R.string.android_storage_release_recheck),
                        emphasis = CapsuleButtonEmphasis.Secondary,
                    )
                },
            )

        ReleaseState.Released -> FootNote(stringResource(R.string.android_storage_released))
        ReleaseState.Idle, ReleaseState.Checking -> Unit
    }
}

/** A quiet line of explanation under a section. */
@Composable
private fun FootNote(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = ScreenInset, vertical = BlockSpacing / 2),
    )
}

/** Joins two already-localized fragments of one row. */
internal const val SEPARATOR: String = " · "

private const val QUOTA_LINK_KEY = "storage.quota-link"
private const val BUDGET_KEY = "storage.budget-section"
private const val SCOPE_HEADER_KEY = "storage.scope-header"
private const val SCOPE_NOTE_KEY = "storage.scope-note"
private const val SCOPE_KEY_PREFIX = "storage.scope:"
private const val CONSUMERS_HEADER_KEY = "storage.consumers-header"
private const val CONSUMERS_EMPTY_KEY = "storage.consumers-empty"
