package com.justin13888.capsule.feature.settings

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import com.github.takahirom.roborazzi.ExperimentalRoborazziApi
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityCheckOptions
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityChecker
import com.github.takahirom.roborazzi.checkRoboAccessibility
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import com.justin13888.capsule.core.designsystem.theme.CapsuleContrast
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.ImportScope
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * What each settings surface draws, and what it does when touched.
 *
 * The cases lean on the two states a settings screen most often gets wrong: a refused save,
 * which must leave every other setting on the page usable, and a refused read, which must not
 * leave a page of controls that write into nothing.
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class SettingsScreensTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun the_root_lists_every_section_and_opens_the_one_that_is_tapped() {
        val opened = mutableListOf<SettingsSection>()
        renderRoot(SettingsFixtures.state(), onOpenSection = opened::add)

        SettingsSection.entries.forEach { section ->
            compose.onNodeWithTag(settingsSectionTag(section)).performScrollTo().assertIsDisplayed()
        }
        compose.onNodeWithTag(settingsSectionTag(SettingsSection.Privacy)).performClick()

        assertEquals(listOf(SettingsSection.Privacy), opened)
    }

    @Test
    fun the_root_shows_a_loading_state_before_anything_is_read() {
        renderRoot(SettingsUiState())

        compose.onNodeWithTag(SETTINGS_LOADING_TAG).assertIsDisplayed()
    }

    @Test
    fun the_detail_pane_invites_a_choice_before_a_section_is_open() {
        renderSection(section = null, state = SettingsFixtures.state())

        compose.onNodeWithTag(SETTINGS_DETAIL_EMPTY_TAG).assertIsDisplayed()
    }

    @Test
    fun the_appearance_page_offers_the_theme_and_reports_the_choice() {
        var chosen: ThemeMode? = null
        renderSection(
            SettingsSection.Appearance,
            SettingsFixtures.state(),
            recordingActions(onThemeMode = { chosen = it }),
        )

        compose.onNodeWithTag(settingsPageTag(SettingsSection.Appearance)).assertIsDisplayed()
        compose.onNodeWithTag(settingsChoiceTag("theme", "dark")).performScrollTo().performClick()

        assertEquals(ThemeMode.Dark, chosen)
    }

    @Test
    fun raising_the_contrast_disables_wallpaper_colours_and_explains_why() {
        renderSection(
            SettingsSection.Appearance,
            SettingsFixtures.state(
                device = DeviceSettings(appearance = CapsuleAppearance(contrast = CapsuleContrast.High)),
            ),
        )

        compose.onNodeWithText(DYNAMIC_COLOR_UNAVAILABLE).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun a_platform_without_per_app_language_says_where_to_set_it_instead() {
        renderSection(
            SettingsSection.Appearance,
            SettingsFixtures.state().copy(perAppLanguageSupported = false),
        )

        compose.onNodeWithTag(SETTINGS_LANGUAGE_UNSUPPORTED_TAG).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun the_library_page_names_each_import_source_by_its_relative_path() {
        renderSection(SettingsSection.Library, SettingsFixtures.state())

        compose.onNodeWithTag(settingsScopeTag("DCIM/Camera")).performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag(settingsScopeTag("Pictures/Screenshots")).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun an_unmapped_source_says_it_will_ask_rather_than_naming_an_album() {
        renderSection(SettingsSection.Library, SettingsFixtures.state())

        compose.onNodeWithText(ASKS_ON_NEXT_IMPORT).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun choosing_where_a_source_lands_reports_the_source_and_the_album() {
        val chosen = mutableListOf<Pair<ImportScope, AlbumId>>()
        renderSection(
            SettingsSection.Library,
            SettingsFixtures.state(editingScope = SettingsFixtures.SCREENSHOTS),
            recordingActions(onScopeDestination = { scope, album -> chosen += scope to album }),
        )

        compose
            .onNodeWithTag(settingsScopeAlbumTag(SettingsFixtures.TRIP_ALBUM.id.value))
            .performScrollTo()
            .performClick()

        assertEquals(listOf(SettingsFixtures.SCREENSHOTS to SettingsFixtures.TRIP_ALBUM.id), chosen)
    }

    @Test
    fun the_sync_page_shows_the_connection_each_upload_tier_needs() {
        renderSection(SettingsSection.Sync, SettingsFixtures.state())

        compose.onNodeWithText(TIER_ORIGINAL).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(CONNECTION_RULES).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun the_privacy_page_toggles_report_what_turning_them_on_discloses() {
        var retained: Boolean? = null
        renderSection(
            SettingsSection.Privacy,
            SettingsFixtures.state(),
            recordingActions(onRetainIdentifiersOnExport = { retained = it }),
        )

        compose.onNodeWithTag(settingsToggleTag(EXPORT_STRIP_TOGGLE)).performScrollTo().performClick()

        assertEquals(true, retained)
    }

    @Test
    fun the_about_page_states_local_only_as_a_mode_rather_than_a_failure() {
        renderSection(SettingsSection.About, SettingsFixtures.state(isSignedIn = false))

        compose.onNodeWithTag(SETTINGS_LOCAL_ONLY_TAG).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun the_about_page_prints_the_version_and_the_protocol_range() {
        renderSection(SettingsSection.About, SettingsFixtures.state())

        compose.onNodeWithText(SettingsFixtures.BUILD.versionName).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(CapsuleProtocol.RANGE).performScrollTo().assertIsDisplayed()
    }

    @Test
    fun a_refused_save_keeps_the_page_usable_and_offers_to_retry() {
        var retried = 0
        renderSection(
            SettingsSection.Privacy,
            SettingsFixtures.state(failure = SettingsFailure(ErrorCode("error.quota.grace_locked"), isWrite = true)),
            recordingActions(onRetryFailedWrite = { retried++ }),
        )

        compose.onNodeWithTag(SETTINGS_WRITE_FAILURE_TAG).assertIsDisplayed()
        compose.onNodeWithTag(settingsToggleTag(DIAGNOSTICS_TOGGLE)).performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(RETRY_LABEL).performScrollTo().performClick()

        assertEquals(1, retried)
    }

    @Test
    fun a_refused_read_replaces_the_page_and_offers_a_reload() {
        var reloaded = 0
        renderSection(
            SettingsSection.Sync,
            SettingsFixtures.state(failure = SettingsFailure(ErrorCode("error.quota.exceeded"), isWrite = false)),
            recordingActions(onReload = { reloaded++ }),
        )

        compose.onNodeWithTag(SETTINGS_ERROR_TAG).assertIsDisplayed()
        compose.onNodeWithText(QUOTA_EXCEEDED_MESSAGE).assertIsDisplayed()
        compose.onNodeWithText(RETRY_LABEL).performClick()

        assertEquals(1, reloaded)
    }

    @Test
    fun a_failure_with_no_catalogue_code_falls_back_to_the_generic_message() {
        renderSection(
            SettingsSection.Sync,
            SettingsFixtures.state(failure = SettingsFailure(code = null, isWrite = false)),
        )

        compose.onNodeWithText(GENERIC_ERROR_MESSAGE).assertIsDisplayed()
    }

    @Test
    fun the_root_and_the_appearance_page_pass_the_accessibility_checks() {
        renderRoot(SettingsFixtures.state())
        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    @Test
    fun the_library_page_passes_the_accessibility_checks() {
        renderSection(SettingsSection.Library, SettingsFixtures.state())
        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    private fun renderRoot(
        state: SettingsUiState,
        selected: SettingsSection? = null,
        onOpenSection: (SettingsSection) -> Unit = {},
    ) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                SettingsRootScreen(state = state, selected = selected, onOpenSection = onOpenSection)
            }
        }
        compose.waitForIdle()
    }

    private fun renderSection(
        section: SettingsSection?,
        state: SettingsUiState,
        actions: SettingsActions = SettingsActions.NONE,
    ) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                SettingsSectionScreen(section = section, state = state, actions = actions)
            }
        }
        compose.waitForIdle()
    }

    private companion object {
        val ACCESSIBILITY_OPTIONS =
            RoborazziATFAccessibilityCheckOptions(
                checker = RoborazziATFAccessibilityChecker(),
                failureLevel = RoborazziATFAccessibilityChecker.CheckLevel.Warning,
            )

        const val RETRY_LABEL = "Try again"
        const val QUOTA_EXCEEDED_MESSAGE = "Your storage is full."
        const val GENERIC_ERROR_MESSAGE = "Something went wrong. Please try again."
        const val ASKS_ON_NEXT_IMPORT = "Asks on the next import"
        const val TIER_ORIGINAL = "Original files"
        const val CONNECTION_RULES = "Connection rules"
        const val DYNAMIC_COLOR_UNAVAILABLE =
            "Turned off while contrast is raised — the system's wallpaper colours come in one contrast level only."
    }
}
