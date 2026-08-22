package com.justin13888.capsule.shell

import android.app.Application
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import com.justin13888.capsule.core.model.GatedView
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.security.LocalAuthGate
import com.justin13888.capsule.shell.chrome.BIOMETRIC_GATE_TAG
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The SR1 gate, exercised the way a user meets it: through the shell, on a real destination.
 *
 * The gate's arithmetic is covered by `LocalAuthGateTest`. What this adds is the part that
 * arithmetic cannot show — that the gated screen is *not composed* while the gate is holding.
 * A gate that merely drew a cover over a live screen would pass a "the cover is visible"
 * assertion while the screen behind it loaded and cached decrypted thumbnails.
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
class BiometricGateTest {
    @get:Rule(order = 0)
    val graph = CapsuleGraphRule()

    @get:Rule(order = 1)
    val compose = createComposeRule()

    private val shellState = CapsuleShellState()
    private val gate = LocalAuthGate()
    private val prompt = FakeLocalAuthPrompt()

    @Test
    fun trash_is_covered_until_the_user_authenticates() {
        val shell = compose.launchShell(shellState, gate, prompt)

        compose.act { shell.navigator.goTo(CapsuleDestination.Trash) }

        compose.onNodeWithTag(BIOMETRIC_GATE_TAG).assertExists()
        compose.onNodeWithTag(AlbumsTestTags.TRASH).assertDoesNotExist()
    }

    @Test
    fun authenticating_opens_the_trash() {
        val shell = compose.launchShell(shellState, gate, prompt)
        compose.act { shell.navigator.goTo(CapsuleDestination.Trash) }

        compose.onNodeWithText(UNLOCK_LABEL).performClick()
        compose.waitForIdle()

        compose.onNodeWithTag(BIOMETRIC_GATE_TAG).assertDoesNotExist()
        assertEquals(listOf(GatedView.RecentlyDeleted), prompt.promptedFor)
    }

    @Test
    fun a_refused_prompt_leaves_the_gate_closed() {
        prompt.authenticates = false
        val shell = compose.launchShell(shellState, gate, prompt)
        compose.act { shell.navigator.goTo(CapsuleDestination.Trash) }

        compose.onNodeWithText(UNLOCK_LABEL).performClick()
        compose.waitForIdle()

        compose.onNodeWithTag(BIOMETRIC_GATE_TAG).assertExists()
        compose.onNodeWithTag(AlbumsTestTags.TRASH).assertDoesNotExist()
        assertTrue("the gate never raised the prompt", prompt.attempts > 0)
    }

    @Test
    fun unlocking_the_trash_does_not_unlock_hidden() {
        val shell = compose.launchShell(shellState, gate, prompt)
        compose.act { shell.navigator.goTo(CapsuleDestination.Trash) }
        compose.onNodeWithText(UNLOCK_LABEL).performClick()
        compose.waitForIdle()

        compose.act { shell.navigator.goTo(CapsuleDestination.Hidden) }

        // Per-view grants are the whole point: one is about recovering a deleted photo, the
        // other about what the user chose to keep out of sight.
        compose.onNodeWithTag(BIOMETRIC_GATE_TAG).assertExists()
        compose.onNodeWithTag(AlbumsTestTags.HIDDEN).assertDoesNotExist()
    }

    @Test
    fun an_ungated_destination_is_never_covered() {
        val shell = compose.launchShell(shellState, gate, prompt)

        compose.act { shell.navigator.goTo(CapsuleDestination.AuditLog) }

        compose.onNodeWithTag(BIOMETRIC_GATE_TAG).assertDoesNotExist()
        assertEquals(0, prompt.attempts)
    }

    private companion object {
        /** The English catalog text for `android.shell.gate_unlock`. */
        const val UNLOCK_LABEL = "Unlock"
    }
}
