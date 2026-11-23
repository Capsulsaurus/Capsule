package com.justin13888.capsule.feature.settings

import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleDivider
import com.justin13888.capsule.core.i18n.R

/** Test handle id for the export-identifier toggle. */
internal const val EXPORT_STRIP_TOGGLE = "export-strip"

/** Test handle id for the diagnostics-consent toggle. */
internal const val DIAGNOSTICS_TOGGLE = "diagnostics"

/**
 * What leaves this device, and on whose say-so.
 *
 * Both rows default to off and both are phrased as what turning them *on* discloses, rather
 * than as what leaving them off protects. The export row in particular is a default for the
 * next export prompt and not a standing permission: sticky opt-in is exactly the foot-gun
 * where a user consents once and then leaks a camera serial with every share afterwards.
 */
@Composable
internal fun ColumnScope.PrivacySettingsPage(
    state: SettingsUiState,
    actions: SettingsActions,
) {
    SettingsToggleRow(
        id = EXPORT_STRIP_TOGGLE,
        title = stringResource(R.string.android_settings_privacy_export_strip),
        summary = stringResource(R.string.android_settings_privacy_export_strip_summary),
        checked = state.library.retainIdentifiersOnExport,
        onCheckedChange = actions.onRetainIdentifiersOnExport,
    )
    CapsuleDivider(modifier = Modifier.fillMaxWidth())
    SettingsToggleRow(
        id = DIAGNOSTICS_TOGGLE,
        title = stringResource(R.string.android_settings_privacy_diagnostics),
        summary = stringResource(R.string.android_settings_privacy_diagnostics_summary),
        checked = state.device.diagnosticsConsent,
        onCheckedChange = actions.onDiagnosticsConsent,
    )
}
