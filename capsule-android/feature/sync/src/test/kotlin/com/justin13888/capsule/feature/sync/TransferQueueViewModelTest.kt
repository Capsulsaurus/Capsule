package com.justin13888.capsule.feature.sync

import app.cash.turbine.test
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.SyncScope
import com.justin13888.capsule.core.model.UploadTier
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
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * The two transfer queues, derived from the seeded library rather than accumulated.
 *
 * The properties asserted here are the ones that make the screen trustworthy: all three
 * upload rungs are always present, the connection class decides which of them may open, and
 * the fetch queue reflects the library's own scope rather than an ambition to hold everything.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class TransferQueueViewModelTest {
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
    fun `the staged ladder always lists all three rungs`() =
        runTest(dispatcher) {
            val viewModel = viewModel(SyncFixture.repositories())

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val loaded = expectMostRecentItem()

                assertFalse(loaded.isLoading)
                assertEquals(UploadTier.entries, loaded.uploads.map(UploadTierGroup::tier))
                assertFalse(loaded.hasNoUploads)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `an offline device may open no rung at all`() =
        runTest(dispatcher) {
            val viewModel = viewModel(SyncFixture.repositories(MockScenario.Offline))

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val loaded = expectMostRecentItem()

                assertEquals(ConnectionClass.Offline, loaded.connection)
                assertTrue(loaded.uploads.none(UploadTierGroup::opensOnThisConnection))
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `an awaiting-original library owes originals and reads as degraded on the fetch side`() =
        runTest(dispatcher) {
            val repositories = SyncFixture.repositories(MockScenario.AwaitingOriginal)
            repositories.sync.setSyncScope(SyncScope.MetadataThumbnailsAndOriginal)
            val viewModel = viewModel(repositories)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val loaded = expectMostRecentItem()

                assertTrue(loaded.uploads.first { it.tier == UploadTier.Original }.pending > 0)
                assertTrue(loaded.hasDegraded)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `lowering the fetch scope empties the fetch queue without evicting anything`() =
        runTest(dispatcher) {
            val repositories = SyncFixture.repositories()
            repositories.sync.setSyncScope(SyncScope.MetadataOnly)
            val viewModel = viewModel(repositories)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val loaded = expectMostRecentItem()

                assertEquals(SyncScope.MetadataOnly, loaded.scope)
                assertTrue(loaded.hasNoFetches)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a fetch that cannot reach the original degrades rather than failing the screen`() =
        runTest(dispatcher) {
            val repositories = SyncFixture.repositories(MockScenario.GoneDegrade)
            repositories.sync.setSyncScope(SyncScope.MetadataThumbnailsAndOriginal)
            val viewModel = viewModel(repositories)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val before = expectMostRecentItem()
                val subject = before.fetches.first().id

                viewModel.fetchNow(subject)
                advanceUntilIdle()
                // A permanent failure degrades to the best held rung rather than throwing, so
                // nothing about the queue changes — the asset stays listed and re-fetchable.
                expectNoEvents()
                val after = viewModel.state.value

                assertFalse(after.hasFailed)
                assertEquals(null, after.failure)
                assertTrue(after.fetches.any { it.id == subject })
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a revoked album surfaces its code rather than a missing file`() =
        runTest(dispatcher) {
            val repositories = SyncFixture.repositories(MockScenario.MembershipResync)
            repositories.sync.setSyncScope(SyncScope.MetadataThumbnailsAndOriginal)
            val viewModel = viewModel(repositories)

            viewModel.state.test {
                awaitItem()
                advanceUntilIdle()
                val before = expectMostRecentItem()

                viewModel.fetchNow(before.fetches.first().id)
                advanceUntilIdle()

                assertEquals("error.album.not_available", expectMostRecentItem().failure?.value)
                cancelAndIgnoreRemainingEvents()
            }
        }

    private fun viewModel(repositories: MockRepositories) =
        TransferQueueViewModel(sync = repositories.sync, timeline = repositories.timeline)
}
