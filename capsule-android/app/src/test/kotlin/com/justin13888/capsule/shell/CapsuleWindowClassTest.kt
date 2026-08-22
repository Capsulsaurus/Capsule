package com.justin13888.capsule.shell

import androidx.compose.material3.adaptive.navigationsuite.NavigationSuiteType
import androidx.window.core.layout.WindowSizeClass
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

/**
 * Holds the width buckets to the breakpoints the layouts are specified against.
 *
 * These are exact boundaries, not approximate ones: 600dp and 840dp are where the navigation
 * component changes and where a second pane appears, so an off-by-one here is a device class
 * rendering the wrong shell. Testing the boundary on both sides is the only way that shows up.
 */
class CapsuleWindowClassTest {
    @Test
    fun `below 600dp is compact`() {
        assertEquals(CapsuleWindowClass.Compact, classAt(599))
    }

    @Test
    fun `600dp is the first medium width`() {
        assertEquals(CapsuleWindowClass.Medium, classAt(600))
    }

    @Test
    fun `839dp is still medium`() {
        assertEquals(CapsuleWindowClass.Medium, classAt(839))
    }

    @Test
    fun `840dp is the first expanded width`() {
        assertEquals(CapsuleWindowClass.Expanded, classAt(840))
    }

    @Test
    fun `extra large widths collapse into expanded`() {
        // Material splits these further; the shell deliberately does not, because nothing it
        // decides changes again above 840dp.
        assertEquals(CapsuleWindowClass.Expanded, classAt(1600))
    }

    @Test
    fun `compact renders the short navigation bar`() {
        assertEquals(
            NavigationSuiteType.ShortNavigationBarCompact,
            CapsuleWindowClass.Compact.navigationSuiteType(immersive = false),
        )
    }

    @Test
    fun `medium renders a collapsed wide rail`() {
        assertEquals(
            NavigationSuiteType.WideNavigationRailCollapsed,
            CapsuleWindowClass.Medium.navigationSuiteType(immersive = false),
        )
    }

    @Test
    fun `expanded renders an expanded wide rail`() {
        assertEquals(
            NavigationSuiteType.WideNavigationRailExpanded,
            CapsuleWindowClass.Expanded.navigationSuiteType(immersive = false),
        )
    }

    @Test
    fun `an immersive destination removes the navigation component at every width`() {
        CapsuleWindowClass.entries.forEach {
            assertEquals(
                NavigationSuiteType.None,
                it.navigationSuiteType(immersive = true),
                "$it kept its navigation component on an immersive destination",
            )
        }
    }

    private fun classAt(widthDp: Int) = CapsuleWindowClass.of(WindowSizeClass(widthDp, TEST_HEIGHT_DP))

    private companion object {
        /** Tall enough that height never influences the bucket under test. */
        const val TEST_HEIGHT_DP = 1024
    }
}
