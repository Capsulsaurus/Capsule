package com.justin13888.capsule.feature.sharing

import app.cash.turbine.test
import com.justin13888.capsule.core.data.ShareError
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.AlbumKind
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.ShareScope
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
import java.time.Duration

/**
 * The create-share-link form.
 *
 * Two things are load-bearing and are asserted rather than assumed: a link can only be
 * scoped to a *container* album, because a view album holds no key and would grant access to
 * nothing; and the passphrase reaches the repository, which is where it wraps the secret
 * locally rather than travelling anywhere.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ShareLinkCreateViewModelTest {
    private val dispatcher = StandardTestDispatcher()

    @BeforeEach
    fun setUp() = Dispatchers.setMain(dispatcher)

    @AfterEach
    fun tearDown() = Dispatchers.resetMain()

    @Test
    fun `only container albums are offered, and one is preselected`() =
        runTest(dispatcher) {
            val repositories = SharingFixtures.freshRepositories()
            val viewModel =
                ShareLinkCreateViewModel(
                    repositories.sharing,
                    repositories.albums,
                    clock = SharingFixtures.CLOCK,
                )
            advanceUntilIdle()

            val state = viewModel.state.value
            val containers =
                repositories.albums
                    .albums()
                    .first()
                    .filter { it.kind.knownOrNull == AlbumKind.Container }
            assertEquals(containers.size, state.albums.size)
            assertTrue(containers.isNotEmpty(), "the fixture has container albums to offer")
            assertNotNull(state.selectedAlbumId, "an album scope with nothing selected cannot be submitted")
            assertTrue(state.canSubmit)
        }

    @Test
    fun `the single-item scope is refused when the screen was not opened from an item`() =
        runTest(dispatcher) {
            val repositories = SharingFixtures.freshRepositories()
            val viewModel =
                ShareLinkCreateViewModel(
                    repositories.sharing,
                    repositories.albums,
                    clock = SharingFixtures.CLOCK,
                )
            advanceUntilIdle()

            assertFalse(viewModel.state.value.canScopeToAsset)
            viewModel.selectScope(ShareScopeChoice.SingleAsset)
            assertEquals(
                ShareScopeChoice.WholeAlbum,
                viewModel.state.value.scope,
                "the form must not describe a scope the repository would reject",
            )
        }

    @Test
    fun `opening from an item scopes to that item`() =
        runTest(dispatcher) {
            val repositories = SharingFixtures.freshRepositories()
            val asset = AssetId("asset-under-test")
            val viewModel =
                ShareLinkCreateViewModel(
                    repositories.sharing,
                    repositories.albums,
                    assetId = asset,
                    clock = SharingFixtures.CLOCK,
                )
            advanceUntilIdle()

            assertEquals(ShareScopeChoice.SingleAsset, viewModel.state.value.scope)
            assertTrue(viewModel.state.value.canScopeToAsset)
        }

    @Test
    fun `creating passes the chosen expiry and passphrase through`() =
        runTest(dispatcher) {
            val repositories = SharingFixtures.freshRepositories()
            val viewModel =
                ShareLinkCreateViewModel(
                    repositories.sharing,
                    repositories.albums,
                    clock = SharingFixtures.CLOCK,
                )
            advanceUntilIdle()
            viewModel.setPassphrase("correct horse")
            viewModel.selectExpiry(ShareExpiry.OneWeek)
            viewModel.create()
            advanceUntilIdle()

            val created = requireNotNull(viewModel.state.value.created)
            assertTrue(created.requiresPassphrase, "a passphrase makes the link passphrase-wrapped")
            assertEquals(SharingFixtures.CLOCK.instant().plus(Duration.ofDays(7)), created.expiresAt)
            assertTrue(created.scope is ShareScope.WholeAlbum)
            assertNotNull(created.fragmentSecret, "the secret comes back exactly once, here")
        }

    @Test
    fun `a blank passphrase means no passphrase at all`() =
        runTest(dispatcher) {
            val repositories = SharingFixtures.freshRepositories()
            val viewModel =
                ShareLinkCreateViewModel(
                    repositories.sharing,
                    repositories.albums,
                    clock = SharingFixtures.CLOCK,
                )
            advanceUntilIdle()
            viewModel.setPassphrase("   ")
            viewModel.create()
            advanceUntilIdle()

            assertFalse(requireNotNull(viewModel.state.value.created).requiresPassphrase)
        }

    @Test
    fun `a refused creation surfaces the catalogue code and leaves the form usable`() =
        runTest(dispatcher) {
            val scenario = MockScenario.Default.failing(MockOperation.LinkWrite, ShareError.RateLimited)
            val repositories = SharingFixtures.freshRepositories(scenario)
            val viewModel =
                ShareLinkCreateViewModel(
                    repositories.sharing,
                    repositories.albums,
                    clock = SharingFixtures.CLOCK,
                )
            advanceUntilIdle()

            viewModel.state.test {
                skipItems(1)
                viewModel.create()
                advanceUntilIdle()
                val failed = expectMostRecentItem()
                assertEquals(SharingFailureKind.Reported, failed.failure?.kind)
                assertEquals("error.share.rate_limited", failed.failure?.code?.value)
                assertNull(failed.created)
                assertFalse(failed.isSubmitting, "the form is usable again after a refusal")
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `creating with no network reports offline rather than a coded error`() =
        runTest(dispatcher) {
            val repositories = SharingFixtures.freshRepositories(MockScenario.Offline)
            val viewModel =
                ShareLinkCreateViewModel(
                    repositories.sharing,
                    repositories.albums,
                    clock = SharingFixtures.CLOCK,
                )
            advanceUntilIdle()
            viewModel.create()
            advanceUntilIdle()

            assertEquals(
                SharingFailureKind.Offline,
                viewModel.state.value.failure
                    ?.kind,
            )
        }
}
