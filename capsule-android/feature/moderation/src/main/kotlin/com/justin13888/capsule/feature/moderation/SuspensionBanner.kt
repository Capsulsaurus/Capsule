package com.justin13888.capsule.feature.moderation

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
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
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.SuspensionEffects

/** Test handle for the suspended-account banner. */
const val MODERATION_SUSPENSION_TAG: String = "moderation.suspension"

internal val ScreenInset = 16.dp
internal val ScreenSpacing = 12.dp

/**
 * The standing notice that this account is suspended, and what that does and does not mean.
 *
 * Public because a suspension is not a moderation-screen fact: it changes what happens when
 * the user tries to upload or share, so any surface that offers those may render it. The
 * feature exposes the banner rather than the reasoning behind it, which keeps one wording of
 * "your data is untouched" in the app instead of one per screen that noticed.
 *
 * Every effect is spelled out, including the ones that are still *allowed*. A user told only
 * what they have lost will assume they have lost everything, and the one capability that
 * matters most here — signing every session out — is precisely the one a suspended user is
 * most likely to need and least likely to guess they still have.
 *
 * @param effects what the account can still do, from the repository.
 * @param onAppeal opens the appeal flow, or `null` where there is nothing to appeal against.
 */
@Composable
fun ModerationSuspensionBanner(
    effects: SuspensionEffects,
    modifier: Modifier = Modifier,
    onAppeal: (() -> Unit)? = null,
) {
    CapsuleBanner(
        title = stringResource(R.string.android_moderation_suspension_title),
        modifier = modifier.fillMaxWidth().testTag(MODERATION_SUSPENSION_TAG),
        body = stringResource(R.string.android_moderation_suspension_body),
        severity = CapsuleBannerSeverity.Warning,
        action =
            onAppeal?.let {
                {
                    CapsuleButton(
                        onClick = it,
                        label = stringResource(R.string.android_moderation_suspension_appeal),
                        emphasis = CapsuleButtonEmphasis.Secondary,
                    )
                }
            },
    )
    SuspensionEffectList(effects = effects, modifier = Modifier.padding(horizontal = ScreenInset))
}

/**
 * The four capabilities a suspension decides, each stated either way.
 *
 * Rendered as plain lines rather than as a list of denials so that "you can still sign every
 * session out" reads as the standing guarantee it is, not as a consolation.
 */
@Composable
private fun SuspensionEffectList(
    effects: SuspensionEffects,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier) {
        SuspensionEffectLine(
            if (effects.canRead) {
                R.string.android_moderation_suspension_can_read
            } else {
                R.string.android_moderation_suspension_no_read
            },
        )
        SuspensionEffectLine(
            if (effects.canUpload) {
                R.string.android_moderation_suspension_can_upload
            } else {
                R.string.android_moderation_suspension_no_upload
            },
        )
        SuspensionEffectLine(
            if (effects.canCreateLinks) {
                R.string.android_moderation_suspension_can_links
            } else {
                R.string.android_moderation_suspension_no_links
            },
        )
        SuspensionEffectLine(
            if (effects.canRevokeAllSessions) {
                R.string.android_moderation_suspension_can_revoke
            } else {
                R.string.android_moderation_suspension_no_revoke
            },
        )
    }
}

@Composable
private fun SuspensionEffectLine(
    @StringRes textRes: Int,
) {
    Text(
        text = stringResource(textRes),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(top = 4.dp),
    )
}
