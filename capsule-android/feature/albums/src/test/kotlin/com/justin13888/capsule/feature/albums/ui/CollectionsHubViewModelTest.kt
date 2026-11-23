package com.justin13888.capsule.feature.albums.ui

import com.justin13888.capsule.core.model.AlbumKind
import com.justin13888.capsule.core.model.knownOrNull
import com.justin13888.capsule.feature.albums.AlbumsFixtures
import com.justin13888.capsule.feature.albums.data.DerivedFederatedAlbumSource
import com.justin13888.capsule.feature.albums.data.LibrarySample
import com.justin13888.capsule.feature.albums.ui.hub.CollectionsHubUiState
import com.justin13888.capsule.feature.albums.ui.hub.CollectionsHubViewModel
import com.justin13888.capsule.feature.albums.viewModelTest
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.advanceUntilIdle
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The hub's split between containers and views.
 *
 * The ordering assertion is not cosmetic: the nameless default album is where every unfiled
 * import lands, so it is the row reached for most often and the one that must not move as
 * other albums are renamed around it.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class CollectionsHubViewModelTest {
    private fun viewModel(mock: com.justin13888.capsule.core.mock.MockRepositories) =
        CollectionsHubViewModel(
            albums = mock.albums,
            search = mock.search,
            federated = DerivedFederatedAlbumSource(mock.albums),
            sample = LibrarySample(mock.timeline, limit = SAMPLE, pageSize = SAMPLE),
            clock = AlbumsFixtures.CLOCK,
        )

    @Test
    fun `containers and views are listed separately, with the default album first`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val model = viewModel(mock)
            advanceUntilIdle()

            val state = assertInstanceOf(CollectionsHubUiState.Content::class.java, model.uiState.value)
            assertTrue(state.myAlbums.first().isDefault)

            val containers =
                mock.albums
                    .albums()
                    .first()
                    .filter { it.kind.knownOrNull == AlbumKind.Container }
            assertEquals(containers.size, state.myAlbums.size)
            assertTrue(state.viewAlbums.isNotEmpty())
            assertTrue(state.viewAlbums.none { row -> row.id in containers.map { it.id } })
        }

    @Test
    fun `every built-in view gets a row, counted from the sample`() =
        viewModelTest {
            val model = viewModel(AlbumsFixtures.repositories())
            advanceUntilIdle()

            val state = assertInstanceOf(CollectionsHubUiState.Content::class.java, model.uiState.value)
            assertEquals(
                com.justin13888.capsule.feature.albums.model.SystemViewKind.entries.size,
                state.mediaTypes.size,
            )
            assertTrue(state.mediaTypes.all { it.count >= 0 })
        }

    @Test
    fun `a shared container is also surfaced as an aggregate, with one unreachable origin`() =
        viewModelTest {
            val model = viewModel(AlbumsFixtures.repositories())
            advanceUntilIdle()

            val state = assertInstanceOf(CollectionsHubUiState.Content::class.java, model.uiState.value)
            val group = state.federated.firstOrNull()
            requireNotNull(group) { "the seeded library shares one album, so an aggregate must exist" }
            assertTrue(group.constituents.any { it.isOwn })
            assertTrue(group.unavailable.isNotEmpty())
        }

    @Test
    fun `an empty library still shows its default container and no albums to open`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories(com.justin13888.capsule.core.mock.MockScenario.FirstRun)
            val model = viewModel(mock)
            advanceUntilIdle()

            val state = assertInstanceOf(CollectionsHubUiState.Content::class.java, model.uiState.value)
            assertEquals(1, state.myAlbums.size)
            assertTrue(state.myAlbums.single().isDefault)
        }

    @Test
    fun `creating an album adds a container the user can file photographs into`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val model = viewModel(mock)
            advanceUntilIdle()
            val before = assertInstanceOf(CollectionsHubUiState.Content::class.java, model.uiState.value)

            model.createAlbum("  Iceland 2026  ")
            advanceUntilIdle()

            val after = assertInstanceOf(CollectionsHubUiState.Content::class.java, model.uiState.value)
            assertEquals(before.myAlbums.size + 1, after.myAlbums.size)
            assertTrue(after.myAlbums.any { it.name == "Iceland 2026" })
            assertTrue(after.viewAlbums.none { it.name == "Iceland 2026" })
        }

    @Test
    fun `a blank name creates nothing`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val model = viewModel(mock)
            advanceUntilIdle()
            val before = assertInstanceOf(CollectionsHubUiState.Content::class.java, model.uiState.value)

            model.createAlbum("   ")
            advanceUntilIdle()

            val after = assertInstanceOf(CollectionsHubUiState.Content::class.java, model.uiState.value)
            assertEquals(before.myAlbums.size, after.myAlbums.size)
        }

    private companion object {
        /** Small enough to keep the suite quick, large enough to reach every media type. */
        const val SAMPLE = 512
    }
}
