package com.justin13888.capsule.core.testing

import androidx.compose.material3.Text
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * Proves the test harness itself works before any feature depends on it: JUnit 4 running
 * under the Vintage engine on the JUnit Platform task, Robolectric supplying an Android
 * runtime, and Compose UI test driving a real composition — all on the JVM, no emulator.
 */
@RunWith(RobolectricTestRunner::class)
class HarnessProbeTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun robolectric_provides_an_android_runtime() {
        assertEquals("robolectric", android.os.Build.FINGERPRINT.substringBefore('-'))
    }

    @Test
    fun compose_renders_under_robolectric() {
        compose.setContent { Text("harness.probe") }
        compose.onNodeWithText("harness.probe").assertExists()
    }
}
