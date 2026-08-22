package com.justin13888.capsule.screenshot

import android.app.Application
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.unit.DpSize
import com.github.takahirom.roborazzi.captureRoboImage
import com.justin13888.capsule.security.LocalAuthGate
import com.justin13888.capsule.shell.COMPACT_WINDOW
import com.justin13888.capsule.shell.CapsuleGraphRule
import com.justin13888.capsule.shell.CapsuleShellState
import com.justin13888.capsule.shell.EXPANDED_WINDOW
import com.justin13888.capsule.shell.FakeLocalAuthPrompt
import com.justin13888.capsule.shell.FoldPosture
import com.justin13888.capsule.shell.MEDIUM_WINDOW
import com.justin13888.capsule.shell.launchShell
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Golden screenshots of the shell at each window size class, and in tabletop fold posture.
 *
 * The adaptive behaviour is the reason this app is described as running on three targets, so
 * it is the thing most worth having a picture of. Assertions already prove the *right*
 * navigation component is chosen at each breakpoint; these prove it is laid out sensibly,
 * which no assertion about component identity can.
 *
 * Rendered through Robolectric on the JVM. The emulator is reserved for proving the app
 * launches on a real Android runtime — it is not on the path of everyday review.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
// A plain Application: the manifest one starts Koin, and Robolectric rebuilds the
// application per test case, so the second case would fail on an already-started graph.
@Config(application = Application::class, qualifiers = "w1280dp-h1024dp-xhdpi")
class ShellScreenshotTest {
    @get:Rule(order = 0)
    val graph = CapsuleGraphRule()

    @get:Rule(order = 1)
    val compose = createComposeRule()

    private fun capture(
        name: String,
        windowSize: DpSize,
        foldPosture: FoldPosture = FoldPosture.Flat,
    ) {
        compose.launchShell(
            shellState = CapsuleShellState(),
            gate = LocalAuthGate(),
            prompt = FakeLocalAuthPrompt(),
            windowSize = windowSize,
            foldPosture = foldPosture,
        )
        compose.onRoot().captureRoboImage("src/test/screenshots/$name.png")
    }

    @Test
    fun shell_on_a_phone() {
        capture("shell-compact", COMPACT_WINDOW)
    }

    @Test
    fun shell_on_an_unfolded_foldable() {
        capture("shell-medium", MEDIUM_WINDOW)
    }

    @Test
    fun shell_on_a_tablet() {
        capture("shell-expanded", EXPANDED_WINDOW)
    }

    @Test
    fun shell_in_tabletop_posture() {
        // A horizontal hinge across the middle of the window, which is what a tabletop
        // fold actually presents to the app.
        capture("shell-tabletop", MEDIUM_WINDOW, FoldPosture.Tabletop(Rect(0f, 440f, 700f, 460f)))
    }
}
