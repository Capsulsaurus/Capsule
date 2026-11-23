package com.justin13888.capsule.feature.viewer

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.BrokenImage
import androidx.compose.material.icons.filled.CloudOff
import androidx.compose.material.icons.filled.PhonelinkRing
import androidx.compose.material.icons.filled.Shield
import androidx.compose.material.icons.filled.Upgrade
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.RepresentationTier
import kotlinx.collections.immutable.ImmutableList

/** Test handle for one degraded-state banner, so a test can prove the five stay distinct. */
fun degradationTag(degradation: ViewerDegradation): String = "viewer.degraded.${degradation.name}"

/**
 * What the user is offered when a condition is showing.
 *
 * Three of the five have no action at all, and that is the honest answer rather than an
 * oversight: an original still uploading from another device and a format this device cannot
 * decode are both things the user can do exactly nothing about, and a button that pretended
 * otherwise would waste a tap and teach them the app lies.
 */
enum class DegradationAction {
    /** Retry the fetch that walked down the ladder. */
    Retry,

    /** Open the store listing, because this build is behind the peer that wrote the asset. */
    UpdateApp,

    /** Open the surface holding the refused change. */
    ReviewQuarantine,
}

/**
 * Every condition on the current asset, drawn as its own banner.
 *
 * One banner each rather than a combined "this asset is degraded" line. The five have
 * different causes, different remedies and different answers to "is my photo safe?", and
 * collapsing them would train the user to dismiss all five on sight.
 *
 * @param degradations the conditions in force, already ordered.
 * @param heldTier the best rung this device can actually draw.
 * @param deviceLabel the device holding an unuploaded original, when the directory names one.
 * @param formatToken the raw content type of an undecodable file, when it carries one.
 * @param onAction invoked with the remediation the user chose.
 */
@Composable
fun DegradationBanners(
    degradations: ImmutableList<ViewerDegradation>,
    heldTier: RepresentationTier,
    deviceLabel: String?,
    formatToken: String?,
    onAction: (DegradationAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    if (degradations.isEmpty()) return
    Column(
        modifier = modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(BannerSpacing),
    ) {
        degradations.forEach { degradation ->
            CapsuleBanner(
                title = degradation.title(deviceLabel),
                modifier = Modifier.fillMaxWidth().testTag(degradationTag(degradation)),
                body = degradation.body(heldTier, formatToken),
                severity = degradation.severity(),
                icon = degradation.icon(),
                action = degradation.action()?.let { { DegradationButton(it, onAction) } },
            )
        }
    }
}

/** The one button a banner may carry. */
@Composable
private fun DegradationButton(
    action: DegradationAction,
    onAction: (DegradationAction) -> Unit,
) {
    CapsuleButton(
        onClick = { onAction(action) },
        label = stringResource(action.labelRes()),
        emphasis = CapsuleButtonEmphasis.Tertiary,
    )
}

@StringRes
private fun DegradationAction.labelRes(): Int =
    when (this) {
        DegradationAction.Retry -> R.string.viewer_action_retry
        DegradationAction.UpdateApp -> R.string.android_shell_protocol_block_action
        DegradationAction.ReviewQuarantine -> R.string.viewer_degraded_quarantined_action
    }

/** The remediation this condition offers, or `null` when there is genuinely nothing to do. */
internal fun ViewerDegradation.action(): DegradationAction? =
    when (this) {
        ViewerDegradation.FullResolutionUnavailable -> DegradationAction.Retry
        ViewerDegradation.OriginalOnAnotherDevice -> null
        ViewerDegradation.UnreadableHere -> null
        ViewerDegradation.CreatedByNewerVersion -> DegradationAction.UpdateApp
        ViewerDegradation.Quarantined -> DegradationAction.ReviewQuarantine
    }

/**
 * How loudly each condition is allowed to speak.
 *
 * Only the quarantine is an error, because only it means something was refused rather than
 * merely unavailable. The forward-schema case is a warning — the app is behind — and the
 * other three are the design system's *degraded* tone, which is exactly the state they name.
 */
@Composable
private fun ViewerDegradation.severity(): CapsuleBannerSeverity =
    when (this) {
        ViewerDegradation.FullResolutionUnavailable -> CapsuleBannerSeverity.Degraded
        ViewerDegradation.OriginalOnAnotherDevice -> CapsuleBannerSeverity.Info
        ViewerDegradation.UnreadableHere -> CapsuleBannerSeverity.Degraded
        ViewerDegradation.CreatedByNewerVersion -> CapsuleBannerSeverity.Warning
        ViewerDegradation.Quarantined -> CapsuleBannerSeverity.Error
    }

/** A different glyph per condition, so the five are distinguishable without reading. */
private fun ViewerDegradation.icon(): ImageVector =
    when (this) {
        ViewerDegradation.FullResolutionUnavailable -> Icons.Filled.CloudOff
        ViewerDegradation.OriginalOnAnotherDevice -> Icons.Filled.PhonelinkRing
        ViewerDegradation.UnreadableHere -> Icons.Filled.BrokenImage
        ViewerDegradation.CreatedByNewerVersion -> Icons.Filled.Upgrade
        ViewerDegradation.Quarantined -> Icons.Filled.Shield
    }

@Composable
private fun ViewerDegradation.title(deviceLabel: String?): String =
    when (this) {
        ViewerDegradation.FullResolutionUnavailable ->
            stringResource(R.string.viewer_degraded_full_resolution_title)

        ViewerDegradation.OriginalOnAnotherDevice ->
            stringResource(R.string.viewer_degraded_awaiting_original_title).withArguments(
                "device" to (deviceLabel ?: stringResource(R.string.viewer_degraded_awaiting_original_device_unknown)),
            )

        ViewerDegradation.UnreadableHere -> stringResource(R.string.viewer_degraded_unreadable_title)
        ViewerDegradation.CreatedByNewerVersion -> stringResource(R.string.viewer_degraded_forward_schema_title)
        ViewerDegradation.Quarantined -> stringResource(R.string.viewer_degraded_quarantined_title)
    }

@Composable
private fun ViewerDegradation.body(
    heldTier: RepresentationTier,
    formatToken: String?,
): String =
    when (this) {
        ViewerDegradation.FullResolutionUnavailable ->
            stringResource(R.string.viewer_degraded_full_resolution_body)
                .withArguments("tier" to stringResource(heldTier.labelRes()))

        ViewerDegradation.OriginalOnAnotherDevice -> stringResource(R.string.viewer_degraded_awaiting_original_body)

        ViewerDegradation.UnreadableHere ->
            stringResource(R.string.viewer_degraded_unreadable_body).withArguments(
                "format" to (formatToken ?: stringResource(R.string.viewer_degraded_unreadable_format_unknown)),
            )

        ViewerDegradation.CreatedByNewerVersion -> stringResource(R.string.viewer_degraded_forward_schema_body)
        ViewerDegradation.Quarantined -> stringResource(R.string.viewer_degraded_quarantined_body)
    }

/** The catalogue label for one rung of the representation ladder. */
@StringRes
internal fun RepresentationTier.labelRes(): Int =
    when (this) {
        RepresentationTier.Lqip -> R.string.viewer_tier_lqip
        RepresentationTier.Thumbnail -> R.string.viewer_tier_thumbnail
        RepresentationTier.Preview -> R.string.viewer_tier_preview
        RepresentationTier.Original -> R.string.viewer_tier_original
    }

private val BannerSpacing = 8.dp
