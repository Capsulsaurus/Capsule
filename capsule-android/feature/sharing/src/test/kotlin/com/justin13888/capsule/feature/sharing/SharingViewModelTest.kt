package com.justin13888.capsule.feature.sharing

import app.cash.turbine.test
import com.justin13888.capsule.core.mock.MockScenario
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.BeforeEach
import org.junit.jupiter.api.Test

/**
 * The hub's state machine.
 *
 * The one property worth pinning here is that the three counts come from a single reading:
 * a hub that showed yesterday's share links beside today's inbox would be read as two
 * separate bugs, and the combine is the only thing preventing it.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class SharingViewModelTest {
    private val dispatcher = StandardTestDispatcher()

    @BeforeEach
    fun setUp() = Dispatchers.setMain(dispatcher)

    @AfterEach
    fun tearDown() = Dispatchers.resetMain()

    @Test
    fun `the first reading replaces loading with every count at once`() =
        runTest(dispatcher) {
            val repositories = SharingFixtures.freshRepositories()
            val viewModel = SharingViewModel(repositories.sharing, repositories.drops, repositories.albums)

            viewModel.state.test {
                assertTrue(awaitItem().isLoading, "the hub starts loading")
                val loaded = awaitItem()
                assertFalse(loaded.isLoading)
                assertNull(loaded.failure)
                // The fixture issues one live read link, one upload link and three deposits.
                assertEquals(1, loaded.shareLinks.size)
                assertEquals(1, loaded.uploadLinkCount)
                assertEquals(3, loaded.pendingDropCount)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `an expired link is absent rather than listed as expired`() =
        runTest(dispatcher) {
            // The fixture also seeds a link whose expiry has passed. A visitor cannot tell an
            // expired link from one that never existed, so neither can this list.
            val repositories = SharingFixtures.freshRepositories()
            val viewModel = SharingViewModel(repositories.sharing, repositories.drops, repositories.albums)

            viewModel.state.test {
                awaitItem()
                val loaded = awaitItem()
                assertTrue(
                    loaded.shareLinks.all { it.link.isLiveAt(SharingFixtures.CLOCK.instant()) },
                    "only live links are listed",
                )
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a first-run library reports an empty hub rather than an error`() =
        runTest(dispatcher) {
            val repositories = SharingFixtures.freshRepositories(MockScenario.FirstRun)
            val viewModel = SharingViewModel(repositories.sharing, repositories.drops, repositories.albums)

            viewModel.state.test {
                awaitItem()
                val loaded = awaitItem()
                assertNull(loaded.failure, "having nothing yet is a mode, not a failure")
                assertEquals(0, loaded.pendingDropCount)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `reload restarts the reading`() =
        runTest(dispatcher) {
            val repositories = SharingFixtures.freshRepositories()
            val viewModel = SharingViewModel(repositories.sharing, repositories.drops, repositories.albums)

            viewModel.state.test {
                awaitItem()
                awaitItem()
                viewModel.reload()
                assertTrue(awaitItem().isLoading, "retry visibly starts over")
                assertFalse(awaitItem().isLoading)
                cancelAndIgnoreRemainingEvents()
            }
        }
}
