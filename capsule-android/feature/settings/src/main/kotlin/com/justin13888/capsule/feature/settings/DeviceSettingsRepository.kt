package com.justin13888.capsule.feature.settings

import com.justin13888.capsule.core.designsystem.theme.CapsuleContrast
import kotlinx.coroutines.flow.Flow

/**
 * The device-scoped preferences, read as one record and written one field at a time.
 *
 * Deliberately the same shape as `SettingsRepository` in `core:data`, and for the same
 * reasons: one flow, because the settings screen shows them together and a flow per knob
 * would have it recomposing once per knob for a single change; per-field writes, because a
 * whole-record write is a lost update waiting to happen the moment two panes are open at
 * once — which, on a tablet, is the normal case rather than the exceptional one.
 *
 * It lives in this feature rather than in `core:data` because nothing else reads it. The one
 * exception is the theme, and that is handed out as a composable
 * ([CapsuleAppearanceHost]) rather than as a dependency, so no other module has to know this
 * interface exists.
 */
interface DeviceSettingsRepository {
    /** Every device preference, re-emitted whenever one of them changes. */
    fun deviceSettings(): Flow<DeviceSettings>

    /** Sets the colour scheme, or hands the decision back to the device. */
    suspend fun setThemeMode(mode: ThemeMode)

    /** Allows or forbids Material You recolouring the chrome from the wallpaper. */
    suspend fun setDynamicColor(enabled: Boolean)

    /**
     * Overrides the contrast level, or clears the override.
     *
     * `null` restores the platform's own reading rather than pinning Standard: below API 34
     * Android reports no preference at all, and recording Standard there would turn "the
     * system has no opinion" into "the user chose Standard".
     */
    suspend fun setContrast(contrast: CapsuleContrast?)

    /** Sets the rung the library grid opens at, in photos across a row. */
    suspend fun setGridAssetsAcross(assetsAcross: Int)

    /** Turns the app's own reduce-motion request on or off. Never overrides the system's. */
    suspend fun setReduceMotion(enabled: Boolean)

    /** Sets the app language by BCP 47 tag, or `null` to follow the device. */
    suspend fun setLanguageTag(tag: String?)

    /** Records whether diagnostics may be sent. */
    suspend fun setDiagnosticsConsent(consented: Boolean)
}
