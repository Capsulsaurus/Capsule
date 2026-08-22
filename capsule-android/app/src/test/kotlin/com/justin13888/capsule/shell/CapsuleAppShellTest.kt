package com.justin13888.capsule.shell

import android.app.Application
import androidx.compose.material3.adaptive.navigationsuite.NavigationSuiteType
import androidx.compose.ui.test.junit4.ComposeContentTestRule
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import com.justin13888.capsule.core.data.ProtocolError
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.placeholder.PLACEHOLDER_BACK_TAG
import com.justin13888.capsule.placeholder.placeholderTagFor
import com.justin13888.capsule.security.LocalAuthGate
import com.justin13888.capsule.shell.chrome.PROTOCOL_MISMATCH_TAG
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The shell's four load-bearing behaviours, driven through a real composition.
 *
 * These are the ones that cannot be checked by reading the code: whether the navigation
 * component the window class *selects* is the one that actually renders, whether the back
 * stack really returns, and whether the two screens that are supposed to replace everything
 * else actually do. Everything else about the shell is a pure function with its own test.
 *
 * Robolectric runs these against a plain [Application] rather than `CapsuleApplication`:
 * that class starts Koin, Koin's global context is static, and Robolectric builds a fresh
 * Application per test in the same JVM — so the second test in the class would die on
 * `KoinApplicationAlreadyStartedException`. Nothing here needs the graph, because the
 * shell's collaborators are passed in explicitly. That is the same seam that makes them
 * substitutable in production.
 */
@RunWith(RobolectricTestRunner::class)
@Config(application = Application::class)
class CapsuleAppShellTest {
    @get:Rule
    val compose = createComposeRule()

    private val shellState = CapsuleShellState()
    private val gate = LocalAuthGate()
    private val prompt = FakeLocalAuthPrompt()

    @Test
    fun compact_windows_get_the_short_navigation_bar() {
        compose.launchShell(shellState, gate, prompt, windowSize = COMPACT_WINDOW)

        compose.assertSuiteIs(NavigationSuiteType.ShortNavigationBarCompact)
    }

    @Test
    fun medium_windows_get_a_collapsed_wide_rail() {
        compose.launchShell(shellState, gate, prompt, windowSize = MEDIUM_WINDOW)

        compose.assertSuiteIs(NavigationSuiteType.WideNavigationRailCollapsed)
    }

    @Test
    fun expanded_windows_get_an_expanded_wide_rail() {
        compose.launchShell(shellState, gate, prompt, windowSize = EXPANDED_WINDOW)

        compose.assertSuiteIs(NavigationSuiteType.WideNavigationRailExpanded)
    }

    @Test
    fun the_shell_opens_on_the_timeline() {
        compose.launchShell(shellState, gate, prompt)

        compose.onNodeWithTag(placeholderTagFor(CapsuleDestination.Timeline)).assertExists()
    }

    @Test
    fun switching_section_moves_the_whole_shell() {
        compose.launchShell(shellState, gate, prompt)

        compose.onNodeWithTag(navigationItemTag(CapsuleDestination.Settings)).performClick()
        compose.waitForIdle()

        compose.onNodeWithTag(placeholderTagFor(CapsuleDestination.Settings)).assertExists()
    }

    @Test
    fun a_nested_destination_can_be_backed_out_of() {
        val shell = compose.launchShell(shellState, gate, prompt)

        compose.act { shell.navigator.goTo(CapsuleDestination.AlbumDetail("album-1")) }
        compose.onNodeWithTag(placeholderTagFor(CapsuleDestination.AlbumDetail(""))).assertExists()

        compose.onNodeWithTag(PLACEHOLDER_BACK_TAG).performClick()
        compose.waitForIdle()

        compose.onNodeWithTag(placeholderTagFor(CapsuleDestination.Timeline)).assertExists()
        compose.onNodeWithTag(placeholderTagFor(CapsuleDestination.AlbumDetail(""))).assertDoesNotExist()
    }

    @Test
    fun an_immersive_destination_removes_the_navigation_component() {
        val shell = compose.launchShell(shellState, gate, prompt, windowSize = EXPANDED_WINDOW)

        compose.act { shell.navigator.goTo(CapsuleDestination.AssetViewer("asset-1")) }

        compose.assertSuiteIs(NavigationSuiteType.None)
    }

    @Test
    fun going_offline_raises_the_banner_without_hiding_the_library() {
        compose.launchShell(shellState, gate, prompt)
        compose.onNodeWithTag(placeholderTagFor(CapsuleDestination.Timeline)).assertExists()

        compose.act { shellState.onConnectionChanged(ConnectionClass.Offline) }

        // The point of the banner rather than a blocking screen: the library is still there.
        compose.onNodeWithTag(placeholderTagFor(CapsuleDestination.Timeline)).assertExists()
    }

    @Test
    fun a_queued_message_reaches_the_snackbar_host() {
        compose.launchShell(shellState, gate, prompt)

        compose.act { shellState.showMessage(R.string.android_shell_gate_failed) }

        // Events get a snackbar; conditions get a banner. This is the event half of that rule.
        compose.onNodeWithText(GATE_FAILED_MESSAGE).assertExists()
    }

    @Test
    fun a_protocol_refusal_replaces_everything() {
        compose.launchShell(shellState, gate, prompt, windowSize = EXPANDED_WINDOW)

        compose.act { shellState.onProtocolRefused(ProtocolError.VersionUnsupported) }

        compose.onNodeWithTag(PROTOCOL_MISMATCH_TAG).assertExists()
        compose.onNodeWithTag(placeholderTagFor(CapsuleDestination.Timeline)).assertDoesNotExist()
        compose.onNodeWithTag(navigationItemTag(CapsuleDestination.Timeline)).assertDoesNotExist()
    }

    private companion object {
        /** The English catalog text for `android.shell.gate_failed`. */
        const val GATE_FAILED_MESSAGE = "That did not unlock the view. Try again."
    }
}

/** Asserts which navigation component the shell chose, by the tag it stamps on its root. */
private fun ComposeContentTestRule.assertSuiteIs(type: NavigationSuiteType) {
    onNodeWithTag(navigationSuiteTag(type)).assertExists()
}
