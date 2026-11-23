package com.justin13888.capsule.feature.settings

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.designsystem.component.CapsuleButton
import com.justin13888.capsule.core.designsystem.component.CapsuleButtonEmphasis
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleErrorState
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleLoadingState
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R

/** Test handle for the root list. */
const val SETTINGS_ROOT_TAG: String = "settings.root"

/** Test handle for a settings page: `settings.page:<section key>`. */
fun settingsPageTag(section: SettingsSection): String = "settings.page:${section.key}"

/** Test handle for a root-list row: `settings.section:<section key>`. */
fun settingsSectionTag(section: SettingsSection): String = "settings.section:${section.key}"

/** Test handle for the detail pane before a section is chosen. */
const val SETTINGS_DETAIL_EMPTY_TAG: String = "settings.detail.empty"

/** Test handle for the loading state. */
const val SETTINGS_LOADING_TAG: String = "settings.loading"

/** Test handle for the refused-read state. */
const val SETTINGS_ERROR_TAG: String = "settings.error"

/** Test handle for the refused-write banner. */
const val SETTINGS_WRITE_FAILURE_TAG: String = "settings.write-failure"

/**
 * The list of settings pages.
 *
 * Stateless, and rendered as a selectable list rather than a set of links: on a wide window
 * this is the list half of a two-pane layout, where exactly one row is the open page, and
 * announcing those rows as buttons would lose the one fact a screen-reader user needs.
 *
 * @param state everything the pages draw, so the list can show what is selected.
 * @param selected the section the detail pane is showing, or `null` on a narrow window.
 * @param onOpenSection opens a page.
 */
@Composable
fun SettingsRootScreen(
    state: SettingsUiState,
    selected: SettingsSection?,
    onOpenSection: (SettingsSection) -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().verticalScroll(rememberScrollState()).testTag(SETTINGS_ROOT_TAG)) {
        CapsuleTopAppBar(title = stringResource(R.string.nav_settings))
        if (state.isLoading) {
            CapsuleLoadingState(
                modifier = Modifier.fillMaxWidth().testTag(SETTINGS_LOADING_TAG),
                title = stringResource(R.string.android_settings_loading),
                contentDescription = stringResource(R.string.android_settings_loading),
            )
            return@Column
        }
        SettingsSection.entries.forEach { section ->
            CapsuleListItem(
                headline = stringResource(section.titleRes),
                modifier =
                    Modifier
                        .fillMaxWidth()
                        .testTag(settingsSectionTag(section))
                        .selectable(
                            selected = section == selected,
                            role = Role.Tab,
                            onClick = { onOpenSection(section) },
                        ),
                supporting = stringResource(section.summaryRes),
            )
        }
    }
}

/**
 * One settings page, whichever it is.
 *
 * A single composable over the section rather than one entry point per page, because the
 * detail pane is handed a content key and has to turn it into a page: five entry points would
 * mean the `when` existed anyway, one layer up, in a module that must not know these pages
 * apart.
 *
 * @param section the page to draw, or `null` before the user has chosen one.
 */
@Composable
fun SettingsSectionScreen(
    section: SettingsSection?,
    state: SettingsUiState,
    actions: SettingsActions,
    modifier: Modifier = Modifier,
) {
    if (section == null) {
        CapsuleEmptyState(
            title = stringResource(R.string.android_settings_detail_empty_title),
            modifier = modifier.fillMaxSize().testTag(SETTINGS_DETAIL_EMPTY_TAG),
            body = stringResource(R.string.android_settings_detail_empty_body),
        )
        return
    }
    Column(
        modifier =
            modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .testTag(settingsPageTag(section)),
    ) {
        CapsuleTopAppBar(title = stringResource(section.titleRes))
        when {
            state.isLoading ->
                CapsuleLoadingState(
                    modifier = Modifier.fillMaxWidth().testTag(SETTINGS_LOADING_TAG),
                    title = stringResource(R.string.android_settings_loading),
                    contentDescription = stringResource(R.string.android_settings_loading),
                )

            // A refused *read* leaves nothing to show, so it replaces the page. A refused
            // *write* leaves every other setting intact, so it sits above them as a banner
            // instead of throwing away a page the user can still use.
            state.failure?.isWrite == false -> ReadFailureState(state.failure, actions)

            else -> {
                if (state.failure != null) WriteFailureBanner(state.failure, actions)
                SettingsPageBody(section, state, actions)
            }
        }
    }
}

@Composable
private fun ColumnScope.SettingsPageBody(
    section: SettingsSection,
    state: SettingsUiState,
    actions: SettingsActions,
) {
    when (section) {
        SettingsSection.Appearance -> AppearanceSettings(state, actions)
        SettingsSection.Library -> LibrarySettingsPage(state, actions)
        SettingsSection.Sync -> SyncSettingsPage(state, actions)
        SettingsSection.Privacy -> PrivacySettingsPage(state, actions)
        SettingsSection.About -> AboutSettingsPage(state)
    }
}

@Composable
private fun ReadFailureState(
    failure: SettingsFailure,
    actions: SettingsActions,
) {
    CapsuleErrorState(
        title = stringResource(R.string.android_settings_error_title),
        modifier = Modifier.fillMaxWidth().testTag(SETTINGS_ERROR_TAG),
        body = localizedErrorMessage(failure.code),
        action = {
            CapsuleButton(
                onClick = actions.onReload,
                label = stringResource(R.string.android_settings_error_retry),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}

@Composable
private fun WriteFailureBanner(
    failure: SettingsFailure,
    actions: SettingsActions,
) {
    CapsuleBanner(
        title = stringResource(R.string.android_settings_error_title),
        modifier = Modifier.fillMaxWidth().padding(SettingsInset).testTag(SETTINGS_WRITE_FAILURE_TAG),
        // The localized `error.*` code, never a status: a status tells the user nothing they
        // can act on and two deployments may spell it differently for the same condition.
        body = localizedErrorMessage(failure.code),
        severity = CapsuleBannerSeverity.Error,
        action = {
            CapsuleButton(
                onClick = actions.onRetryFailedWrite,
                label = stringResource(R.string.android_settings_error_retry),
                emphasis = CapsuleButtonEmphasis.Secondary,
            )
        },
    )
}
