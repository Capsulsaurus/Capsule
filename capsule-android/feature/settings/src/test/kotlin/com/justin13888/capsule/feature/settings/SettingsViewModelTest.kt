package com.justin13888.capsule.feature.settings

import app.cash.turbine.test
import com.justin13888.capsule.core.data.QuotaError
import com.justin13888.capsule.core.designsystem.theme.CapsuleContrast
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.StaleSyncNotification
import com.justin13888.capsule.core.model.StaleSyncPolicy
import com.justin13888.capsule.core.model.SyncScope
import com.justin13888.capsule.core.model.UploadPolicy
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.BeforeEach
import org.junit.jupiter.api.Test
import java.time.Duration

/**
 * The state machine behind all five settings pages.
 *
 * The assertions deliberately read the value back out of the repository's own flow rather
 * than out of an echo in local state. That is the property that matters: a setting the screen
 * shows as saved must be a setting the repository actually holds, and an optimistic mirror
 * would make every one of these cases pass while shipping a screen that lies after a refusal.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class SettingsViewModelTest {
    private val dispatcher = StandardTestDispatcher()

    @BeforeEach
    fun installMainDispatcher() {
        Dispatchers.setMain(dispatcher)
    }

    @AfterEach
    fun removeMainDispatcher() {
        Dispatchers.resetMain()
    }

    @Test
    fun `it opens loading and settles on every repository's current value`() =
        runTest(dispatcher) {
            val viewModel = SettingsFixtures.viewModel()

            viewModel.state.test {
                assertTrue(awaitItem().isLoading)
                val settled = awaitItem()
                assertFalse(settled.isLoading)
                assertNull(settled.failure)
                assertTrue(settled.scopes.isNotEmpty(), "the seeded device has import sources")
                assertTrue(settled.albums.isNotEmpty(), "an import needs somewhere to land")
                assertNotNull(settled.thisDevice)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `an appearance change is persisted through the device store, not held in the screen`() =
        runTest(dispatcher) {
            val store = InMemoryDeviceSettingsRepository()
            val viewModel = settled(device = store)

            viewModel.setThemeMode(ThemeMode.Dark)
            viewModel.setContrast(CapsuleContrast.High)
            viewModel.setReduceMotion(true)
            viewModel.setGridAssetsAcross(CapsuleAppearance.GRID_CHOICES.first())
            dispatcher.scheduler.advanceUntilIdle()

            val stored = store.deviceSettings().first()
            assertEquals(ThemeMode.Dark, stored.appearance.themeMode)
            assertEquals(CapsuleContrast.High, stored.appearance.contrast)
            assertTrue(stored.appearance.reduceMotion)
            assertEquals(CapsuleAppearance.GRID_CHOICES.first(), stored.appearance.gridAssetsAcross)
            assertEquals(stored, viewModel.state.value.device, "the screen shows what the store holds")
        }

    @Test
    fun `choosing a language records it first and only then applies it`() =
        runTest(dispatcher) {
            val store = InMemoryDeviceSettingsRepository()
            val locales = RecordingLocaleApplier()
            val viewModel = settled(device = store, locales = locales)

            viewModel.setLanguage("ja")
            dispatcher.scheduler.advanceUntilIdle()

            assertEquals("ja", viewModel.state.value.device.languageTag)
            assertEquals(listOf<String?>("ja"), locales.applied)
        }

    @Test
    fun `a platform with no per-app language setting says so in the state`() =
        runTest(dispatcher) {
            val viewModel = settled(locales = RecordingLocaleApplier(isSupported = false))

            assertFalse(viewModel.state.value.perAppLanguageSupported)
        }

    @Test
    fun `library and sync settings go to the repositories that own them`() =
        runTest(dispatcher) {
            val mock = SettingsFixtures.repositories()
            val viewModel = settled(mock = mock)

            viewModel.setAutoSyncEnabled(false)
            viewModel.setDefaultRetention(Duration.ofDays(SEVEN_DAYS))
            viewModel.setSyncScope(SyncScope.MetadataOnly)
            viewModel.setUploadPolicy(UploadPolicy.Staged)
            viewModel.setRetainIdentifiersOnExport(true)
            dispatcher.scheduler.advanceUntilIdle()

            val library = mock.settings.settings().first()
            assertFalse(library.autoSyncEnabled)
            assertEquals(Duration.ofDays(SEVEN_DAYS), library.defaultRetention)
            assertTrue(library.retainIdentifiersOnExport)
            assertEquals(SyncScope.MetadataOnly, mock.sync.syncScope().first())
            assertEquals(UploadPolicy.Staged, mock.sync.uploadPolicy().first())
        }

    @Test
    fun `snoozing the stale-library warning stamps a deadline and leaves auto-sync alone`() =
        runTest(dispatcher) {
            val mock = SettingsFixtures.repositories()
            val viewModel = settled(mock = mock)
            val autoSyncBefore = viewModel.state.value.library.autoSyncEnabled

            viewModel.setStaleSyncChoice(StaleSyncChoice.Snooze)
            dispatcher.scheduler.advanceUntilIdle()

            val library = mock.settings.settings().first()
            val snoozed = library.staleSyncNotification
            assertTrue(snoozed is StaleSyncNotification.Snoozed, "a snooze without a deadline is an opt-out")
            assertEquals(
                SettingsFixtures.NOW.plus(StaleSyncPolicy.DEFAULT_SNOOZE),
                (snoozed as StaleSyncNotification.Snoozed).until,
            )
            assertEquals(autoSyncBefore, library.autoSyncEnabled, "silencing a warning is not stopping the backup")
        }

    @Test
    fun `opting out of the warning is distinct from snoozing it`() =
        runTest(dispatcher) {
            val mock = SettingsFixtures.repositories()
            val viewModel = settled(mock = mock)

            viewModel.setStaleSyncChoice(StaleSyncChoice.Never)
            dispatcher.scheduler.advanceUntilIdle()

            assertEquals(
                StaleSyncNotification.Disabled,
                mock.settings
                    .settings()
                    .first()
                    .staleSyncNotification,
            )
        }

    @Test
    fun `pointing an import source at an album makes the scope-override rule fire`() =
        runTest(dispatcher) {
            val mock = SettingsFixtures.repositories()
            val viewModel = settled(mock = mock)
            val source =
                viewModel.state.value.scopes
                    .first()
            val target =
                viewModel.state.value.albums
                    .first()

            viewModel.setScopeDestination(source.scope, target.id)
            dispatcher.scheduler.advanceUntilIdle()

            val mapping = viewModel.state.value.mappingFor(source.scope)
            assertEquals(target.id, mapping?.destination?.id, "the row must show where imports now land")
            assertNull(viewModel.state.value.editingScope, "a completed choice closes itself")
        }

    @Test
    fun `every import source is identified by its relative path, never by a bucket id`() =
        runTest(dispatcher) {
            val viewModel = settled()

            val locators =
                viewModel.state.value.scopes
                    .map { it.scope.locator }
            assertTrue(locators.isNotEmpty())
            assertTrue(
                locators.all { it.contains('/') && it.toLongOrNull() == null },
                "an Android scope locator is a relative path like DCIM/Camera",
            )
        }

    @Test
    fun `a source nobody has answered for says it will ask rather than naming today's default`() =
        runTest(dispatcher) {
            val viewModel = settled()

            assertTrue(
                viewModel.state.value.scopes
                    .any { it.destination == null },
                "the seeded sources include one that has never been mapped",
            )
        }

    @Test
    fun `opening and closing a source's album choice is state, not navigation`() =
        runTest(dispatcher) {
            val viewModel = settled()
            val source =
                viewModel.state.value.scopes
                    .first()
                    .scope

            viewModel.editScope(source)
            assertEquals(source, viewModel.state.value.editingScope)

            viewModel.editScope(null)
            assertNull(viewModel.state.value.editingScope)
        }

    @Test
    fun `a refused write surfaces its code as a write failure, and retrying recovers`() =
        runTest(dispatcher) {
            val backend = MockBackend(SettingsFixtures.library(), SettingsFixtures.clock(), MockScenario.Default)
            val mock = MockRepositories.from(backend)
            val viewModel = settled(mock = mock)
            backend.applyScenario(MockScenario.Default.failing(MockOperation.SettingsWrite, QuotaError.GraceLocked))

            viewModel.setAutoSyncEnabled(false)
            dispatcher.scheduler.advanceUntilIdle()

            val failure = viewModel.state.value.failure
            assertEquals(QuotaError.GraceLocked.code, failure?.code)
            assertTrue(failure?.isWrite == true, "a refused save must not blank the page")

            backend.applyScenario(MockScenario.Default)
            viewModel.retryFailedWrite()
            dispatcher.scheduler.advanceUntilIdle()

            assertNull(viewModel.state.value.failure)
            assertFalse(
                mock.settings
                    .settings()
                    .first()
                    .autoSyncEnabled,
            )
        }

    @Test
    fun `dismissing a refused write clears the banner without retrying it`() =
        runTest(dispatcher) {
            val backend = MockBackend(SettingsFixtures.library(), SettingsFixtures.clock(), MockScenario.Default)
            val mock = MockRepositories.from(backend)
            val viewModel = settled(mock = mock)
            backend.applyScenario(MockScenario.Default.failing(MockOperation.SettingsWrite, QuotaError.GraceLocked))
            viewModel.setAutoSyncEnabled(false)
            dispatcher.scheduler.advanceUntilIdle()

            viewModel.dismissFailure()

            assertNull(viewModel.state.value.failure)
            assertTrue(
                mock.settings
                    .settings()
                    .first()
                    .autoSyncEnabled,
                "dismissing is not saving",
            )
        }

    @Test
    fun `a refused read is a read failure, so the page offers a reload rather than a retry`() =
        runTest(dispatcher) {
            val backend = MockBackend(SettingsFixtures.library(), SettingsFixtures.clock(), MockScenario.Default)
            backend.applyScenario(MockScenario.Default.failing(MockOperation.AlbumRead, QuotaError.Exceeded))
            val viewModel = SettingsFixtures.viewModel(mock = MockRepositories.from(backend))
            dispatcher.scheduler.advanceUntilIdle()

            val failure = viewModel.state.value.failure
            assertEquals(QuotaError.Exceeded.code, failure?.code)
            assertFalse(failure?.isWrite == true)

            backend.applyScenario(MockScenario.Default)
            viewModel.reload()
            dispatcher.scheduler.advanceUntilIdle()

            assertNull(viewModel.state.value.failure)
            assertTrue(viewModel.state.value.isReady)
        }

    @Test
    fun `a library with no server reports local-only rather than an error`() =
        runTest(dispatcher) {
            val viewModel = settled(mock = SettingsFixtures.repositories(MockScenario.NeverSignedIn))

            assertFalse(viewModel.state.value.isSignedIn)
            assertNull(viewModel.state.value.failure, "never having signed in is a mode, not a failure")
        }

    private fun settled(
        mock: MockRepositories = SettingsFixtures.repositories(),
        device: DeviceSettingsRepository = InMemoryDeviceSettingsRepository(),
        locales: LocaleApplier = RecordingLocaleApplier(),
    ): SettingsViewModel =
        SettingsFixtures.viewModel(mock = mock, device = device, locales = locales).also {
            dispatcher.scheduler.advanceUntilIdle()
        }

    private companion object {
        const val SEVEN_DAYS = 7L
    }
}
