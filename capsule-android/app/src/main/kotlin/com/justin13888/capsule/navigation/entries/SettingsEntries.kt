package com.justin13888.capsule.navigation.entries

import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.feature.settings.CapsuleAppearanceHost
import com.justin13888.capsule.feature.settings.SettingsRootScreen
import com.justin13888.capsule.feature.settings.SettingsSection
import com.justin13888.capsule.feature.settings.SettingsSectionScreen
import com.justin13888.capsule.feature.settings.SettingsViewModel
import com.justin13888.capsule.feature.settings.actions
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.destination
import com.justin13888.capsule.shell.panes.CapsuleListDetailPane
import org.koin.androidx.compose.koinViewModel

/**
 * Settings and its sub-pages.
 *
 * Sub-pages are real destinations rather than expanding sections so that a link from
 * elsewhere in the app — "change this in Sync settings" — has somewhere to point.
 *
 * The root is a list-detail scaffold: one pane on a phone, where tapping a row covers the
 * list and back returns to it, and two panes from 600dp up, where the chosen page sits beside
 * the list. The scaffold owns that decision and its own back behaviour, so the back stack
 * never learns that a pane opened.
 *
 * Every settings destination is wrapped in [CapsuleAppearanceHost], which rebuilds the theme
 * from the stored appearance preferences. That is what makes the appearance page's controls
 * do something visible the instant they are used. It is applied here rather than app-wide
 * because the app root is not this slice's to edit; hoisting the same wrapper above
 * `CapsuleAppShell` is a one-line change that themes everything.
 */
fun EntryProviderScope<NavKey>.settingsEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.Settings>(scope) { SettingsRoot() }
    destination<CapsuleDestination.SettingsAppearance>(scope) { SettingsPage(SettingsSection.Appearance) }
    destination<CapsuleDestination.SettingsLibrary>(scope) { SettingsPage(SettingsSection.Library) }
    destination<CapsuleDestination.SettingsSync>(scope) { SettingsPage(SettingsSection.Sync) }
    destination<CapsuleDestination.SettingsPrivacy>(scope) { SettingsPage(SettingsSection.Privacy) }
    destination<CapsuleDestination.SettingsAbout>(scope) { SettingsPage(SettingsSection.About) }
}

/**
 * The root: the section list, beside the open section where there is room for both.
 *
 * One view model for both panes rather than one each. They would resolve the same instance
 * from the same store owner anyway, but resolving it once here is what guarantees the list
 * and the page can never disagree about what was just saved.
 */
@Composable
private fun SettingsRoot() {
    val viewModel = koinViewModel<SettingsViewModel>()
    val state by viewModel.state.collectAsStateWithLifecycle()
    val actions = remember(viewModel) { viewModel.actions() }
    // Mirrored here rather than read back out of the scaffold: the scaffold knows which pane
    // is showing, not which row the list should mark as current, and on a phone — where no
    // detail pane is visible — those are different answers.
    var selected by remember { mutableStateOf<SettingsSection?>(null) }
    CapsuleAppearanceHost {
        CapsuleListDetailPane(
            listPane = { openDetail ->
                SettingsRootScreen(
                    state = state,
                    selected = selected,
                    onOpenSection = { section ->
                        selected = section
                        openDetail(section.key)
                    },
                )
            },
            detailPane = { contentKey ->
                SettingsSectionScreen(
                    section = SettingsSection.byKey(contentKey),
                    state = state,
                    actions = actions,
                )
            },
        )
    }
}

/** One settings page, reached directly rather than through the root's list. */
@Composable
private fun SettingsPage(section: SettingsSection) {
    val viewModel = koinViewModel<SettingsViewModel>()
    val state by viewModel.state.collectAsStateWithLifecycle()
    val actions = remember(viewModel) { viewModel.actions() }
    CapsuleAppearanceHost {
        SettingsSectionScreen(section = section, state = state, actions = actions)
    }
}
