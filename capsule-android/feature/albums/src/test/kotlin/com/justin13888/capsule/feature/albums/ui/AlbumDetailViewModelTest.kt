package com.justin13888.capsule.feature.albums.ui

import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.model.AlbumCapability
import com.justin13888.capsule.core.model.UserId
import com.justin13888.capsule.feature.albums.AlbumsFixtures
import com.justin13888.capsule.feature.albums.data.DerivedFederatedAlbumSource
import com.justin13888.capsule.feature.albums.data.InMemoryAlbumPolicyStore
import com.justin13888.capsule.feature.albums.data.InMemoryAlbumUpgradeCoordinator
import com.justin13888.capsule.feature.albums.data.LibrarySample
import com.justin13888.capsule.feature.albums.model.CollectionRef
import com.justin13888.capsule.feature.albums.model.MembershipRefusal
import com.justin13888.capsule.feature.albums.model.UpgradePhase
import com.justin13888.capsule.feature.albums.ui.detail.AlbumDetailMessage
import com.justin13888.capsule.feature.albums.ui.detail.AlbumDetailUiState.Content
import com.justin13888.capsule.feature.albums.ui.detail.AlbumDetailViewModel
import com.justin13888.capsule.feature.albums.viewModelTest
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.advanceUntilIdle
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * Album detail: what it loads, what it refuses, and how the upgrade ceremony runs from it.
 *
 * The refusals are the interesting half. A capability *is* which keys MLS delivered, so a
 * client that wrote a membership change optimistically would be showing the user a role the
 * group never granted; and the default album has to stay deletable-only-after-repointing or
 * the next import has nowhere to land.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class AlbumDetailViewModelTest {
    private val policies = InMemoryAlbumPolicyStore()

    private suspend fun viewModel(
        mock: MockRepositories,
        ref: CollectionRef,
        upgrades: InMemoryAlbumUpgradeCoordinator = InMemoryAlbumUpgradeCoordinator(clock = AlbumsFixtures.CLOCK),
    ) = AlbumDetailViewModel(
        ref = ref,
        albums = mock.albums,
        policies = policies,
        upgrades = upgrades,
        federated = DerivedFederatedAlbumSource(mock.albums),
        sample = LibrarySample(mock.timeline, limit = SAMPLE, pageSize = SAMPLE),
        clock = AlbumsFixtures.CLOCK,
    )

    private suspend fun defaultAlbumRef(mock: MockRepositories): CollectionRef.Container =
        CollectionRef.Container(
            mock.albums
                .albums()
                .first()
                .first { it.isDefault }
                .id,
        )

    private suspend fun sharedAlbumRef(mock: MockRepositories): CollectionRef.Container =
        CollectionRef.Container(
            mock.albums
                .albums()
                .first()
                .first { it.members.size > 1 }
                .id,
        )

    private fun content(model: AlbumDetailViewModel) = assertInstanceOf(Content::class.java, model.uiState.value)

    @Test
    fun `a container loads with its contents, its policy and its membership`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val model = viewModel(mock, sharedAlbumRef(mock))
            advanceUntilIdle()

            val state = content(model)
            assertTrue(state.assets.isNotEmpty())
            assertTrue(state.policy != null)
            assertTrue(requireNotNull(state.album).members.isNotEmpty())
        }

    @Test
    fun `renaming writes through to the album`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val ref = sharedAlbumRef(mock)
            val model = viewModel(mock, ref)
            advanceUntilIdle()

            model.rename("Household")
            advanceUntilIdle()

            assertEquals(
                "Household",
                mock.albums
                    .album(ref.albumId)
                    .first()
                    ?.name,
            )
        }

    @Test
    fun `the designated default refuses deletion and says why`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val model = viewModel(mock, defaultAlbumRef(mock))
            advanceUntilIdle()

            var navigatedAway = false
            model.delete { navigatedAway = true }
            advanceUntilIdle()

            assertEquals(AlbumDetailMessage.DefaultUndeletable, content(model).message)
            assertTrue(!navigatedAway)
        }

    @Test
    fun `an admin's invite reaches the album`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val ref = sharedAlbumRef(mock)
            val model = viewModel(mock, ref)
            advanceUntilIdle()
            val before = requireNotNull(content(model).album).members.size

            model.invite("newcomer", AlbumCapability.Read)
            advanceUntilIdle()

            assertEquals(
                before + 1,
                mock.albums
                    .album(ref.albumId)
                    .first()
                    ?.members
                    ?.size,
            )
        }

    @Test
    fun `removing the only admin is refused before anything is proposed`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val ref = sharedAlbumRef(mock)
            val model = viewModel(mock, ref)
            advanceUntilIdle()
            val admin =
                requireNotNull(content(model).album)
                    .members
                    .first()
                    .userId

            model.removeMember(admin)
            advanceUntilIdle()

            assertEquals(AlbumDetailMessage.Refused(MembershipRefusal.LastAdmin), content(model).message)
            assertEquals(
                admin,
                mock.albums
                    .album(ref.albumId)
                    .first()
                    ?.members
                    ?.first()
                    ?.userId,
            )
        }

    @Test
    fun `a non-admin's invite never reaches the repository`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val ref = sharedAlbumRef(mock)
            val album = requireNotNull(mock.albums.album(ref.albumId).first())
            mock.albums.updateAlbum(album.copy(viewerCapability = null))
            val model = viewModel(mock, ref)
            advanceUntilIdle()

            model.invite("newcomer", AlbumCapability.Read)
            advanceUntilIdle()

            assertEquals(AlbumDetailMessage.Refused(MembershipRefusal.NotAdmin), content(model).message)
            assertEquals(
                album.members.size,
                mock.albums
                    .album(ref.albumId)
                    .first()
                    ?.members
                    ?.size,
            )
        }

    @Test
    fun `the upgrade ceremony walks quiesce, drain, tombstone, fork and records the new pin`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val ref = sharedAlbumRef(mock)
            val model = viewModel(mock, ref)
            advanceUntilIdle()

            model.startUpgrade()
            advanceUntilIdle()
            assertInstanceOf(UpgradePhase.Quiescing::class.java, content(model).upgrade)

            repeat(UPGRADE_STEPS) {
                model.advanceUpgrade()
                advanceUntilIdle()
            }

            val complete = assertInstanceOf(UpgradePhase.Complete::class.java, content(model).upgrade)
            assertEquals(complete.toVersion, policies.policy(ref.albumId).first().protocolPin)
        }

    @Test
    fun `aborting mid-ceremony returns the album to normal`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val model = viewModel(mock, sharedAlbumRef(mock))
            advanceUntilIdle()

            model.startUpgrade()
            advanceUntilIdle()
            model.abortUpgrade()
            advanceUntilIdle()

            assertInstanceOf(UpgradePhase.Aborted::class.java, content(model).upgrade)
        }

    @Test
    fun `a member whose copy of the album differs aborts the fork`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val ref = sharedAlbumRef(mock)
            val model =
                viewModel(
                    mock,
                    ref,
                    upgrades =
                        InMemoryAlbumUpgradeCoordinator(clock = AlbumsFixtures.CLOCK, membersAgree = { false }),
                )
            advanceUntilIdle()

            model.startUpgrade()
            advanceUntilIdle()
            repeat(UPGRADE_STEPS) {
                model.advanceUpgrade()
                advanceUntilIdle()
            }

            assertInstanceOf(UpgradePhase.Aborted::class.java, content(model).upgrade)
        }

    @Test
    fun `a built-in view shows only the assets it computes over, and has no membership`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val model =
                viewModel(
                    mock,
                    CollectionRef.SystemView(com.justin13888.capsule.feature.albums.model.SystemViewKind.Videos),
                )
            advanceUntilIdle()

            val state = content(model)
            assertTrue(state.album == null)
            assertTrue(state.policy == null)
            assertTrue(state.assets.all { asset -> asset.duration != null || asset.dimensions != null })
        }

    @Test
    fun `an aggregate lists its contributors and admits an unreachable one`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val shared = sharedAlbumRef(mock)
            val model = viewModel(mock, CollectionRef.Federated(shared.albumId.value))
            advanceUntilIdle()

            val group = requireNotNull(content(model).federated)
            assertTrue(group.constituents.any { it.isOwn })
            assertTrue(group.unavailable.isNotEmpty())
        }

    @Test
    fun `an unknown user cannot be removed from an album that does not list them`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val ref = sharedAlbumRef(mock)
            val model = viewModel(mock, ref)
            advanceUntilIdle()
            val before = requireNotNull(content(model).album).members.size

            model.removeMember(UserId("nobody"))
            advanceUntilIdle()

            assertEquals(
                before,
                mock.albums
                    .album(ref.albumId)
                    .first()
                    ?.members
                    ?.size,
            )
        }

    private companion object {
        /** Quiesce → drain → tombstone → fork → complete is four advances after the proposal. */
        const val UPGRADE_STEPS = 4

        /** Small enough to keep the suite quick. */
        const val SAMPLE = 256
    }
}
