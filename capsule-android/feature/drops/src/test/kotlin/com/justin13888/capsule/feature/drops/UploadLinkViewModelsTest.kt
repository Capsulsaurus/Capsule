package com.justin13888.capsule.feature.drops

import app.cash.turbine.test
import com.justin13888.capsule.core.data.DropError
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.UploadLink
import com.justin13888.capsule.core.model.UploadLinkId
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
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.BeforeEach
import org.junit.jupiter.api.Test

/**
 * The upload-link list, form and detail.
 *
 * Liveness is asserted from both sides of the seeded link's expiry, because "is this link
 * still a door?" is the only question on these screens whose answer changes without anybody
 * touching the app — and a client that answered it from stale state would invite a user to
 * revoke something already closed while a genuinely open one kept taking bytes.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class UploadLinkViewModelsTest {
    private val dispatcher = StandardTestDispatcher()

    @BeforeEach
    fun setUp() = Dispatchers.setMain(dispatcher)

    @AfterEach
    fun tearDown() = Dispatchers.resetMain()

    private suspend fun seededLink(repositories: MockRepositories): UploadLink =
        repositories.sharing
            .uploadLinks()
            .first()
            .first()

    @Test
    fun `the list reads the provisioned links and calls the live one open`() =
        runTest(dispatcher) {
            val repositories = DropsFixtures.freshRepositories()
            val viewModel = UploadLinksViewModel(repositories.sharing, DropsFixtures.CLOCK)

            viewModel.state.test {
                assertTrue(awaitItem().isLoading)
                val loaded = awaitItem()
                assertEquals(1, loaded.links.size)
                assertTrue(loaded.links.single().acceptsDeposits)
                assertNull(loaded.failure)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `past its expiry the same link reads as closed`() =
        runTest(dispatcher) {
            val repositories = DropsFixtures.freshRepositories()
            val viewModel = UploadLinksViewModel(repositories.sharing, DropsFixtures.CLOCK_AFTER_EXPIRY)
            advanceUntilIdle()

            assertFalse(
                viewModel.state.value.links
                    .single()
                    .acceptsDeposits,
            )
        }

    @Test
    fun `the form provisions a link under exactly the caps it described`() =
        runTest(dispatcher) {
            val repositories = DropsFixtures.freshRepositories()
            val viewModel = UploadLinkCreateViewModel(repositories.sharing, DropsFixtures.CLOCK)
            viewModel.setCap(CapKind.TotalBytes, "500")
            viewModel.setCap(CapKind.FileCount, "20")
            viewModel.setCap(CapKind.FileSize, "25")
            viewModel.setSingleUse(true)
            viewModel.setPassphrase("let me in")
            viewModel.create()
            advanceUntilIdle()

            val created = requireNotNull(viewModel.state.value.created)
            assertEquals(500L * BYTES_PER_MEGABYTE, created.caps.maxTotalBytes)
            assertEquals(20, created.caps.maxFileCount)
            assertEquals(25L * BYTES_PER_MEGABYTE, created.caps.maxFileSize)
            assertTrue(created.caps.singleUse)
            assertTrue(created.requiresPassphrase)
            assertEquals(0L, created.bytesReceived, "a fresh link has spent nothing")
        }

    @Test
    fun `the form refuses to submit a cap it could not parse`() =
        runTest(dispatcher) {
            val repositories = DropsFixtures.freshRepositories()
            val viewModel = UploadLinkCreateViewModel(repositories.sharing, DropsFixtures.CLOCK)
            viewModel.setCap(CapKind.FileCount, "twenty")
            assertFalse(viewModel.state.value.canSubmit)
            viewModel.create()
            advanceUntilIdle()

            assertNull(viewModel.state.value.created)
        }

    @Test
    fun `a refused provisioning surfaces the catalogue code`() =
        runTest(dispatcher) {
            val scenario = MockScenario.Default.failing(MockOperation.LinkWrite, DropError.RateLimited)
            val repositories = DropsFixtures.freshRepositories(scenario)
            val viewModel = UploadLinkCreateViewModel(repositories.sharing, DropsFixtures.CLOCK)
            viewModel.create()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals("error.drop.rate_limited", state.failure?.code?.value)
            assertFalse(state.isSubmitting)
        }

    @Test
    fun `the detail reads usage against every cap that was set`() =
        runTest(dispatcher) {
            val repositories = DropsFixtures.freshRepositories()
            val link = seededLink(repositories)
            val viewModel = UploadLinkDetailViewModel(repositories.sharing, link.id, DropsFixtures.CLOCK)
            advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(link.bytesReceived, state.bytes?.used)
            assertEquals(link.caps.maxTotalBytes, state.bytes?.limit)
            assertEquals(link.filesReceived.toLong(), state.files?.used)
            assertEquals(link.caps.maxFileCount?.toLong(), state.files?.limit)
            assertTrue(state.acceptsDeposits)
            assertFalse(state.hasExpired)
        }

    @Test
    fun `revoking closes the door and says so`() =
        runTest(dispatcher) {
            val repositories = DropsFixtures.freshRepositories()
            val link = seededLink(repositories)
            val viewModel = UploadLinkDetailViewModel(repositories.sharing, link.id, DropsFixtures.CLOCK)
            advanceUntilIdle()
            viewModel.revoke()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(state.isRevoked)
            assertFalse(state.acceptsDeposits)
            assertFalse(state.isMissing, "a link this screen revoked is explained, not just absent")
        }

    @Test
    fun `revoking a link leaves whatever it already delivered in the inbox`() =
        runTest(dispatcher) {
            val repositories = DropsFixtures.freshRepositories()
            val link = seededLink(repositories)
            val before =
                repositories.drops
                    .inbox()
                    .first()
                    .size
            val viewModel = UploadLinkDetailViewModel(repositories.sharing, link.id, DropsFixtures.CLOCK)
            advanceUntilIdle()
            viewModel.revoke()
            advanceUntilIdle()

            assertEquals(
                before,
                repositories.drops
                    .inbox()
                    .first()
                    .size,
            )
        }

    @Test
    fun `a link that is not provisioned reads as gone`() =
        runTest(dispatcher) {
            val repositories = DropsFixtures.freshRepositories()
            val viewModel =
                UploadLinkDetailViewModel(repositories.sharing, UploadLinkId("never-existed"), DropsFixtures.CLOCK)
            advanceUntilIdle()

            assertTrue(viewModel.state.value.isMissing)
            assertFalse(viewModel.state.value.canRevoke)
        }

    @Test
    fun `a refused revocation is reported and the link stays open`() =
        runTest(dispatcher) {
            val scenario = MockScenario.Default.failing(MockOperation.LinkWrite, DropError.RateLimited)
            val repositories = DropsFixtures.freshRepositories(scenario)
            val link = seededLink(repositories)
            val viewModel = UploadLinkDetailViewModel(repositories.sharing, link.id, DropsFixtures.CLOCK)
            advanceUntilIdle()
            viewModel.revoke()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertFalse(state.isRevoked)
            assertEquals("error.drop.rate_limited", state.failure?.code?.value)
            assertTrue(state.canRevoke)
        }
}
