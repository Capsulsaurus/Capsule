package com.justin13888.capsule.feature.sharing

import com.justin13888.capsule.core.data.ShareError
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.ShareLink
import com.justin13888.capsule.core.model.ShareLinkId
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
 * One share link's detail.
 *
 * Revocation is the interesting half. There is no undo, the far side cannot tell a revoked
 * link from one that never existed, and this screen is where a user learns both of those
 * things — so the state after a revoke has to say "revoked", not "missing", and a refused
 * revoke must not pretend it succeeded.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ShareLinkDetailViewModelTest {
    private val dispatcher = StandardTestDispatcher()

    @BeforeEach
    fun setUp() = Dispatchers.setMain(dispatcher)

    @AfterEach
    fun tearDown() = Dispatchers.resetMain()

    private suspend fun liveLink(repositories: com.justin13888.capsule.core.mock.MockRepositories): ShareLink =
        repositories.sharing
            .shareLinks()
            .first()
            .first { it.fragmentSecret != null }

    @Test
    fun `the detail builds the complete URL, fragment included`() =
        runTest(dispatcher) {
            val repositories = SharingFixtures.freshRepositories()
            val link = liveLink(repositories)
            val viewModel =
                ShareLinkDetailViewModel(
                    repositories.sharing,
                    repositories.albums,
                    link.id,
                    SharingFixtures.ORIGIN,
                )
            advanceUntilIdle()

            val url = requireNotNull(viewModel.state.value.url)
            assertTrue(url.endsWith("#" + link.fragmentSecret), "the secret has to be in the URL")
            assertTrue(url.contains("/s/" + link.opaqueId))
            assertTrue(viewModel.state.value.canShare)
        }

    @Test
    fun `revoking says revoked, not missing, and withdraws the URL`() =
        runTest(dispatcher) {
            val repositories = SharingFixtures.freshRepositories()
            val link = liveLink(repositories)
            val viewModel =
                ShareLinkDetailViewModel(
                    repositories.sharing,
                    repositories.albums,
                    link.id,
                    SharingFixtures.ORIGIN,
                )
            advanceUntilIdle()
            viewModel.revoke()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(state.isRevoked)
            assertFalse(state.isMissing, "a link this screen revoked is explained, not just absent")
            assertNull(state.url)
            assertFalse(state.canRevoke, "there is nothing left to revoke")
        }

    @Test
    fun `a link that is not in the issued list reads as no longer live`() =
        runTest(dispatcher) {
            val repositories = SharingFixtures.freshRepositories()
            val viewModel =
                ShareLinkDetailViewModel(
                    repositories.sharing,
                    repositories.albums,
                    ShareLinkId("never-existed"),
                    SharingFixtures.ORIGIN,
                )
            advanceUntilIdle()

            assertTrue(viewModel.state.value.isMissing)
            assertFalse(viewModel.state.value.canShare)
        }

    @Test
    fun `a refused revocation is reported and the link stays live`() =
        runTest(dispatcher) {
            val scenario = MockScenario.Default.failing(MockOperation.LinkWrite, ShareError.RateLimited)
            val repositories = SharingFixtures.freshRepositories(scenario)
            val link = liveLink(repositories)
            val viewModel =
                ShareLinkDetailViewModel(
                    repositories.sharing,
                    repositories.albums,
                    link.id,
                    SharingFixtures.ORIGIN,
                )
            advanceUntilIdle()
            viewModel.revoke()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertFalse(state.isRevoked)
            assertEquals("error.share.rate_limited", state.failure?.code?.value)
            assertTrue(state.canRevoke, "the user can try again")
        }
}
