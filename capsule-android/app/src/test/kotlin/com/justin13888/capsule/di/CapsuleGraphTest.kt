package com.justin13888.capsule.di

import androidx.compose.ui.test.junit4.v2.createComposeRule
import com.justin13888.capsule.CapsuleApp
import com.justin13888.capsule.security.LocalAuthGate
import com.justin13888.capsule.shell.CapsuleShellState
import org.junit.After
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertSame
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.koin.core.context.GlobalContext
import org.koin.core.context.stopKoin
import org.robolectric.RobolectricTestRunner

/**
 * Boots the real application and proves the graph it assembles actually resolves.
 *
 * Koin fails lazily by default — a missing binding surfaces at the screen that needed it,
 * which for most of this app would be weeks after the mistake was made. `CapsuleApplication`
 * starts the graph eagerly for that reason, and this test is what makes that promise real:
 * it runs the manifest's application, then renders the app root, which is the only consumer
 * that resolves anything today.
 *
 * Unlike the shell tests, this one *wants* `CapsuleApplication`, so it tears the global Koin
 * context down after each case rather than swapping the application out.
 */
@RunWith(RobolectricTestRunner::class)
class CapsuleGraphTest {
    @get:Rule
    val compose = createComposeRule()

    @After
    fun stopTheGraph() {
        stopKoin()
    }

    @Test
    fun the_application_starts_the_graph() {
        assertNotNull("CapsuleApplication did not start Koin", GlobalContext.getOrNull())
    }

    @Test
    fun the_shell_singletons_resolve_and_are_shared() {
        val koin = requireNotNull(GlobalContext.getOrNull())

        assertSame(koin.get<CapsuleShellState>(), koin.get<CapsuleShellState>())
        assertSame(koin.get<LocalAuthGate>(), koin.get<LocalAuthGate>())
    }

    @Test
    fun the_app_root_renders_from_the_graph() {
        compose.setContent { CapsuleApp(onUpdateApp = {}) }
        compose.waitForIdle()

        // The root renders the timeline, which resolves its view model — and therefore
        // every repository beneath it — from the graph the application just started. That
        // it composed at all is the assertion; what it drew belongs to the feature's tests.
        assertNotNull(GlobalContext.getOrNull())
    }
}
