package com.justin13888.capsule.feature.settings

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * The pieces of the About page that come from the platform rather than from a repository.
 *
 * Small, but worth pinning: the About page is where a user goes when they have to report a
 * problem, so every row on it has to render something rather than throw or come out blank.
 */
@RunWith(RobolectricTestRunner::class)
class PlatformPortsTest {
    @Test
    fun the_build_info_always_gives_the_about_page_something_printable() {
        val context = ApplicationProvider.getApplicationContext<Context>()

        val build = context.capsuleBuildInfo()

        // Robolectric's synthetic package declares no version name, so this case runs the
        // fallback path — which is the one that must not leave a blank row or throw.
        assertTrue("a version row must never render empty", build.versionName.isNotBlank())
        assertTrue(build.versionCode >= 0)
    }

    @Test
    fun the_protocol_range_collapses_when_this_build_speaks_one_version() {
        assertEquals(CapsuleProtocol.MINIMUM, CapsuleProtocol.RANGE)
    }

    @Test
    fun every_shipped_language_is_named_in_itself_and_listed_once() {
        val tags = CapsuleLanguage.ALL.map(CapsuleLanguage::tag)

        assertEquals(tags.distinct(), tags)
        assertTrue(
            "a language list rendered in the current language is useless to the one person who needs it",
            CapsuleLanguage.ALL.all { it.endonym.isNotBlank() },
        )
    }

    @Test
    fun every_licence_names_a_component_and_the_licence_it_is_offered_under() {
        assertTrue(CAPSULE_LICENCES.isNotEmpty())
        assertTrue(CAPSULE_LICENCES.all { it.component.isNotBlank() && it.licence.isNotBlank() })
    }
}
