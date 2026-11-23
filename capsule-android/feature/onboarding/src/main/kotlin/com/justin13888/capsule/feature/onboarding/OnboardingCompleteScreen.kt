package com.justin13888.capsule.feature.onboarding

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.PhotoLibrary
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
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviews
import com.justin13888.capsule.core.i18n.R

/** Test handle for the media-permission primer. */
const val MEDIA_PRIMER_TAG: String = "onboarding.media-primer"

/** Test handle for the partial-access state, which is an outcome rather than a failure. */
const val PARTIAL_ACCESS_TAG: String = "onboarding.media-partial"

/** Test handle for the hand-off out of onboarding. */
const val COMPLETE_ACTION_TAG: String = "onboarding.complete-action"

private val ScreenPadding = 24.dp
private val BlockSpacing = 16.dp

/**
 * The media-permission primer and the hand-off into the library.
 *
 * Denial is not an error state here. A library with no media access is empty rather than
 * broken — importing from a file still works, and the never-signed-in mode was never going to
 * ask for a server — so the copy says what is true and offers a way to change it.
 */
@Suppress("LongParameterList")
@Composable
fun OnboardingCompleteScreen(
    state: CompletionUiState,
    onRequestAccess: () -> Unit,
    onOpenAppSettings: () -> Unit,
    onFinish: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(ScreenPadding),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        Text(
            text = stringResource(R.string.android_onboarding_complete_title),
            style = MaterialTheme.typography.headlineMediumEmphasized,
        )
        Text(
            text =
                stringResource(
                    when (state.mode) {
                        OnboardingMode.LocalOnly -> R.string.android_onboarding_complete_body_local
                        OnboardingMode.ServerBacked -> R.string.android_onboarding_complete_body_server
                    },
                ),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        when {
            state.showingPrimer -> MediaPrimer(onRequestAccess)
            state.access == MediaAccess.Partial -> PartialAccess(onRequestAccess)
            state.access == MediaAccess.Full -> FullAccess()
            else -> DeniedAccess(onOpenAppSettings)
        }
        CapsuleButton(
            onClick = onFinish,
            label = stringResource(R.string.android_onboarding_complete_action),
            modifier = Modifier.fillMaxWidth().testTag(COMPLETE_ACTION_TAG),
        )
    }
}

@Composable
private fun MediaPrimer(
    onRequestAccess: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleEmptyState(
        title = stringResource(R.string.android_onboarding_permission_title),
        body = stringResource(R.string.android_onboarding_permission_body),
        icon = Icons.Filled.PhotoLibrary,
        modifier = modifier.fillMaxWidth().testTag(MEDIA_PRIMER_TAG),
        action = {
            CapsuleButton(
                onClick = onRequestAccess,
                label = stringResource(R.string.android_onboarding_permission_grant),
            )
        },
    )
}

/**
 * Android 14's "select photos" answer, stated as the deliberate choice it is.
 *
 * Rendered as information rather than as a warning, because a user who narrowed what this app
 * can see made a decision the app should respect. The only action offered widens it, and it
 * is secondary.
 */
@Composable
private fun PartialAccess(
    onRequestAccess: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleBanner(
        title = stringResource(R.string.android_onboarding_permission_partial_title),
        body = stringResource(R.string.android_onboarding_permission_partial_body),
        severity = CapsuleBannerSeverity.Info,
        modifier = modifier.fillMaxWidth().testTag(PARTIAL_ACCESS_TAG),
        action = {
            CapsuleButton(
                onClick = onRequestAccess,
                label = stringResource(R.string.android_onboarding_permission_partial_action),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

@Composable
private fun FullAccess(modifier: Modifier = Modifier) {
    CapsuleBanner(
        title = stringResource(R.string.android_onboarding_permission_full_title),
        severity = CapsuleBannerSeverity.Info,
        modifier = modifier.fillMaxWidth(),
    )
}

@Composable
private fun DeniedAccess(
    onOpenAppSettings: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleBanner(
        title = stringResource(R.string.android_onboarding_permission_denied_title),
        body = stringResource(R.string.android_onboarding_permission_denied_body),
        severity = CapsuleBannerSeverity.Degraded,
        modifier = modifier.fillMaxWidth(),
        action = {
            CapsuleButton(
                onClick = onOpenAppSettings,
                label = stringResource(R.string.android_onboarding_permission_denied_action),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

@CapsulePreviews
@Composable
internal fun PrimerPreview() {
    CapsulePreviewSurface {
        OnboardingCompleteScreen(
            state = CompletionUiState(mode = OnboardingMode.LocalOnly),
            onRequestAccess = {},
            onOpenAppSettings = {},
            onFinish = {},
        )
    }
}

@CapsulePreviews
@Composable
internal fun PartialPreview() {
    CapsulePreviewSurface {
        OnboardingCompleteScreen(
            state =
                CompletionUiState(
                    mode = OnboardingMode.ServerBacked,
                    access = MediaAccess.Partial,
                    hasAsked = true,
                ),
            onRequestAccess = {},
            onOpenAppSettings = {},
            onFinish = {},
        )
    }
}
