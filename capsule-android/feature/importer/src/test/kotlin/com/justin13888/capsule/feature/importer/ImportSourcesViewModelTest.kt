package com.justin13888.capsule.feature.importer

import app.cash.turbine.test
import com.justin13888.capsule.core.data.AlbumError
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.SourceKind
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * The source picker's state machine, and the one-time destination question inside it.
 *
 * "Ask once" is the property under test throughout. It is a rule about the *import*, not about
 * a dialog: a selection containing a source Capsule has never imported from must not scan, and
 * once answered the question must not come back — otherwise an automated import would end up
 * filing photos somewhere nobody chose.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ImportSourcesViewModelTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun installDispatcher() {
        Dispatchers.setMain(dispatcher)
    }

    @After
    fun removeDispatcher() {
        Dispatchers.resetMain()
    }

    @Test
    fun `sources are listed by their reinstall-stable relative path`() =
        runTest(dispatcher) {
            val viewModel = viewModel(ImporterFixture.repositories())

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val loaded = expectMostRecentItem()

                assertFalse(loaded.isLoading)
                assertTrue(loaded.sources.any { it.locator == "DCIM/Camera" })
                assertTrue(loaded.sources.any { it.locator == "Pictures/Screenshots" })
                assertTrue(loaded.sources.all { it.locator.isNotBlank() })
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a source the user points at by hand joins the list and is ticked`() =
        runTest(dispatcher) {
            val viewModel = viewModel(ImporterFixture.repositories())

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                viewModel.addSource("Pictures/Takeout", SourceKind.Folder)
                advanceUntilIdle()
                val added = expectMostRecentItem()

                assertTrue(added.sources.any { it.locator == "Pictures/Takeout" })
                assertTrue("Pictures/Takeout" in added.selected)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `an unmapped source stops the scan and raises the destination question instead`() =
        runTest(dispatcher) {
            val viewModel = viewModel(ImporterFixture.repositories())

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                viewModel.toggle("DCIM/Camera")
                viewModel.scan()
                advanceUntilIdle()
                val prompted = expectMostRecentItem()

                assertNotNull(prompted.prompt)
                assertEquals("DCIM/Camera", prompted.prompt?.source?.locator)
                assertNull(prompted.plannedImportId)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `answering the question is remembered, and the scan proceeds to a plan`() =
        runTest(dispatcher) {
            val repositories = ImporterFixture.repositories()
            val viewModel = viewModel(repositories)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                viewModel.toggle("DCIM/Camera")
                viewModel.scan()
                advanceUntilIdle()
                val album = requireNotNull(expectMostRecentItem().albums.firstOrNull()).id

                viewModel.choosePromptAlbum(album)
                viewModel.confirmPrompt()
                advanceUntilIdle()
                val planned = expectMostRecentItem()

                assertNull(planned.prompt)
                assertNotNull(planned.plannedImportId)
                assertEquals(album, planned.sources.first { it.locator == "DCIM/Camera" }.destination)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `the question is not asked a second time for the same source`() =
        runTest(dispatcher) {
            val repositories = ImporterFixture.repositories()
            val viewModel = viewModel(repositories)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                viewModel.toggle("DCIM/Camera")
                viewModel.scan()
                advanceUntilIdle()
                val album = requireNotNull(expectMostRecentItem().albums.firstOrNull()).id
                viewModel.choosePromptAlbum(album)
                viewModel.confirmPrompt()
                advanceUntilIdle()
                viewModel.clearPlan()
                advanceUntilIdle()

                viewModel.scan()
                advanceUntilIdle()
                val second = expectMostRecentItem()

                assertNull(second.prompt)
                assertNotNull(second.plannedImportId)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `dismissing the question leaves the scan unstarted rather than choosing for the user`() =
        runTest(dispatcher) {
            val viewModel = viewModel(ImporterFixture.repositories())

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                viewModel.toggle("DCIM/Camera")
                viewModel.scan()
                advanceUntilIdle()
                viewModel.dismissPrompt()
                advanceUntilIdle()
                val dismissed = expectMostRecentItem()

                assertNull(dismissed.prompt)
                assertNull(dismissed.plannedImportId)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `the plan is computed over every file the scan enumerated`() =
        runTest(dispatcher) {
            val repositories = ImporterFixture.repositories()
            val plans = InMemoryImportPlanStore()
            val viewModel = viewModel(repositories, plans = plans)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                viewModel.toggle("DCIM/Camera")
                viewModel.scan()
                advanceUntilIdle()
                val album = requireNotNull(expectMostRecentItem().albums.firstOrNull()).id
                viewModel.choosePromptAlbum(album)
                viewModel.confirmPrompt()
                advanceUntilIdle()
                val planned = expectMostRecentItem()

                assertNull(planned.scan)
                val recorded = plans.get(requireNotNull(planned.plannedImportId))
                assertEquals(FILES_PER_SOURCE, requireNotNull(recorded).sourceLocators.size)
                assertEquals(album, recorded.destination)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a refused write leaves the question unanswered and surfaces its code`() =
        runTest(dispatcher) {
            val repositories =
                ImporterFixture.repositories(
                    MockScenario.Default.failing(MockOperation.SettingsWrite, AlbumError.NotAvailable),
                )
            val viewModel = viewModel(repositories)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                viewModel.toggle("DCIM/Camera")
                viewModel.scan()
                advanceUntilIdle()
                val album = requireNotNull(expectMostRecentItem().albums.firstOrNull()).id
                viewModel.choosePromptAlbum(album)
                viewModel.confirmPrompt()
                advanceUntilIdle()
                val refused = expectMostRecentItem()

                assertEquals("error.album.not_available", refused.failure?.value)
                assertNotNull(refused.prompt)
                assertNull(refused.plannedImportId)
                cancelAndIgnoreRemainingEvents()
            }
        }

    private fun viewModel(
        repositories: MockRepositories,
        scanner: SourceScanner = ImporterFixture.scanner(FILES_PER_SOURCE),
        plans: ImportPlanStore = InMemoryImportPlanStore(),
    ) = ImportSourcesViewModel(
        imports = repositories.imports,
        albums = repositories.albums,
        scanner = scanner,
        scopeMappings = InMemoryScopeMappingStore(),
        plans = plans,
    )

    private companion object {
        /** Enough files that a plan has something to say, few enough to stay quick. */
        const val FILES_PER_SOURCE = 8
    }
}
