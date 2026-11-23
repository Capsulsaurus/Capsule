package com.justin13888.capsule.feature.importer

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
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
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleToggleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.ImportCandidate
import com.justin13888.capsule.core.model.ImportPlan

/** Test handle for the plan's headline counts. */
const val IMPORT_PLAN_SUMMARY_TAG: String = "importer.plan-summary"

/** Test handle for the streaming recommendation. */
const val IMPORT_STREAMING_BANNER_TAG: String = "importer.streaming-banner"

/** Test handle for the hard single-file refusal. */
const val IMPORT_BLOCKED_TAG: String = "importer.blocked"

/** Test handle for the confirm action. */
const val IMPORT_CONFIRM_TAG: String = "importer.confirm"

/** Every callback the plan-and-confirm screen raises. */
data class ImportPlanActions(
    /** Chooses whether the source files survive the run. */
    val onSetMode: (ImportMode) -> Unit,
    /** Confirms the plan and starts the run. */
    val onConfirm: () -> Unit,
    /** Takes the device off staged uploads so a streaming run can proceed. */
    val onSwitchToFullUploads: () -> Unit,
    /** Re-derives the plan and re-probes free space. */
    val onRetry: () -> Unit,
    /** Leaves the screen without importing anything. */
    val onBack: () -> Unit,
)

/**
 * What the planner decided, shown before a byte moves.
 *
 * The counts, the size and the destination are the contract of this screen: they are the
 * numbers the user agrees to, and confirmation attaches nothing to the plan except the
 * free-space verdict the pure planner is forbidden to compute.
 */
@Composable
fun ImportPlanScreen(
    state: ImportPlanUiState,
    actions: ImportPlanActions,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize()) {
        CapsuleTopAppBar(
            title = stringResource(R.string.android_destination_import_plan),
            navigationIcon = {
                IconButton(onClick = actions.onBack) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                        contentDescription = stringResource(R.string.android_shell_back),
                    )
                }
            },
        )
        when {
            state.isLoading ->
                CapsuleLoadingState(
                    modifier = Modifier.fillMaxSize(),
                    title = stringResource(R.string.android_importer_plan_loading),
                    contentDescription = stringResource(R.string.android_importer_plan_loading),
                )

            state.hasFailed ->
                CapsuleErrorState(
                    title = stringResource(R.string.android_importer_plan_error_title),
                    modifier = Modifier.fillMaxSize(),
                    body = localizedErrorMessage(state.failure),
                    action = {
                        CapsuleButton(
                            onClick = actions.onRetry,
                            label = stringResource(R.string.android_importer_retry),
                        )
                    },
                )

            state.isEmpty ->
                CapsuleEmptyState(
                    title = stringResource(R.string.android_importer_plan_empty_title),
                    modifier = Modifier.fillMaxSize(),
                    body = stringResource(R.string.android_importer_plan_empty_body),
                    action = {
                        CapsuleButton(
                            onClick = actions.onBack,
                            label = stringResource(R.string.common_back),
                            emphasis = CapsuleButtonEmphasis.Secondary,
                        )
                    },
                )

            state.plan != null -> PlanBody(state = state, plan = state.plan, actions = actions)
        }
    }
}

@Composable
private fun PlanBody(
    state: ImportPlanUiState,
    plan: ImportPlan,
    actions: ImportPlanActions,
) {
    Column(modifier = Modifier.fillMaxSize()) {
        Column(
            modifier = Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(ScreenPadding),
            verticalArrangement = Arrangement.spacedBy(BlockSpacing),
        ) {
            SpaceNotice(state = state, actions = actions)
            PlanCounts(plan = plan)
            DestinationCard(state = state, plan = plan)
            ModeChoice(mode = state.mode, onSetMode = actions.onSetMode)
            SkippedList(plan = plan)
        }
        ConfirmFooter(state = state, plan = plan, onConfirm = actions.onConfirm)
    }
}

@Composable
private fun SpaceNotice(
    state: ImportPlanUiState,
    actions: ImportPlanActions,
) {
    when (val space = state.space) {
        ImportSpaceVerdict.Fits -> Unit

        is ImportSpaceVerdict.StreamingRecommended ->
            CapsuleBanner(
                title = stringResource(R.string.android_importer_streaming_title),
                modifier = Modifier.fillMaxWidth().testTag(IMPORT_STREAMING_BANNER_TAG),
                body =
                    stringResource(R.string.android_importer_streaming_body).withArguments(
                        "free" to formattedSize(space.freeBytes),
                        "needed" to formattedSize(space.neededBytes),
                    ),
                severity = CapsuleBannerSeverity.Warning,
            )

        is ImportSpaceVerdict.SingleFileTooLarge ->
            CapsuleBanner(
                title =
                    stringResource(R.string.android_importer_blocked_title).withArguments(
                        "name" to space.locator.substringAfterLast('/'),
                        "needed" to formattedSize(space.shortfallBytes),
                    ),
                modifier = Modifier.fillMaxWidth().testTag(IMPORT_BLOCKED_TAG),
                body =
                    stringResource(R.string.android_importer_blocked_body).withArguments(
                        "size" to formattedSize(space.sizeBytes),
                        "free" to formattedSize(space.freeBytes),
                    ),
                severity = CapsuleBannerSeverity.Error,
            )
    }
    if (state.hasStagedConflict) {
        CapsuleBanner(
            title = stringResource(R.string.android_importer_staged_conflict_title),
            modifier = Modifier.fillMaxWidth(),
            body = stringResource(R.string.android_importer_staged_conflict_body),
            severity = CapsuleBannerSeverity.Error,
            action = {
                CapsuleButton(
                    onClick = actions.onSwitchToFullUploads,
                    label = stringResource(R.string.android_importer_staged_conflict_action),
                    emphasis = CapsuleButtonEmphasis.Secondary,
                )
            },
        )
    }
}

@Composable
private fun PlanCounts(plan: ImportPlan) {
    CapsuleCard(modifier = Modifier.fillMaxWidth().testTag(IMPORT_PLAN_SUMMARY_TAG)) {
        CountRow(label = stringResource(R.string.android_importer_plan_added), value = plan.added.size.toString())
        CountRow(label = stringResource(R.string.android_importer_plan_skipped), value = plan.skipped.size.toString())
        CountRow(
            label = stringResource(R.string.android_importer_plan_conflicts),
            value = plan.conflicts.size.toString(),
        )
        CountRow(
            label = stringResource(R.string.android_importer_plan_total_size),
            value = formattedSize(plan.totalSize),
        )
    }
}

@Composable
private fun CountRow(
    label: String,
    value: String,
) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = ScreenPadding, vertical = CountRowPadding),
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text(text = label, style = MaterialTheme.typography.bodyMedium)
        Text(text = value, style = MaterialTheme.typography.titleMediumEmphasized)
    }
}

@Composable
private fun DestinationCard(
    state: ImportPlanUiState,
    plan: ImportPlan,
) {
    CapsuleListItem(
        headline = albumLabel(state.destinationName),
        modifier = Modifier.fillMaxWidth(),
        supporting = destinationRuleLabel(plan.destinationRule),
        overline = stringResource(R.string.android_importer_plan_destination),
    )
}

@Composable
private fun ModeChoice(
    mode: ImportMode,
    onSetMode: (ImportMode) -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(FooterSpacing)) {
        Row(horizontalArrangement = Arrangement.spacedBy(FooterSpacing)) {
            CapsuleToggleButton(
                checked = mode == ImportMode.Copy,
                onCheckedChange = { onSetMode(ImportMode.Copy) },
                label = stringResource(R.string.android_importer_plan_mode_copy),
            )
            CapsuleToggleButton(
                checked = mode == ImportMode.Move,
                onCheckedChange = { onSetMode(ImportMode.Move) },
                label = stringResource(R.string.android_importer_plan_mode_move),
            )
        }
        Text(
            text = stringResource(R.string.android_importer_plan_mode_body),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun SkippedList(plan: ImportPlan) {
    val passedOver = plan.skipped + plan.conflicts
    if (passedOver.isEmpty()) return
    Column {
        CapsuleSectionHeader(title = stringResource(R.string.android_importer_plan_skipped))
        passedOver.forEach { candidate -> SkippedRow(candidate) }
    }
}

@Composable
private fun SkippedRow(candidate: ImportCandidate) {
    CapsuleListItem(
        headline = candidate.sourceLocator.substringAfterLast('/'),
        supporting = decisionLabel(candidate.decision),
    )
}

@Composable
private fun ConfirmFooter(
    state: ImportPlanUiState,
    plan: ImportPlan,
    onConfirm: () -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxWidth().padding(ScreenPadding),
        verticalArrangement = Arrangement.spacedBy(FooterSpacing),
    ) {
        if (plan.conflicts.isNotEmpty()) {
            Text(
                text = stringResource(R.string.android_importer_plan_conflicts_block),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }
        val label =
            if (state.isStreaming) {
                stringResource(R.string.android_importer_streaming_action)
            } else {
                stringResource(R.string.android_importer_plan_confirm).withArguments("count" to plan.added.size)
            }
        CapsuleButton(
            onClick = onConfirm,
            label = label,
            modifier = Modifier.fillMaxWidth().testTag(IMPORT_CONFIRM_TAG),
            emphasis = CapsuleButtonEmphasis.Primary,
            enabled = state.canConfirm,
        )
    }
}

/** The vertical gap between the plan's stacked blocks. */
private val BlockSpacing = 16.dp

/** The inset above and below one count row. */
private val CountRowPadding = 8.dp
