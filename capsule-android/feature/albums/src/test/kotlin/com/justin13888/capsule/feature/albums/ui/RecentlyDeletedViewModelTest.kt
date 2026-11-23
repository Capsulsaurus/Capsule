package com.justin13888.capsule.feature.albums.ui

import com.justin13888.capsule.feature.albums.AlbumsFixtures
import com.justin13888.capsule.feature.albums.model.RetentionCountdown
import com.justin13888.capsule.feature.albums.ui.trash.RecentlyDeletedUiState.Content
import com.justin13888.capsule.feature.albums.ui.trash.RecentlyDeletedViewModel
import com.justin13888.capsule.feature.albums.ui.trash.TrashedItem
import com.justin13888.capsule.feature.albums.viewModelTest
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceUntilIdle
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * Recently Deleted, held to the signed retention window.
 *
 * Emptying the trash is the case worth pinning: the window is signed into the delete manifest
 * at delete time precisely so that nothing downstream can shorten it, and a client that
 * purged everything on "empty trash" would be doing exactly what that signature exists to
 * prevent.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class RecentlyDeletedViewModelTest {
    private fun content(model: RecentlyDeletedViewModel) = assertInstanceOf(Content::class.java, model.uiState.value)

    @Test
    fun `every item carries a countdown derived from its own signed deadline`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val model = RecentlyDeletedViewModel(mock.timeline, AlbumsFixtures.CLOCK)
            advanceUntilIdle()

            val items = content(model).items
            assertTrue(items.isNotEmpty(), "the seeded library trashes several assets")
            items.forEach { item ->
                assertEquals(RetentionCountdown.of(retention(item), AlbumsFixtures.SPEC.now), item.countdown)
            }
        }

    @Test
    fun `an item past its window is offered for purge and the rest are not`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val model = RecentlyDeletedViewModel(mock.timeline, AlbumsFixtures.CLOCK)
            advanceUntilIdle()

            val state = content(model)
            assertTrue(state.hasPurgeable)
            assertTrue(state.items.any { !it.isPurgeable })
        }

    @Test
    fun `restoring takes an item out of the trash`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val model = RecentlyDeletedViewModel(mock.timeline, AlbumsFixtures.CLOCK)
            advanceUntilIdle()
            val target = content(model).items.first { !it.isPurgeable }

            model.restore(target.asset.id)
            advanceUntilIdle()

            assertTrue(content(model).items.none { it.asset.id == target.asset.id })
        }

    @Test
    fun `emptying the trash removes only what is already past its window`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val model = RecentlyDeletedViewModel(mock.timeline, AlbumsFixtures.CLOCK)
            advanceUntilIdle()
            val before = content(model).items
            val stillProtected = before.filter { !it.isPurgeable }.map { it.asset.id }

            model.emptyTrash()
            advanceUntilIdle()

            val after = content(model).items
            assertEquals(stillProtected.toSet(), after.map { it.asset.id }.toSet())
            assertFalse(content(model).hasPurgeable)
        }

    private fun retention(item: TrashedItem) =
        com.justin13888.capsule.core.model.DeletionState
            .Trashed(deletedAt = item.deletedAt, retentionUntil = item.retentionUntil)
}
