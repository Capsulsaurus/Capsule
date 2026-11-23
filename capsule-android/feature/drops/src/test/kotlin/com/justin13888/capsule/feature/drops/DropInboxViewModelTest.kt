package com.justin13888.capsule.feature.drops

import com.justin13888.capsule.core.data.DropError
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.AlbumKind
import com.justin13888.capsule.core.model.knownOrNull
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
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

/**
 * The inbox, and the two futures a deposit has.
 *
 * The assertions worth reading are the ones about what adoption *does not* do: nothing
 * reaches the library until it is adopted, adoption mints an asset owned by the adopter, and
 * discard leaves nothing behind. A drop can never be observed half-adopted, because both
 * operations remove the inbox row in the same step as their real work.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class DropInboxViewModelTest {
    private val dispatcher = StandardTestDispatcher()

    @BeforeEach
    fun setUp() = Dispatchers.setMain(dispatcher)

    @AfterEach
    fun tearDown() = Dispatchers.resetMain()

    @Test
    fun `the inbox reads every deposit, newest first, with the link it came through`() =
        runTest(dispatcher) {
            val repositories = DropsFixtures.freshRepositories()
            val viewModel = DropInboxViewModel(repositories.drops, repositories.albums, repositories.sharing)
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(3, state.drops.size, "the fixture seeds three guest deposits")
            val received = state.drops.map { it.drop.receivedAt }
            assertEquals(received.sortedDescending(), received, "newest first")
            assertNotNull(state.drops.first().viaLinkOpaqueId, "the origin note names the link")
            assertTrue(state.canAdopt)
        }

    @Test
    fun `only container albums are offered as destinations`() =
        runTest(dispatcher) {
            val repositories = DropsFixtures.freshRepositories()
            val viewModel = DropInboxViewModel(repositories.drops, repositories.albums, repositories.sharing)
            advanceUntilIdle()

            val containers =
                repositories.albums
                    .albums()
                    .first()
                    .filter { it.kind.knownOrNull == AlbumKind.Container }
            assertEquals(containers.size, viewModel.state.value.albums.size)
            // A view album holds no key and owns no assets, so it can never receive a drop.
            assertTrue(
                containers.size <
                    repositories.albums
                        .albums()
                        .first()
                        .size,
            )
        }

    @Test
    fun `nothing is applied until the user adopts it`() =
        runTest(dispatcher) {
            val repositories = DropsFixtures.freshRepositories()
            val viewModel = DropInboxViewModel(repositories.drops, repositories.albums, repositories.sharing)
            advanceUntilIdle()

            val album =
                viewModel.state.value.albums
                    .first()
            val before = repositories.albums.albumAssets(album.id, 0, Int.MAX_VALUE).size
            // Merely opening the picker must not move anything.
            viewModel.beginAdoption(
                viewModel.state.value.drops
                    .first()
                    .drop.id,
            )
            advanceUntilIdle()
            assertEquals(before, repositories.albums.albumAssets(album.id, 0, Int.MAX_VALUE).size)
            assertEquals(
                3,
                repositories.drops
                    .inbox()
                    .first()
                    .size,
            )
        }

    @Test
    fun `adopting mints an asset in the chosen album and empties that inbox row`() =
        runTest(dispatcher) {
            val repositories = DropsFixtures.freshRepositories()
            val viewModel = DropInboxViewModel(repositories.drops, repositories.albums, repositories.sharing)
            advanceUntilIdle()

            val album =
                viewModel.state.value.albums
                    .first()
            val before = repositories.albums.albumAssets(album.id, 0, Int.MAX_VALUE).size
            val drop =
                viewModel.state.value.drops
                    .first()
                    .drop
            viewModel.adopt(drop.id, album.id)
            advanceUntilIdle()

            assertEquals(before + 1, repositories.albums.albumAssets(album.id, 0, Int.MAX_VALUE).size)
            assertEquals(2, viewModel.state.value.drops.size)
            assertNull(viewModel.state.value.busyDropId)
            assertNull(viewModel.state.value.failure)
        }

    @Test
    fun `discarding removes the deposit and adds nothing anywhere`() =
        runTest(dispatcher) {
            val repositories = DropsFixtures.freshRepositories()
            val viewModel = DropInboxViewModel(repositories.drops, repositories.albums, repositories.sharing)
            advanceUntilIdle()

            val album =
                viewModel.state.value.albums
                    .first()
            val before = repositories.albums.albumAssets(album.id, 0, Int.MAX_VALUE).size
            viewModel.discard(
                viewModel.state.value.drops
                    .first()
                    .drop.id,
            )
            advanceUntilIdle()

            assertEquals(2, viewModel.state.value.drops.size)
            assertEquals(before, repositories.albums.albumAssets(album.id, 0, Int.MAX_VALUE).size)
        }

    @Test
    fun `a refused adoption leaves the deposit in the inbox and names the reason`() =
        runTest(dispatcher) {
            val scenario = MockScenario.Default.failing(MockOperation.DropWrite, DropError.NotInInbox)
            val repositories = DropsFixtures.freshRepositories(scenario)
            val viewModel = DropInboxViewModel(repositories.drops, repositories.albums, repositories.sharing)
            advanceUntilIdle()

            val album =
                viewModel.state.value.albums
                    .first()
            viewModel.adopt(
                viewModel.state.value.drops
                    .first()
                    .drop.id,
                album.id,
            )
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(3, state.drops.size, "a refused adoption changes nothing")
            assertEquals("error.drop.not_in_inbox", state.failure?.code?.value)
            assertNull(state.busyDropId)
        }

    @Test
    fun `adopting with no network reports offline rather than a coded error`() =
        runTest(dispatcher) {
            val repositories = DropsFixtures.freshRepositories(MockScenario.Offline)
            val viewModel = DropInboxViewModel(repositories.drops, repositories.albums, repositories.sharing)
            advanceUntilIdle()

            val album =
                viewModel.state.value.albums
                    .first()
            viewModel.adopt(
                viewModel.state.value.drops
                    .first()
                    .drop.id,
                album.id,
            )
            advanceUntilIdle()

            assertEquals(
                DropsFailureKind.Offline,
                viewModel.state.value.failure
                    ?.kind,
            )
            assertEquals(3, viewModel.state.value.drops.size)
        }

    @Test
    fun `cancelling the picker adopts nothing`() =
        runTest(dispatcher) {
            val repositories = DropsFixtures.freshRepositories()
            val viewModel = DropInboxViewModel(repositories.drops, repositories.albums, repositories.sharing)
            advanceUntilIdle()

            viewModel.beginAdoption(
                viewModel.state.value.drops
                    .first()
                    .drop.id,
            )
            assertNotNull(viewModel.state.value.pendingAdoption)
            viewModel.cancelAdoption()
            advanceUntilIdle()

            assertNull(viewModel.state.value.pendingAdoption)
            assertEquals(3, viewModel.state.value.drops.size)
        }

    @Test
    fun `a first-run library shows an empty inbox rather than a failure`() =
        runTest(dispatcher) {
            val repositories = DropsFixtures.freshRepositories(MockScenario.FirstRun)
            val viewModel = DropInboxViewModel(repositories.drops, repositories.albums, repositories.sharing)
            advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(state.isEmpty)
            assertFalse(state.isLoading)
            assertNull(state.failure)
        }
}
