package com.justin13888.capsule.shell

import androidx.compose.ui.test.DeviceConfigurationOverride
import androidx.compose.ui.test.WindowSize
import androidx.compose.ui.test.junit4.ComposeContentTestRule
import androidx.compose.ui.unit.DpSize
import androidx.compose.ui.unit.dp
import androidx.test.core.app.ApplicationProvider
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.model.GatedView
import com.justin13888.capsule.di.capsuleModules
import com.justin13888.capsule.navigation.CapsuleNavigator
import com.justin13888.capsule.navigation.rememberCapsuleNavigator
import com.justin13888.capsule.security.LocalAuthGate
import com.justin13888.capsule.security.LocalAuthPrompt
import org.junit.rules.TestRule
import org.junit.runner.Description
import org.junit.runners.model.Statement
import org.koin.android.ext.koin.androidContext
import org.koin.core.context.startKoin
import org.koin.core.context.stopKoin

/** A phone. Below the 600dp breakpoint, so the suite renders a short navigation bar. */
val COMPACT_WINDOW: DpSize = DpSize(411.dp, 891.dp)

/** An unfolded foldable. Between the breakpoints, so the suite renders a collapsed rail. */
val MEDIUM_WINDOW: DpSize = DpSize(700.dp, 900.dp)

/** A tablet. Above 840dp, so the suite renders an expanded rail. */
val EXPANDED_WINDOW: DpSize = DpSize(1000.dp, 800.dp)

/**
 * A local authenticator a test can make say yes or no.
 *
 * The whole reason [LocalAuthPrompt] is a port. There is no way to satisfy a real biometric
 * prompt on the JVM, so without this the gate would be the one piece of the shell that is
 * only ever checked by hand.
 */
class FakeLocalAuthPrompt(
    /** What the next authentication attempt returns. */
    var authenticates: Boolean = true,
) : LocalAuthPrompt {
    /** How many times the gate has raised the prompt. */
    var attempts: Int = 0
        private set

    /** The views the gate has prompted for, in order. */
    val promptedFor: MutableList<GatedView> = mutableListOf()

    override suspend fun authenticate(view: GatedView): Boolean {
        attempts++
        promptedFor += view
        return authenticates
    }
}

/** What a test holds onto after launching the shell. */
class ShellHandle(
    /** The live navigator, so a test can drive navigation without inventing UI affordances. */
    val navigator: CapsuleNavigator,
)

/**
 * Renders the whole shell at a chosen window size and hands back its navigator.
 *
 * The window is forced through `DeviceConfigurationOverride.WindowSize`, which is what the
 * adaptive library actually reads — it takes the container size from `LocalWindowInfo`, so a
 * Robolectric screen qualifier would not move it.
 */
fun ComposeContentTestRule.launchShell(
    shellState: CapsuleShellState,
    gate: LocalAuthGate,
    prompt: LocalAuthPrompt,
    windowSize: DpSize = COMPACT_WINDOW,
    foldPosture: FoldPosture = FoldPosture.Flat,
): ShellHandle {
    lateinit var navigator: CapsuleNavigator
    setContent {
        DeviceConfigurationOverride(DeviceConfigurationOverride.WindowSize(windowSize)) {
            // Dynamic colour is off so the test never depends on a wallpaper the JVM has not
            // got; dark mode is pinned so a golden or a contrast assertion stays stable.
            CapsuleTheme(darkTheme = false, dynamicColor = false) {
                navigator = rememberCapsuleNavigator()
                CapsuleAppShell(
                    shellState = shellState,
                    gate = gate,
                    prompt = prompt,
                    navigator = navigator,
                    foldPosture = foldPosture,
                )
            }
        }
    }
    waitForIdle()
    return ShellHandle(navigator)
}

/** Runs [block] on the composition thread and settles the resulting recomposition. */
fun ComposeContentTestRule.act(block: () -> Unit) {
    runOnIdle(block)
    waitForIdle()
}

/**
 * Starts and stops the real application graph around each test.
 *
 * The shell renders destinations, and a destination resolves its own view model from Koin,
 * so composing it needs a graph. These tests deliberately run on a plain `Application`
 * rather than `CapsuleApplication` — Koin's context is static and Robolectric builds a
 * fresh application per test in the same JVM, so the class's own application would start
 * the graph a second time and throw. Owning the lifecycle here instead keeps that
 * explicit, and using the *real* module list means a feature whose bindings are missing
 * fails these tests rather than only failing on a device.
 */
class CapsuleGraphRule : TestRule {
    override fun apply(
        base: Statement,
        description: Description,
    ): Statement =
        object : Statement() {
            override fun evaluate() {
                stopKoin()
                startKoin {
                    androidContext(ApplicationProvider.getApplicationContext())
                    modules(capsuleModules)
                }
                try {
                    base.evaluate()
                } finally {
                    stopKoin()
                }
            }
        }
}
