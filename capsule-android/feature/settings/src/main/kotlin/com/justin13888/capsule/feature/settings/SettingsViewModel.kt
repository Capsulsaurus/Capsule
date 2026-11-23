package com.justin13888.capsule.feature.settings

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AlbumRepository
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.ImportRepository
import com.justin13888.capsule.core.data.SessionRepository
import com.justin13888.capsule.core.data.SettingsRepository
import com.justin13888.capsule.core.data.SyncRepository
import com.justin13888.capsule.core.designsystem.theme.CapsuleContrast
import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.AlbumKind
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.DefaultAlbumRule
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.ImportScope
import com.justin13888.capsule.core.model.LibrarySettings
import com.justin13888.capsule.core.model.Session
import com.justin13888.capsule.core.model.StaleSyncNotification
import com.justin13888.capsule.core.model.StaleSyncPolicy
import com.justin13888.capsule.core.model.SyncScope
import com.justin13888.capsule.core.model.UploadPolicy
import com.justin13888.capsule.core.model.knownOrNull
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.io.IOException
import java.time.Clock
import java.time.Duration

/**
 * Every settings page's state machine.
 *
 * One view model for five pages rather than five, because on a wide window the root list and
 * an open page are composed at the same time and must agree: a per-page model would let the
 * list show a value the page had already changed.
 *
 * Reads are one combined flow so the pages never render a half-applied change; writes go
 * straight to the repository that owns each field and are *not* mirrored into local state.
 * The repository's own flow re-emits, and echoing the value optimistically would show a
 * setting as saved that the repository had just refused.
 *
 * The public surface is wide because the settings screen is wide. Folding twenty knobs behind
 * one `onChange(Setting, Any)` would trade the compiler's checking for a runtime cast, on the
 * screen where a wrong write does the most damage.
 */
@Suppress("TooManyFunctions", "LongParameterList")
class SettingsViewModel(
    private val deviceSettings: DeviceSettingsRepository,
    private val librarySettings: SettingsRepository,
    private val sync: SyncRepository,
    private val imports: ImportRepository,
    private val albums: AlbumRepository,
    private val sessions: SessionRepository,
    private val locales: LocaleApplier,
    private val build: AppBuildInfo,
    private val clock: Clock = Clock.systemUTC(),
) : ViewModel() {
    private val mutableState =
        MutableStateFlow(
            SettingsUiState(build = build, perAppLanguageSupported = locales.isSupported),
        )

    /** Everything the settings screens draw. */
    val state: StateFlow<SettingsUiState> = mutableState.asStateFlow()

    private var collection: Job? = null
    private var lastWrite: (suspend () -> Unit)? = null

    /**
     * Bumped after every successful write.
     *
     * Not every value on these pages is announced by a repository flow. The destination a
     * scope resolves to is *derived* — the resolver is asked, it is not observed — so a write
     * that changes only the resolution leaves every observed flow byte-identical and the page
     * showing the answer from before the write. One counter in the combine is the smallest
     * honest fix; the alternative is a resolver that pretends to be observable.
     */
    private val refreshes = MutableStateFlow(0L)

    init {
        reload()
    }

    /** Restarts the read. Also the remediation behind a refused *read*. */
    fun reload() {
        collection?.cancel()
        mutableState.update { it.copy(isLoading = true, failure = null) }
        collection =
            viewModelScope.launch {
                observations()
                    .catch { throwable -> mutableState.update { it.failed(throwable, isWrite = false) } }
                    .collect { snapshot -> mutableState.update { it.applied(snapshot) } }
            }
    }

    /** Retries the write that was refused. */
    fun retryFailedWrite() {
        val pending = lastWrite ?: return reload()
        write(pending)
    }

    /** Clears the failure banner without retrying. */
    fun dismissFailure() {
        mutableState.update { it.copy(failure = null) }
    }

    // ---- Appearance --------------------------------------------------------------------

    /** Sets light, dark, or follow-the-device. */
    fun setThemeMode(mode: ThemeMode) = write { deviceSettings.setThemeMode(mode) }

    /** Allows or forbids Material You recolouring the chrome. */
    fun setDynamicColor(enabled: Boolean) = write { deviceSettings.setDynamicColor(enabled) }

    /** Overrides the contrast level, or clears the override with `null`. */
    fun setContrast(contrast: CapsuleContrast?) = write { deviceSettings.setContrast(contrast) }

    /** Sets the rung the library grid opens at. */
    fun setGridAssetsAcross(assetsAcross: Int) = write { deviceSettings.setGridAssetsAcross(assetsAcross) }

    /** Turns the app's own reduce-motion request on or off. */
    fun setReduceMotion(enabled: Boolean) = write { deviceSettings.setReduceMotion(enabled) }

    /**
     * Sets the app's language, or hands the choice back to the device with `null`.
     *
     * Persisted first and applied second, and never the other way round: applying the locale
     * recreates the activity, so a failed write after a successful apply would leave the app
     * speaking a language it had not recorded.
     */
    fun setLanguage(tag: String?) =
        write {
            deviceSettings.setLanguageTag(tag)
            locales.apply(tag)
        }

    // ---- Library -----------------------------------------------------------------------

    /** Sets the retention window signed into new deletes. Never touches what is already in trash. */
    fun setDefaultRetention(window: Duration) = write { librarySettings.setDefaultRetention(window) }

    /** Sets how long a confirmed identity check keeps one gated view open. */
    fun setGatedViewGrace(window: Duration) = write { librarySettings.setGatedViewGrace(window) }

    /** Opens the album choice for one import source. */
    fun editScope(scope: ImportScope?) {
        mutableState.update { it.copy(editingScope = scope) }
    }

    /** Records where a source's imports land from now on, and closes the choice. */
    fun setScopeDestination(
        scope: ImportScope,
        albumId: AlbumId,
    ) = write {
        imports.setScopeDestination(scope, albumId)
        mutableState.update { it.copy(editingScope = null) }
    }

    // ---- Sync and data -----------------------------------------------------------------

    /** Turns background reconciliation on or off. Never deletes anything. */
    fun setAutoSyncEnabled(enabled: Boolean) = write { librarySettings.setAutoSyncEnabled(enabled) }

    /** Sets how much of each new asset this device fetches. */
    fun setSyncScope(scope: SyncScope) = write { sync.setSyncScope(scope) }

    /** Sets whether upload sessions open eagerly or in tier order. */
    fun setUploadPolicy(policy: UploadPolicy) = write { sync.setUploadPolicy(policy) }

    /**
     * Sets what happens when the library falls behind. Never affects auto-sync itself.
     *
     * The snooze deadline is stamped here rather than on the screen, because a stateless
     * screen has no business reading a clock and a snooze without a deadline is an opt-out
     * wearing a friendlier label.
     */
    fun setStaleSyncChoice(choice: StaleSyncChoice) =
        write {
            val mode =
                when (choice) {
                    StaleSyncChoice.Warn -> StaleSyncNotification.Enabled
                    StaleSyncChoice.Snooze ->
                        StaleSyncNotification.Snoozed(clock.instant().plus(StaleSyncPolicy.DEFAULT_SNOOZE))

                    StaleSyncChoice.Never -> StaleSyncNotification.Disabled
                }
            librarySettings.setStaleSyncNotification(mode)
        }

    // ---- Privacy -----------------------------------------------------------------------

    /** Sets whether the next export keeps the identifiers the privacy table strips. */
    fun setRetainIdentifiersOnExport(retain: Boolean) = write { librarySettings.setRetainIdentifiersOnExport(retain) }

    /** Records whether diagnostics may be sent. */
    fun setDiagnosticsConsent(consented: Boolean) = write { deviceSettings.setDiagnosticsConsent(consented) }

    // ---- Plumbing ----------------------------------------------------------------------

    /**
     * Runs one write, remembering it so a refusal has something to retry.
     *
     * Two specific catches rather than one broad one. A refused repository write and a
     * filesystem failure are both the user's problem and both surface here; an
     * `IllegalArgumentException` from a value outside a closed set is a bug in this file and
     * must not be dressed up as a condition the user can retry past.
     */
    private fun write(block: suspend () -> Unit) {
        lastWrite = block
        viewModelScope.launch {
            try {
                block()
                mutableState.update { it.copy(failure = null) }
                lastWrite = null
                refreshes.update { it + 1 }
            } catch (error: CapsuleError) {
                mutableState.update { it.failed(error, isWrite = true) }
            } catch (error: IOException) {
                mutableState.update { it.failed(error, isWrite = true) }
            }
        }
    }

    private fun observations(): Flow<SettingsSnapshot> =
        combine(
            deviceSettings.deviceSettings(),
            librarySettings.settings(),
            combine(sync.syncScope(), sync.uploadPolicy(), sync.connectionClass(), ::NetworkState),
            combine(imports.scopes(), albums.albums(), refreshes, ::SourceState),
            combine(sessions.currentSession(), sessions.devices(), ::AccountState),
        ) { device, library, network, sources, account ->
            SettingsSnapshot(
                device = device,
                library = library,
                network = network,
                albums = sources.albums.containers(),
                scopes = mappings(sources.scopes, sources.albums.containers()),
                account = account,
            )
        }

    /**
     * Resolves each source's destination through the same rule the import planner uses.
     *
     * Asking the resolver rather than reading a mapping table is what keeps this page honest:
     * the rule that fires here is the rule that will fire at import time, so a row that says
     * "asks on the next import" is a statement about behaviour rather than about storage.
     */
    private suspend fun mappings(
        scopes: List<ImportScope>,
        containers: List<AlbumChoice>,
    ): List<ScopeMapping> =
        scopes.map { scope ->
            val resolved = albums.resolveDefaultAlbum(scope)
            val mapped = resolved.rule == DefaultAlbumRule.ScopeOverride
            ScopeMapping(
                scope = scope,
                destination = if (mapped) containers.firstOrNull { it.id == resolved.albumId } else null,
            )
        }

    private fun List<Album>.containers(): List<AlbumChoice> =
        filter { it.kind.knownOrNull == AlbumKind.Container }
            .map { AlbumChoice(id = it.id, name = it.name, isDefault = it.isDefault) }

    private fun SettingsUiState.applied(snapshot: SettingsSnapshot): SettingsUiState =
        copy(
            isLoading = false,
            failure = null,
            device = snapshot.device,
            library = snapshot.library,
            syncScope = snapshot.network.syncScope,
            uploadPolicy = snapshot.network.uploadPolicy,
            connection = snapshot.network.connection,
            scopes = snapshot.scopes,
            albums = snapshot.albums,
            isSignedIn = snapshot.account.session != null,
            thisDevice = snapshot.account.devices.firstOrNull(Device::isThisDevice),
        )

    private fun SettingsUiState.failed(
        throwable: Throwable,
        isWrite: Boolean,
    ): SettingsUiState =
        copy(
            isLoading = false,
            failure = SettingsFailure.of(throwable, isWrite),
        )
}

/** What the network repositories say, gathered so the outer combine stays inside its arity. */
internal data class NetworkState(
    val syncScope: SyncScope,
    val uploadPolicy: UploadPolicy,
    val connection: ConnectionClass,
)

/** The import sources and the albums they can be pointed at. */
internal data class SourceState(
    val scopes: List<ImportScope>,
    val albums: List<Album>,
    /** The write counter, so a derived resolution is recomputed after a write changes it. */
    val refresh: Long,
)

/** Whether there is a session at all, and which device this is. */
internal data class AccountState(
    val session: Session?,
    val devices: List<Device>,
)

/** One consistent reading of every repository the settings screens draw from. */
internal data class SettingsSnapshot(
    val device: DeviceSettings,
    val library: LibrarySettings,
    val network: NetworkState,
    val albums: List<AlbumChoice>,
    val scopes: List<ScopeMapping>,
    val account: AccountState,
)
