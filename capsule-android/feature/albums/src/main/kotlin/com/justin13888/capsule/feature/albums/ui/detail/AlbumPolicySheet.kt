package com.justin13888.capsule.feature.albums.ui.detail

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.designsystem.component.CapsuleSheet
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.feature.albums.model.AlbumPolicy
import com.justin13888.capsule.feature.albums.model.UpgradePhase
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.common.historyPolicyLabel
import com.justin13888.capsule.feature.albums.ui.common.withArgs

/**
 * The three values an album fixed at creation, and the only way to change them.
 *
 * Read-only, with the reason stated on the sheet. `history_policy`, the protocol pin and the
 * default retention are album-level policy, and the design changes them "only through an
 * album upgrade ceremony, never ad hoc" — so an editable field here would be a field that
 * lies about what it does.
 */
@Composable
internal fun AlbumPolicySheet(
    state: AlbumDetailUiState.Content,
    actions: AlbumDetailActions,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleSheet(onDismissRequest = onDismiss, modifier = modifier.testTag(AlbumsTestTags.POLICY)) {
        CapsuleSectionHeader(title = stringResource(R.string.albums_detail_policy))
        state.policy?.let { policy -> PolicyRows(policy) }
        Text(
            text = stringResource(R.string.albums_policy_read_only),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = GUTTER, vertical = GUTTER / 2),
        )
        UpgradeSection(policy = state.policy, phase = state.upgrade, actions = actions)
    }
}

@Composable
private fun PolicyRows(policy: AlbumPolicy) {
    CapsuleListItem(
        headline = stringResource(R.string.albums_policy_history),
        supporting = historyPolicyLabel(policy.historyPolicy),
        modifier = Modifier.testTag("${AlbumsTestTags.POLICY}:history"),
    )
    CapsuleListItem(
        headline = stringResource(R.string.albums_policy_protocol_pin),
        supporting = policy.protocolPin,
        modifier = Modifier.testTag("${AlbumsTestTags.POLICY}:pin"),
    )
    CapsuleListItem(
        headline = stringResource(R.string.albums_policy_retention),
        supporting = stringResource(R.string.albums_policy_days).withArgs("days" to policy.defaultRetention.toDays()),
        modifier = Modifier.testTag("${AlbumsTestTags.POLICY}:retention"),
    )
}

/**
 * The upgrade ceremony, one phase at a time.
 *
 * Shown as a sequence rather than a single "upgrading…" spinner because the phases are what
 * the user is waiting on and they fail differently: a drain that will not finish is a
 * different problem from a member whose copy of the album disagrees, and only one of them is
 * worth waiting out. The abort button stays live until the tombstone commit, which is the
 * atomic cutover — after it there is nothing left to abort.
 */
@Composable
private fun UpgradeSection(
    policy: AlbumPolicy?,
    phase: UpgradePhase,
    actions: AlbumDetailActions,
) {
    Column(modifier = Modifier.padding(GUTTER).testTag(AlbumsTestTags.UPGRADE)) {
        if (policy?.isUpgradable != true && !phase.isRunning) {
            Text(
                text = stringResource(R.string.albums_policy_upgrade_unavailable),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            return@Column
        }
        val target = policy?.upgradeAvailableTo.orEmpty()
        Text(
            text = stringResource(R.string.albums_upgrade_body).withArgs("version" to target),
            style = MaterialTheme.typography.bodyMedium,
        )
        UpgradeStatus(phase)
        UpgradeControls(phase, actions)
    }
}

@Composable
private fun UpgradeStatus(phase: UpgradePhase) {
    val label = upgradePhaseLabel(phase) ?: return
    val severity =
        when (phase) {
            is UpgradePhase.Aborted -> CapsuleBannerSeverity.Warning
            is UpgradePhase.Complete -> CapsuleBannerSeverity.Info
            else -> CapsuleBannerSeverity.Degraded
        }
    CapsuleBanner(
        title = label,
        modifier = Modifier.padding(vertical = GUTTER / 2).testTag("${AlbumsTestTags.UPGRADE}:phase"),
        body = upgradePhaseDetail(phase),
        severity = severity,
    )
}

@Composable
private fun UpgradeControls(
    phase: UpgradePhase,
    actions: AlbumDetailActions,
) {
    Row(horizontalArrangement = Arrangement.spacedBy(GUTTER / 2)) {
        if (!phase.isRunning) {
            CapsuleButton(
                onClick = actions.onStartUpgrade,
                label = stringResource(R.string.albums_upgrade_start),
                modifier = Modifier.testTag("${AlbumsTestTags.UPGRADE}:start"),
            )
            return@Row
        }
        CapsuleButton(
            onClick = actions.onAdvanceUpgrade,
            label = stringResource(R.string.albums_upgrade_advance),
            modifier = Modifier.testTag("${AlbumsTestTags.UPGRADE}:advance"),
        )
        CapsuleButton(
            onClick = actions.onAbortUpgrade,
            label = stringResource(R.string.albums_upgrade_abort),
            emphasis = CapsuleButtonEmphasis.Destructive,
            modifier = Modifier.testTag("${AlbumsTestTags.UPGRADE}:abort"),
        )
    }
}
