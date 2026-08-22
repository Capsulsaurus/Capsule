package com.justin13888.capsule.core.designsystem.component

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CloudOff
import androidx.compose.material.icons.filled.ErrorOutline
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.WarningAmber
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme

private val BannerPadding = 16.dp
private val BannerContentSpacing = 12.dp
private val BannerTextSpacing = 4.dp

/** What a banner is telling the user, and therefore how loudly it is allowed to say it. */
enum class CapsuleBannerSeverity {
    /** Nothing is wrong. A one-off explanation or a state the user chose. */
    Info,

    /** Something will go wrong if ignored — approaching quota, an expiring share. */
    Warning,

    /** Something has already failed and needs a decision. */
    Error,

    /** Working, but not fully: proxies instead of originals, a stale search index. */
    Degraded,
}

/**
 * A persistent, in-layout message.
 *
 * Deliberately not a snackbar. Everything a banner says here is a condition rather than
 * an event — the library is degraded, the quota is nearly gone, twelve uploads failed —
 * and a condition that scrolls away after four seconds is a condition the user will hit
 * again without understanding why.
 *
 * @param title already-localized headline.
 * @param body already-localized detail.
 * @param action a slot for one [CapsuleButton]; banners never carry two.
 * @param contentDescription replaces the announcement when the title and body read badly
 *   together out loud.
 */
@Composable
fun CapsuleBanner(
    title: String,
    modifier: Modifier = Modifier,
    body: String? = null,
    severity: CapsuleBannerSeverity = CapsuleBannerSeverity.Info,
    icon: ImageVector? = null,
    action: (@Composable () -> Unit)? = null,
    contentDescription: String? = null,
) {
    Surface(
        modifier = modifier.capsuleDescription(contentDescription),
        shape = MaterialTheme.shapes.large,
        color = severity.containerColor(),
        contentColor = severity.onContainerColor(),
    ) {
        Row(
            modifier = Modifier.padding(BannerPadding),
            horizontalArrangement = Arrangement.spacedBy(BannerContentSpacing),
            verticalAlignment = Alignment.Top,
        ) {
            Icon(imageVector = icon ?: severity.defaultIcon(), contentDescription = null)
            Column(
                modifier = Modifier.weight(1f),
                verticalArrangement = Arrangement.spacedBy(BannerTextSpacing),
            ) {
                Text(text = title, style = MaterialTheme.typography.titleMediumEmphasized)
                if (body != null) {
                    Text(text = body, style = MaterialTheme.typography.bodyMedium)
                }
            }
            action?.invoke()
        }
    }
}

private fun CapsuleBannerSeverity.defaultIcon(): ImageVector =
    when (this) {
        CapsuleBannerSeverity.Info -> Icons.Filled.Info
        CapsuleBannerSeverity.Warning -> Icons.Filled.WarningAmber
        CapsuleBannerSeverity.Error -> Icons.Filled.ErrorOutline
        CapsuleBannerSeverity.Degraded -> Icons.Filled.CloudOff
    }

@Composable
private fun CapsuleBannerSeverity.containerColor(): Color =
    when (this) {
        CapsuleBannerSeverity.Info -> MaterialTheme.colorScheme.secondaryContainer
        CapsuleBannerSeverity.Warning -> CapsuleTheme.colors.syncPending.container
        CapsuleBannerSeverity.Error -> MaterialTheme.colorScheme.errorContainer
        CapsuleBannerSeverity.Degraded -> CapsuleTheme.colors.degraded.container
    }

@Composable
private fun CapsuleBannerSeverity.onContainerColor(): Color =
    when (this) {
        CapsuleBannerSeverity.Info -> MaterialTheme.colorScheme.onSecondaryContainer
        CapsuleBannerSeverity.Warning -> CapsuleTheme.colors.syncPending.onContainer
        CapsuleBannerSeverity.Error -> MaterialTheme.colorScheme.onErrorContainer
        CapsuleBannerSeverity.Degraded -> CapsuleTheme.colors.degraded.onContainer
    }
