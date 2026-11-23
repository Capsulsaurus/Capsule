package com.justin13888.capsule.feature.settings

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.designsystem.theme.CapsuleContrast
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.ImportScope
import com.justin13888.capsule.core.model.SyncScope
import com.justin13888.capsule.core.model.UploadPolicy
import java.time.Duration

/**
 * What the user can do on the stale-sync warning.
 *
 * A three-value choice rather than the model's `StaleSyncNotification` directly, because
 * snoozing needs a deadline and a stateless screen has no business reading a clock. The view
 * model turns the choice into the dated value.
 */
enum class StaleSyncChoice {
    /** Warn once the library has been behind for a fortnight. */
    Warn,

    /** Suppress the warning for a fortnight, then let it return. */
    Snooze,

    /** Opt out of the warning. Auto-sync is unaffected either way. */
    Never,
}

/**
 * Every callback the settings pages need, in one object.
 *
 * A holder rather than nineteen parameters per screen. The screens stay stateless — this is
 * still just functions in, no view model — but a page that grew a new control would otherwise
 * change its own signature, its parent's, and the entry that wires it, for one knob.
 *
 * The parameter count is over the project's limit on purpose: these *are* nineteen
 * independent decisions, and grouping them into sub-objects would only move the same list
 * somewhere harder to find.
 */
@Suppress("LongParameterList")
@Immutable
class SettingsActions(
    /** Sets light, dark or follow-the-device. */
    val onThemeMode: (ThemeMode) -> Unit,
    /** Allows or forbids Material You recolouring the chrome. */
    val onDynamicColor: (Boolean) -> Unit,
    /** Overrides the contrast level, or clears the override with `null`. */
    val onContrast: (CapsuleContrast?) -> Unit,
    /** Sets the rung the library grid opens at. */
    val onGridAssetsAcross: (Int) -> Unit,
    /** Turns the app's own reduce-motion request on or off. */
    val onReduceMotion: (Boolean) -> Unit,
    /** Sets the app language, or hands the choice back to the device with `null`. */
    val onLanguage: (String?) -> Unit,
    /** Sets the retention window signed into new deletes. */
    val onDefaultRetention: (Duration) -> Unit,
    /** Sets how long a confirmed identity check keeps a gated view open. */
    val onGatedViewGrace: (Duration) -> Unit,
    /** Opens or closes the album choice for one import source. */
    val onEditScope: (ImportScope?) -> Unit,
    /** Records where a source's imports land from now on. */
    val onScopeDestination: (ImportScope, AlbumId) -> Unit,
    /** Turns background reconciliation on or off. */
    val onAutoSync: (Boolean) -> Unit,
    /** Sets how much of each new asset this device fetches. */
    val onSyncScope: (SyncScope) -> Unit,
    /** Sets whether upload sessions open eagerly or in tier order. */
    val onUploadPolicy: (UploadPolicy) -> Unit,
    /** Sets what happens when the library falls behind. */
    val onStaleSync: (StaleSyncChoice) -> Unit,
    /** Sets whether the next export keeps the identifiers Capsule strips. */
    val onRetainIdentifiersOnExport: (Boolean) -> Unit,
    /** Records whether diagnostics may be sent. */
    val onDiagnosticsConsent: (Boolean) -> Unit,
    /** Retries the write that was refused. */
    val onRetryFailedWrite: () -> Unit,
    /** Clears the failure banner without retrying. */
    val onDismissFailure: () -> Unit,
    /** Re-reads every setting after a refused read. */
    val onReload: () -> Unit,
) {
    companion object {
        /**
         * Actions that do nothing, for a preview or for a case that asserts on rendering only.
         *
         * Worth having rather than writing nineteen empty lambdas at each call site, and worth
         * naming so a test that meant to assert on a callback cannot silently pass by using it.
         */
        val NONE: SettingsActions =
            SettingsActions(
                onThemeMode = {},
                onDynamicColor = {},
                onContrast = {},
                onGridAssetsAcross = {},
                onReduceMotion = {},
                onLanguage = {},
                onDefaultRetention = {},
                onGatedViewGrace = {},
                onEditScope = {},
                onScopeDestination = { _, _ -> },
                onAutoSync = {},
                onSyncScope = {},
                onUploadPolicy = {},
                onStaleSync = {},
                onRetainIdentifiersOnExport = {},
                onDiagnosticsConsent = {},
                onRetryFailedWrite = {},
                onDismissFailure = {},
                onReload = {},
            )
    }
}

/**
 * Wires a [SettingsViewModel] to the pages.
 *
 * The one place the stateless screens meet the state machine, so a screen never holds the
 * view model and a preview never needs one.
 */
fun SettingsViewModel.actions(): SettingsActions =
    SettingsActions(
        onThemeMode = ::setThemeMode,
        onDynamicColor = ::setDynamicColor,
        onContrast = ::setContrast,
        onGridAssetsAcross = ::setGridAssetsAcross,
        onReduceMotion = ::setReduceMotion,
        onLanguage = ::setLanguage,
        onDefaultRetention = ::setDefaultRetention,
        onGatedViewGrace = ::setGatedViewGrace,
        onEditScope = ::editScope,
        onScopeDestination = ::setScopeDestination,
        onAutoSync = ::setAutoSyncEnabled,
        onSyncScope = ::setSyncScope,
        onUploadPolicy = ::setUploadPolicy,
        onStaleSync = ::setStaleSyncChoice,
        onRetainIdentifiersOnExport = ::setRetainIdentifiersOnExport,
        onDiagnosticsConsent = ::setDiagnosticsConsent,
        onRetryFailedWrite = ::retryFailedWrite,
        onDismissFailure = ::dismissFailure,
        onReload = ::reload,
    )
