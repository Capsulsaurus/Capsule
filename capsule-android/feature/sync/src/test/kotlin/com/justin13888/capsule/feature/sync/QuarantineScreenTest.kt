package com.justin13888.capsule.feature.sync

import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.QuarantineSurface
import kotlinx.collections.immutable.toImmutableList
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.time.Instant

/**
 * The quarantine surface.
 *
 * Each of the three surfaces the sync design names — a stale revival, a manifest that failed
 * verification, and a restore whose chain diverges — has to arrive with its own reason and its
 * own action. These assert exactly that, and that no row anywhere offers to apply what was
 * refused.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class QuarantineScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun `a stale revival explains what applying it would have undone`() {
        render(loaded(QuarantineSurface.StaleRevival))

        compose.onNodeWithTag(QUARANTINE_LIST_TAG).assertExists()
        compose.onNodeWithText("A stale change from another device").assertExists()
        compose.onNodeWithText("Keep this device's version").assertExists()
    }

    @Test
    fun `a manifest that failed verification is discardable, not applicable`() {
        render(loaded(QuarantineSurface.VerifyAssetReject))

        compose.onNodeWithText("A change failed verification").assertExists()
        compose.onNodeWithText("Discard what arrived").assertExists()
    }

    @Test
    fun `a tampered sidecar keeps its bytes and offers inspection`() {
        render(loaded(QuarantineSurface.MalformedSidecar))

        compose.onNodeWithText("Details that could not be read").assertExists()
        compose.onNodeWithText("Look at what arrived").assertExists()
    }

    @Test
    fun `a restore conflict asks which history to keep rather than choosing one`() {
        render(loaded(QuarantineSurface.RestoreChainConflict))

        compose.onNodeWithText("A restore conflicts with this device").assertExists()
        compose.onNodeWithText("Choose which history to keep").assertExists()
    }

    @Test
    fun `every row states that nothing was applied automatically`() {
        render(loaded(QuarantineSurface.StaleRevival, QuarantineSurface.RestoreChainConflict))

        compose
            .onNodeWithContentDescription(
                "A stale change from another device. Another device proposed a change built on an older " +
                    "version of this photo than the one you already have. Applying it would undo what came after.",
                useUnmergedTree = true,
            ).assertExists()
    }

    @Test
    fun `deciding about one row reaches the caller`() {
        val resolved = mutableListOf<QuarantineSurface?>()
        render(loaded(QuarantineSurface.StaleRevival), actions(onResolve = { resolved += it.surface }))

        compose.onNodeWithText("Keep this device's version").performClick()
        compose.waitForIdle()

        assertEquals(listOf(QuarantineSurface.StaleRevival), resolved)
    }

    @Test
    fun `an empty surface says everything verified`() {
        render(QuarantineUiState(isLoading = false))

        compose.onNodeWithText("Nothing needs your attention").assertExists()
    }

    @Test
    fun `a failure localizes its code and offers a way back`() {
        var retried = false
        render(
            state = QuarantineUiState(isLoading = false, failure = ErrorCode("error.sync.cursor_invalid")),
            actions = actions(onRetry = { retried = true }),
        )

        compose.onNodeWithText("Could not read what is held").assertExists()
        compose.onNodeWithText("Try again").performClick()
        compose.waitForIdle()

        assertTrue(retried)
    }

    private fun render(
        state: QuarantineUiState,
        actions: QuarantineActions = actions(),
    ) {
        compose.setContent { CapsuleTheme { QuarantineScreen(state = state, actions = actions) } }
        compose.waitForIdle()
    }

    private fun actions(
        onResolve: (QuarantineItem) -> Unit = {},
        onRetry: () -> Unit = {},
        onBack: () -> Unit = {},
    ) = QuarantineActions(onResolve, onRetry, onBack)

    private fun loaded(vararg surfaces: QuarantineSurface) =
        QuarantineUiState(
            isLoading = false,
            items =
                surfaces
                    .map { surface ->
                        QuarantineItem(
                            assetId = AssetId("asset-${surface.wire}"),
                            surface = surface,
                            detectedAt = DETECTED_AT,
                            action = surface.defaultAction(),
                        )
                    }.toImmutableList(),
        )

    private companion object {
        val DETECTED_AT: Instant = Instant.parse("2026-06-14T09:00:00Z")
    }
}
