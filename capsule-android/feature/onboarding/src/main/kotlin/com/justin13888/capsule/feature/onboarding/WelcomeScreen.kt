package com.justin13888.capsule.feature.onboarding

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleCard
import com.justin13888.capsule.core.designsystem.component.capsuleDescription
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviews
import com.justin13888.capsule.core.designsystem.theme.LocalReduceMotion
import com.justin13888.capsule.core.i18n.R
import kotlin.math.absoluteValue

/** Test handle for the welcome carousel. */
const val WELCOME_PAGER_TAG: String = "onboarding.welcome-pager"

/** Test handle for the mode choice, which the tour leads to and skipping does not bypass. */
const val MODE_CHOICE_TAG: String = "onboarding.mode-choice"

/** Test handle for the never-signed-in card, so a test can prove it is offered as a peer. */
const val LOCAL_MODE_CARD_TAG: String = "onboarding.mode-local"

/** Test handle for the server card. */
const val SERVER_MODE_CARD_TAG: String = "onboarding.mode-server"

private val ScreenPadding = 24.dp
private val BlockSpacing = 16.dp
private val IndicatorSize = 10.dp
private const val OFF_PAGE_ALPHA = 0.35f
private const val OFF_PAGE_SCALE = 0.06f

/**
 * The first screen of a new install: a short tour, then the choice of what this library is.
 *
 * The two are one screen because they are one decision with a preamble. Skipping the tour
 * lands on the choice rather than past it — there is nothing past it, by design.
 *
 * @param state everything to draw; the screen holds no state of its own beyond the pager's
 *   scroll position, which is transient and belongs to the composition.
 */
@Suppress("LongParameterList")
@Composable
fun WelcomeScreen(
    state: WelcomeUiState,
    onPaneChanged: (Int) -> Unit,
    onAdvance: () -> Unit,
    onSkipTour: () -> Unit,
    onChooseLocalOnly: () -> Unit,
    onConnectServer: () -> Unit,
    modifier: Modifier = Modifier,
) {
    if (state.choosingMode) {
        ModeChoice(
            onChooseLocalOnly = onChooseLocalOnly,
            onConnectServer = onConnectServer,
            modifier = modifier,
        )
    } else {
        WelcomeTour(
            state = state,
            onPaneChanged = onPaneChanged,
            onAdvance = onAdvance,
            onSkipTour = onSkipTour,
            modifier = modifier,
        )
    }
}

@Composable
private fun WelcomeTour(
    state: WelcomeUiState,
    onPaneChanged: (Int) -> Unit,
    onAdvance: () -> Unit,
    onSkipTour: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val pagerState = rememberPagerState(initialPage = state.page) { state.paneCount }
    val reduceMotion = LocalReduceMotion.current
    LaunchedEffect(state.page) {
        if (pagerState.currentPage == state.page) return@LaunchedEffect
        if (reduceMotion) pagerState.scrollToPage(state.page) else pagerState.animateScrollToPage(state.page)
    }
    LaunchedEffect(pagerState) {
        snapshotFlow { pagerState.settledPage }.collect(onPaneChanged)
    }

    Column(modifier = modifier.fillMaxSize().padding(ScreenPadding)) {
        HorizontalPager(
            state = pagerState,
            modifier = Modifier.weight(1f).fillMaxWidth().testTag(WELCOME_PAGER_TAG),
        ) { page ->
            val offset = (pagerState.currentPage - page + pagerState.currentPageOffsetFraction).absoluteValue
            WelcomePaneCard(
                pane = WELCOME_PANE_CONTENT[page],
                modifier =
                    Modifier.graphicsLayer {
                        // Expressive depth on the neighbouring panes, flattened when the user
                        // has asked the system for less motion — the copy is unaffected either
                        // way, so nothing is lost by the reduced variant.
                        if (reduceMotion) return@graphicsLayer
                        val distance = offset.coerceIn(0f, 1f)
                        alpha = 1f - distance * (1f - OFF_PAGE_ALPHA)
                        val scale = 1f - distance * OFF_PAGE_SCALE
                        scaleX = scale
                        scaleY = scale
                    },
            )
        }
        PaneIndicator(page = pagerState.currentPage, paneCount = state.paneCount)
        Spacer(Modifier.size(BlockSpacing))
        CapsuleButton(
            onClick = onAdvance,
            label = stringResource(R.string.android_onboarding_welcome_next),
            modifier = Modifier.fillMaxWidth(),
        )
        CapsuleButton(
            onClick = onSkipTour,
            label = stringResource(R.string.android_onboarding_welcome_skip),
            emphasis = CapsuleButtonEmphasis.Tertiary,
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

@Composable
private fun WelcomePaneCard(
    pane: WelcomePaneContent,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxSize().padding(ScreenPadding),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            text = stringResource(pane.titleRes),
            style = MaterialTheme.typography.headlineLargeEmphasized,
            textAlign = TextAlign.Center,
        )
        Text(
            text = stringResource(pane.bodyRes),
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
    }
}

/**
 * The dots under the tour.
 *
 * Announced as one position rather than as three unlabelled dots: a screen reader that walked
 * a carousel indicator element by element would say nothing useful three times.
 */
@Composable
private fun PaneIndicator(
    page: Int,
    paneCount: Int,
    modifier: Modifier = Modifier,
) {
    val announcement =
        stringResource(R.string.android_onboarding_welcome_page_indicator)
            .withArguments("page" to page + 1, "total" to paneCount)
    Row(
        modifier = modifier.fillMaxWidth().capsuleDescription(announcement),
        horizontalArrangement = Arrangement.spacedBy(BlockSpacing / 2, Alignment.CenterHorizontally),
    ) {
        repeat(paneCount) { index ->
            val active = index == page
            Spacer(
                Modifier
                    .size(IndicatorSize)
                    .clip(CircleShape)
                    .background(
                        if (active) {
                            MaterialTheme.colorScheme.primary
                        } else {
                            MaterialTheme.colorScheme.surfaceVariant
                        },
                    ).clearAndSetSemantics {},
            )
        }
    }
}

/**
 * The two ways to have a Capsule library, drawn as peers.
 *
 * Both are cards of the same weight with a real primary action, and the local one is listed
 * first. A "skip" link would have said the opposite of what the Local Gallery contract says:
 * never signing in is a complete way to use this app, not a decision deferred.
 */
@Composable
private fun ModeChoice(
    onChooseLocalOnly: () -> Unit,
    onConnectServer: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier =
            modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(ScreenPadding)
                .testTag(MODE_CHOICE_TAG),
        verticalArrangement = Arrangement.spacedBy(BlockSpacing),
    ) {
        Text(
            text = stringResource(R.string.android_onboarding_mode_title),
            style = MaterialTheme.typography.headlineMediumEmphasized,
        )
        Text(
            text = stringResource(R.string.android_onboarding_mode_body),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        ModeCard(
            titleRes = R.string.android_onboarding_mode_local_title,
            bodyRes = R.string.android_onboarding_mode_local_body,
            actionRes = R.string.android_onboarding_mode_local_action,
            onClick = onChooseLocalOnly,
            modifier = Modifier.testTag(LOCAL_MODE_CARD_TAG),
        )
        ModeCard(
            titleRes = R.string.android_onboarding_mode_server_title,
            bodyRes = R.string.android_onboarding_mode_server_body,
            actionRes = R.string.android_onboarding_mode_server_action,
            onClick = onConnectServer,
            modifier = Modifier.testTag(SERVER_MODE_CARD_TAG),
        )
    }
}

@Suppress("LongParameterList")
@Composable
private fun ModeCard(
    titleRes: Int,
    bodyRes: Int,
    actionRes: Int,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    CapsuleCard(modifier = modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(ScreenPadding),
            verticalArrangement = Arrangement.spacedBy(BlockSpacing),
        ) {
            Text(text = stringResource(titleRes), style = MaterialTheme.typography.titleLargeEmphasized)
            Text(
                text = stringResource(bodyRes),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            CapsuleButton(
                onClick = onClick,
                label = stringResource(actionRes),
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}

/** One pane of the tour: a headline and a paragraph, both from the catalogue. */
private class WelcomePaneContent(
    val titleRes: Int,
    val bodyRes: Int,
)

private val WELCOME_PANE_CONTENT =
    listOf(
        WelcomePaneContent(
            R.string.android_onboarding_welcome_pane_one_title,
            R.string.android_onboarding_welcome_pane_one_body,
        ),
        WelcomePaneContent(
            R.string.android_onboarding_welcome_pane_two_title,
            R.string.android_onboarding_welcome_pane_two_body,
        ),
        WelcomePaneContent(
            R.string.android_onboarding_welcome_pane_three_title,
            R.string.android_onboarding_welcome_pane_three_body,
        ),
    )

@CapsulePreviews
@Composable
internal fun WelcomeTourPreview() {
    CapsulePreviewSurface {
        WelcomeScreen(
            state = WelcomeUiState(),
            onPaneChanged = {},
            onAdvance = {},
            onSkipTour = {},
            onChooseLocalOnly = {},
            onConnectServer = {},
        )
    }
}

@CapsulePreviews
@Composable
internal fun ModeChoicePreview() {
    CapsulePreviewSurface {
        WelcomeScreen(
            state = WelcomeUiState(choosingMode = true),
            onPaneChanged = {},
            onAdvance = {},
            onSkipTour = {},
            onChooseLocalOnly = {},
            onConnectServer = {},
        )
    }
}
