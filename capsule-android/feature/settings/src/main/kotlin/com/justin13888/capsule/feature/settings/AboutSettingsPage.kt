package com.justin13888.capsule.feature.settings

import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleDivider
import com.justin13888.capsule.core.designsystem.component.CapsuleListItem
import com.justin13888.capsule.core.designsystem.component.CapsuleSectionHeader
import com.justin13888.capsule.core.i18n.R

/** Test handle for the local-only row, which is a mode rather than a failure. */
const val SETTINGS_LOCAL_ONLY_TAG: String = "settings.about.local-only"

/**
 * What this build is, what it is talking to, and what it is built from.
 *
 * "This device only" is stated as a mode, not as a problem: the Local Gallery contract makes
 * never having signed in a valid way to use Capsule, and an About page that presented it as a
 * missing connection would contradict the product.
 *
 * The protocol range is here because it is the one number that explains a refusal. A server
 * that has moved past it stops accepting writes from this build, and a user reading an
 * "update to continue" screen has somewhere to check what this build actually speaks.
 */
@Composable
internal fun ColumnScope.AboutSettingsPage(state: SettingsUiState) {
    SettingsValueRow(
        label = stringResource(R.string.android_settings_about_version),
        value = state.build.versionName,
        supporting = state.build.versionCode.toString(),
    )
    SettingsValueRow(
        label = stringResource(R.string.android_settings_about_protocol),
        value = CapsuleProtocol.RANGE,
    )
    CapsuleDivider(modifier = Modifier.fillMaxWidth())
    CapsuleSectionHeader(title = stringResource(R.string.android_settings_about_server_section))
    if (state.isSignedIn) {
        CapsuleListItem(
            headline = stringResource(R.string.android_settings_about_signed_in),
            modifier = Modifier.fillMaxWidth(),
            supporting = stringResource(R.string.android_settings_about_signed_in_summary),
        )
    } else {
        CapsuleListItem(
            headline = stringResource(R.string.android_settings_about_local_only),
            modifier = Modifier.fillMaxWidth().testTag(SETTINGS_LOCAL_ONLY_TAG),
            supporting = stringResource(R.string.android_settings_about_local_only_summary),
        )
    }
    state.thisDevice?.let { device ->
        SettingsValueRow(
            label = stringResource(R.string.android_settings_about_this_device),
            // A device's own label is user-chosen data; its platform token is not translated
            // either, so the row falls back to the raw token rather than inventing copy.
            value = device.displayName ?: device.platform.raw,
        )
        SettingsValueRow(
            label = stringResource(R.string.android_settings_about_device_added),
            value = formattedDate(device.addedAt),
        )
    }
    CapsuleDivider(modifier = Modifier.fillMaxWidth())
    CapsuleSectionHeader(title = stringResource(R.string.android_settings_about_licences))
    SettingsNote(stringResource(R.string.android_settings_about_licences_summary))
    CAPSULE_LICENCES.forEach { licence ->
        CapsuleListItem(
            headline = licence.component,
            modifier = Modifier.fillMaxWidth(),
            supporting = licence.licence,
            contentDescription = licence.component + SEPARATOR + licence.licence,
        )
    }
}
