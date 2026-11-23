package com.justin13888.capsule.feature.settings

import com.justin13888.capsule.core.designsystem.theme.CapsuleContrast
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.mock.seed.LibrarySpec
import com.justin13888.capsule.core.mock.seed.MockLibrary
import com.justin13888.capsule.core.mock.seed.SeededLibrary
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.ImportScope
import com.justin13888.capsule.core.model.PlatformTag
import com.justin13888.capsule.core.model.SourceKind
import com.justin13888.capsule.core.model.asWire
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import java.time.Clock
import java.time.Instant
import java.time.ZoneOffset

/**
 * A device-settings store that keeps everything in memory.
 *
 * The DataStore-backed implementation has its own cases; what the view model's cases need is
 * a store whose writes are observable without a file, a dispatcher and a temporary directory
 * in the way of the assertion.
 */
internal class InMemoryDeviceSettingsRepository(
    initial: DeviceSettings = DeviceSettings(),
) : DeviceSettingsRepository {
    private val state = MutableStateFlow(initial)

    override fun deviceSettings(): Flow<DeviceSettings> = state.asStateFlow()

    override suspend fun setThemeMode(mode: ThemeMode) {
        appearance { it.copy(themeMode = mode) }
    }

    override suspend fun setDynamicColor(enabled: Boolean) {
        appearance { it.copy(dynamicColor = enabled) }
    }

    override suspend fun setContrast(contrast: CapsuleContrast?) {
        appearance { it.copy(contrast = contrast) }
    }

    override suspend fun setGridAssetsAcross(assetsAcross: Int) {
        appearance { it.copy(gridAssetsAcross = assetsAcross) }
    }

    override suspend fun setReduceMotion(enabled: Boolean) {
        appearance { it.copy(reduceMotion = enabled) }
    }

    override suspend fun setLanguageTag(tag: String?) {
        mutate { it.copy(languageTag = tag) }
    }

    override suspend fun setDiagnosticsConsent(consented: Boolean) {
        mutate { it.copy(diagnosticsConsent = consented) }
    }

    private fun appearance(transform: (CapsuleAppearance) -> CapsuleAppearance) {
        mutate { it.copy(appearance = transform(it.appearance)) }
    }

    private fun mutate(transform: (DeviceSettings) -> DeviceSettings) {
        state.update(transform)
    }
}

/** A locale applier a case can interrogate, standing in for a platform API the JVM has not got. */
internal class RecordingLocaleApplier(
    override val isSupported: Boolean = true,
) : LocaleApplier {
    /** Every tag applied, in order. `null` means "follow the device". */
    val applied: MutableList<String?> = mutableListOf()

    override fun apply(languageTag: String?) {
        applied += languageTag
    }
}

/** The fixtures the settings suite is built from. */
internal object SettingsFixtures {
    private val SPEC = LibrarySpec.SMALL

    private val LIBRARY: MockLibrary by lazy { SeededLibrary.generate(SPEC) }

    /** The generated fixture, for a case that needs to reach the backend directly. */
    fun library(): MockLibrary = LIBRARY

    /** A clock fixed at the library's own "now", so relative dates line up with the fixture. */
    fun clock(): Clock = Clock.fixed(SPEC.now, ZoneOffset.UTC)

    /** The library's "now". */
    val NOW: Instant = SPEC.now

    /** The whole repository set over fresh mutable state under [scenario]. */
    fun repositories(scenario: MockScenario = MockScenario.Default): MockRepositories =
        MockRepositories.from(MockBackend(library(), clock(), scenario))

    /** A view model over the seeded fakes, with the device store and locale port supplied. */
    fun viewModel(
        mock: MockRepositories = repositories(),
        device: DeviceSettingsRepository = InMemoryDeviceSettingsRepository(),
        locales: LocaleApplier = RecordingLocaleApplier(),
        build: AppBuildInfo = BUILD,
    ): SettingsViewModel =
        SettingsViewModel(
            deviceSettings = device,
            librarySettings = mock.settings,
            sync = mock.sync,
            imports = mock.imports,
            albums = mock.albums,
            sessions = mock.sessions,
            locales = locales,
            build = build,
            clock = clock(),
        )

    /** A build the About page can render without a package manager. */
    val BUILD: AppBuildInfo = AppBuildInfo(versionName = "1.4.0", versionCode = 140)

    /** The Android camera roll, identified by its relative path and never by a bucket id. */
    val CAMERA_ROLL: ImportScope =
        ImportScope(
            platform = PlatformTag.Android.asWire(),
            sourceKind = SourceKind.CameraRoll.asWire(),
            locator = "DCIM/Camera",
        )

    /** The screenshots folder, likewise by relative path. */
    val SCREENSHOTS: ImportScope =
        ImportScope(
            platform = PlatformTag.Android.asWire(),
            sourceKind = SourceKind.Screenshots.asWire(),
            locator = "Pictures/Screenshots",
        )

    /** The nameless default album, as a destination the user can pick. */
    val DEFAULT_ALBUM: AlbumChoice =
        AlbumChoice(id = AlbumId("0197a0d0-0000-7000-8000-00000000a001"), name = "", isDefault = true)

    /** A named album beside it. */
    val TRIP_ALBUM: AlbumChoice =
        AlbumChoice(id = AlbumId("0197a0d0-0000-7000-8000-00000000a002"), name = "Iceland", isDefault = false)

    /** This device, as the signed directory carries it. */
    val THIS_DEVICE: Device =
        Device(
            id = DeviceId("11111111-1111-4111-8111-111111111111"),
            displayName = "Pixel 9 Pro",
            platform = PlatformTag.Android.asWire(),
            addedAt = NOW.minusSeconds(SECONDS_PER_DAY * 200),
            isThisDevice = true,
        )

    /** A settled state with two import sources, two albums, and a signed-in session. */
    fun state(
        scopes: List<ScopeMapping> =
            listOf(
                ScopeMapping(CAMERA_ROLL, DEFAULT_ALBUM),
                ScopeMapping(SCREENSHOTS, null),
            ),
        device: DeviceSettings = DeviceSettings(),
        editingScope: ImportScope? = null,
        isSignedIn: Boolean = true,
        failure: SettingsFailure? = null,
    ): SettingsUiState =
        SettingsUiState(
            isLoading = false,
            failure = failure,
            device = device,
            scopes = scopes,
            albums = listOf(DEFAULT_ALBUM, TRIP_ALBUM),
            editingScope = editingScope,
            isSignedIn = isSignedIn,
            thisDevice = THIS_DEVICE,
            build = BUILD,
        )

    private const val SECONDS_PER_DAY = 86_400L
}

/**
 * Actions that do nothing except the ones a case is asserting on.
 *
 * Only the callbacks the suite actually observes are parameters; the rest stay no-ops. A
 * nineteen-parameter builder would push every case into naming arguments it does not care
 * about, and a case that named the wrong one would still compile.
 */
internal fun recordingActions(
    onThemeMode: (ThemeMode) -> Unit = {},
    onScopeDestination: (ImportScope, AlbumId) -> Unit = { _, _ -> },
    onRetainIdentifiersOnExport: (Boolean) -> Unit = {},
    onRetryFailedWrite: () -> Unit = {},
    onReload: () -> Unit = {},
): SettingsActions =
    SettingsActions(
        onThemeMode = onThemeMode,
        onDynamicColor = {},
        onContrast = {},
        onGridAssetsAcross = {},
        onReduceMotion = {},
        onLanguage = {},
        onDefaultRetention = {},
        onGatedViewGrace = {},
        onEditScope = {},
        onScopeDestination = onScopeDestination,
        onAutoSync = {},
        onSyncScope = {},
        onUploadPolicy = {},
        onStaleSync = {},
        onRetainIdentifiersOnExport = onRetainIdentifiersOnExport,
        onDiagnosticsConsent = {},
        onRetryFailedWrite = onRetryFailedWrite,
        onDismissFailure = {},
        onReload = onReload,
    )
