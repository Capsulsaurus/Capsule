package com.justin13888.capsule.feature.audit

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.ExperimentalRoborazziApi
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityCheckOptions
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityChecker
import com.github.takahirom.roborazzi.checkRoboAccessibility
import com.justin13888.capsule.core.designsystem.preview.CapsulePreviewSurface
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * What the record pane shows behind one entry.
 *
 * The assertions are about evidence surviving the trip to the screen: the chain link, the
 * signed retention deadline, and the fact that a create's missing prior hash is explained
 * rather than rendered as a blank.
 */
@OptIn(ExperimentalRoborazziApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class AuditRecordScreenTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun with_nothing_selected_it_invites_a_choice_instead_of_rendering_an_empty_form() {
        render(entry = null)

        compose.onNodeWithTag(AUDIT_RECORD_EMPTY_TAG).assertIsDisplayed()
    }

    @Test
    fun a_create_explains_its_missing_prior_link_rather_than_leaving_a_blank() {
        render(entry = AuditFixtures.CREATE)

        compose.onNodeWithTag(AUDIT_RECORD_TAG).assertIsDisplayed()
        compose.onNodeWithText(CHAIN_START).assertIsDisplayed()
        compose.onNodeWithText(AuditFixtures.CREATE.record.recordHash).assertIsDisplayed()
    }

    @Test
    fun a_delete_shows_the_chain_link_and_the_retention_deadline_signed_with_it() {
        render(entry = AuditFixtures.DELETE)

        compose.onNodeWithText(AuditFixtures.DELETE.record.recordHash).assertIsDisplayed()
        compose.onNodeWithText(AuditFixtures.CREATE.record.recordHash).assertIsDisplayed()
        compose.onNodeWithText(KEPT_UNTIL).assertIsDisplayed()
    }

    @Test
    fun a_record_signed_by_a_retired_device_still_names_it() {
        render(entry = AuditFixtures.DELETE)

        compose.onNodeWithText(RETIRED_SIGNER).assertIsDisplayed()
    }

    @Test
    fun a_moderation_entry_names_the_operator_and_the_stated_reason() {
        render(entry = AuditFixtures.TAKEDOWN)

        compose.onNodeWithText(OPERATOR).assertIsDisplayed()
        compose.onNodeWithText(TAKEDOWN_REASON).assertIsDisplayed()
    }

    @Test
    fun the_record_pane_passes_the_accessibility_checks_the_design_system_promises() {
        render(entry = AuditFixtures.DELETE)

        compose.onRoot().checkRoboAccessibility(ACCESSIBILITY_OPTIONS)
    }

    private fun render(entry: AuditEntry?) {
        compose.setContent {
            CapsulePreviewSurface(darkTheme = false) {
                AuditRecordScreen(entry = entry, devices = AuditFixtures.DIRECTORY)
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

        const val CHAIN_START = "The first record in this photo's chain"
        const val KEPT_UNTIL = "Kept until"
        const val RETIRED_SIGNER = "Old laptop · Retired device"
        const val OPERATOR = "Server operator"
        const val TAKEDOWN_REASON = "Reported by a recipient of a share link."
    }
}
