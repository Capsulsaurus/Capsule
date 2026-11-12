package com.justin13888.capsule.i18n

import android.content.res.Configuration
import android.view.View
import androidx.activity.ComponentActivity
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.getBoundsInRoot
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.unit.LayoutDirection
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.justin13888.capsule.R
import com.justin13888.capsule.screens.culling.CullingScreen
import com.justin13888.capsule.screens.culling.CullingViewModel
import com.justin13888.capsule.screens.culling.InMemoryCullPort
import com.justin13888.capsule.screens.mock.MockEnvironment
import com.justin13888.capsule.screens.transfer.TransferBoardScreen
import com.justin13888.capsule.screens.transfer.TransferBoardViewModel
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.util.Locale

/**
 * The main screens under Android's own development pseudo-locales, which the debug build
 * generates (`isPseudoLocalesEnabled`; i18n design doc, Layout → Pseudo-locales).
 *
 * - `en-XA` (accented, expanded, bracketed): every visible string must carry the brackets.
 *   One without them never went through `strings.xml` — a hardcoded string the
 *   `i18n-guard` pattern scan missed.
 * - `ar-XB` (right to left): the configuration resolves to a right-to-left layout, that
 *   direction reaches the root, every string comes from the right-to-left resources, and
 *   the back affordance sits at the screen's right edge, as a mirrored layout puts it.
 */
@RunWith(AndroidJUnit4::class)
class PseudoLocaleRenderTest {
    @get:Rule
    val compose = createAndroidComposeRule<ComponentActivity>()

    /** A main screen, composed against the mock world. */
    private enum class Screen {
        CULLING,
        TRANSFERS,
    }

    // A compose rule takes one `setContent` per test, so each screen is its own test.

    @Test
    fun cullingIsCataloguedUnderEnXa() = assertCatalogued(Screen.CULLING)

    @Test
    fun transfersAreCataloguedUnderEnXa() = assertCatalogued(Screen.TRANSFERS)

    @Test
    fun cullingMirrorsUnderArXb() = assertMirrored(Screen.CULLING)

    @Test
    fun transfersMirrorUnderArXb() = assertMirrored(Screen.TRANSFERS)

    /** The control for the mirror: left to right, back sits at the left edge. */
    @Test
    fun cullingBackLeadsUnderEnXa() = assertBackLeads(Screen.CULLING)

    @Test
    fun transfersBackLeadsUnderEnXa() = assertBackLeads(Screen.TRANSFERS)

    private fun assertCatalogued(screen: Screen) {
        show(EN_XA, screen)
        val text = visibleText()
        assertTrue("$screen renders no text", text.isNotEmpty())
        val unmarked = text.filter { !isData(it) && !(it.startsWith('[') && it.endsWith(']')) }
        assertEquals("$screen renders strings not in the catalog", emptyList<String>(), unmarked)
    }

    private fun assertMirrored(screen: Screen) {
        val direction = show(AR_XB, screen)
        assertEquals("$screen: ar-XB resolves to right to left", LayoutDirection.Rtl, direction)

        val unmarked = visibleText().filter { !isData(it) && RLO !in it }
        assertEquals("$screen renders strings outside the ar-XB resources", emptyList<String>(), unmarked)

        val root = compose.onRoot().getBoundsInRoot()
        val back = compose.onNodeWithContentDescription(backLabel(AR_XB)).getBoundsInRoot()
        assertTrue("$screen: the back affordance is not at the trailing (right) edge", back.left > (root.left + root.right) / 2)
    }

    private fun assertBackLeads(screen: Screen) {
        show(EN_XA, screen)
        val root = compose.onRoot().getBoundsInRoot()
        val back = compose.onNodeWithContentDescription(backLabel(EN_XA)).getBoundsInRoot()
        assertTrue("$screen: the back affordance is not at the leading (left) edge", back.right < (root.left + root.right) / 2)
    }

    /**
     * Compose [screen] under [tag], with the resources and the layout direction the system
     * would hand an activity configured for it, and return that direction.
     */
    private fun show(
        tag: String,
        screen: Screen,
    ): LayoutDirection {
        val configuration = localized(tag)
        val direction =
            if (configuration.layoutDirection == View.LAYOUT_DIRECTION_RTL) LayoutDirection.Rtl else LayoutDirection.Ltr
        val context = compose.activity.createConfigurationContext(configuration)
        compose.setContent {
            CompositionLocalProvider(
                LocalContext provides context,
                LocalConfiguration provides configuration,
                LocalLayoutDirection provides direction,
            ) {
                MaterialTheme { Content(screen) }
            }
        }
        compose.waitForIdle()
        return direction
    }

    @Composable
    private fun Content(screen: Screen) {
        when (screen) {
            Screen.CULLING -> {
                val viewModel = remember { CullingViewModel(InMemoryCullPort.sample()) }
                CullingScreen(viewModel, navigateBack = {})
            }
            Screen.TRANSFERS -> {
                val viewModel = remember { TransferBoardViewModel(MockEnvironment().transferEvents) { 0L } }
                TransferBoardScreen(viewModel, navigateBack = {})
            }
        }
    }

    private fun localized(tag: String): Configuration =
        Configuration(compose.activity.resources.configuration).apply { setLocale(Locale.forLanguageTag(tag)) }

    private fun backLabel(tag: String): String =
        compose.activity
            .createConfigurationContext(localized(tag))
            .getString(R.string.back)

    /** Every text and content description on screen, from the unmerged semantics tree. */
    private fun visibleText(): List<String> {
        val out = mutableListOf<String>()

        fun walk(node: SemanticsNode) {
            node.config.getOrNull(SemanticsProperties.Text)?.forEach { out += it.text }
            node.config.getOrNull(SemanticsProperties.ContentDescription)?.let { out += it }
            node.children.forEach(::walk)
        }
        walk(compose.onRoot(useUnmergedTree = true).fetchSemanticsNode())
        return out.map(String::trim).filter(String::isNotEmpty)
    }

    private companion object {
        const val EN_XA = "en-XA"
        const val AR_XB = "ar-XB"

        /** Right-to-left override: Android's `ar-XB` wraps every word in one. */
        const val RLO = '‮'

        /** A content digest prefix, as the transfer board shows a blob's. */
        val HEX = Regex("[0-9a-f]+")

        /**
         * A string with no letters (a count, a size, a position) or a hex digest is a
         * value, not an app string, and is the same in every locale.
         */
        fun isData(text: String): Boolean = text.none(Char::isLetter) || HEX.matches(text)
    }
}
