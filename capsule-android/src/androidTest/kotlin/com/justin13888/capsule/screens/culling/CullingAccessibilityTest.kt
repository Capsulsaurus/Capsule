package com.justin13888.capsule.screens.culling

import androidx.activity.ComponentActivity
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.remember
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.assertContentDescriptionContains
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsFocused
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.accessibility.enableAccessibilityChecks
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performKeyInput
import androidx.compose.ui.test.pressKey
import androidx.compose.ui.test.withKeyDown
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.SdkSuppress
import com.justin13888.capsule.R
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The culling surface under the Compose UI accessibility checks (the Accessibility Test
 * Framework, run on every interaction), with no check suppressed: a whole pass through its
 * buttons, its keys and its TalkBack actions, the sweep's confirmation included.
 *
 * The checks need API 34; Robolectric skips them, so this runs on a device.
 */
@RunWith(AndroidJUnit4::class)
@SdkSuppress(minSdkVersion = 34)
@OptIn(ExperimentalTestApi::class)
class CullingAccessibilityTest {
    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    private lateinit var port: InMemoryCullPort

    @Before
    fun showThePass() {
        compose.enableAccessibilityChecks()
        port = InMemoryCullPort.sample()
        compose.setContent {
            MaterialTheme {
                val viewModel = remember { CullingViewModel(port) }
                CullingScreen(viewModel, navigateBack = {})
            }
        }
        compose.waitForIdle()
    }

    private fun text(id: Int): String = compose.activity.getString(id)

    private fun verb(verb: CullVerb) = compose.onNodeWithTag(CullingTags.verb(verb))

    private val asset get() = compose.onNodeWithTag(CullingTags.ASSET)

    @Test
    fun aWholePassThroughTheButtonsPassesTheChecks() {
        asset.assertContentDescriptionContains(text(R.string.app_culling_flag_unflagged), substring = true)
        verb(CullVerb.PREVIOUS).assertIsNotEnabled()

        verb(CullVerb.PICK).performClick()
        verb(CullVerb.REJECT).performClick()
        verb(CullVerb.PREVIOUS).assertIsEnabled().performClick()
        asset.assertContentDescriptionContains(text(R.string.app_culling_flag_reject), substring = true)
        verb(CullVerb.NEUTRAL).performClick()
        verb(CullVerb.NEXT).performClick()

        // The burst: one step for the stack, a step per member once expanded.
        asset.assertContentDescriptionContains(text(R.string.app_culling_stack_mixed), substring = true)
        verb(CullVerb.EXPAND_STACK).performClick()
        verb(CullVerb.PICK).performClick()
        verb(CullVerb.COLLAPSE_STACK).performClick()
        asset.assertContentDescriptionContains(text(R.string.app_culling_stack_any_pick), substring = true)
        verb(CullVerb.NEXT).performClick()
        compose.waitForIdle()

        assertTrue(port.assets.any { it.stack != null && it.flag == CullFlag.PICK })
    }

    @Test
    fun everyVerdictIsATalkBackActionOnTheAsset() {
        val actions = asset.fetchSemanticsNode().config[SemanticsActions.CustomActions]
        val labels = actions.map { it.label }
        for (verdict in listOf(CullVerb.PICK, CullVerb.NEUTRAL, CullVerb.REJECT, CullVerb.NEXT, CullVerb.PREVIOUS)) {
            assertTrue("${verdict.name} is a custom action", text(verdict.label) in labels)
        }

        compose.runOnUiThread { actions.first { it.label == text(R.string.app_culling_action_reject) }.action() }
        compose.waitForIdle()

        assertEquals(CullFlag.REJECT, port.assets.first().flag)
        asset.assertContentDescriptionContains("2", substring = true)
    }

    @Test
    fun theKeysRunThePassAndTheSweepAsksFirst() {
        asset.assertIsFocused()
        asset.performKeyInput {
            pressKey(Key.P)
            pressKey(Key.DirectionLeft)
            pressKey(Key.X)
        }
        compose.waitForIdle()
        assertEquals(CullFlag.REJECT, port.assets.first().flag)

        asset.performKeyInput { withKeyDown(Key.CtrlLeft) { pressKey(Key.Backspace) } }
        compose.onNodeWithText(text(R.string.app_culling_sweep_message)).assertExists()
        compose.onNodeWithText(text(R.string.app_culling_sweep_confirm)).performClick()
        compose.waitForIdle()

        assertEquals(2, port.trashed.size)
        val done = compose.activity.resources.getQuantityString(R.plurals.app_culling_sweep_done, 2, 2)
        compose.waitUntil(timeoutMillis = 5_000) { compose.onAllNodes(hasText(done)).fetchSemanticsNodes().isNotEmpty() }
    }

    @Test
    fun aDismissedSweepMovesNothing() {
        compose.onNodeWithTag(CullingTags.SWEEP).performClick()
        compose.onNodeWithText(text(R.string.app_common_cancel)).performClick()
        compose.waitForIdle()

        assertTrue(port.trashed.isEmpty())
    }
}
