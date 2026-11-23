package com.justin13888.capsule.feature.albums.ui

import com.justin13888.capsule.feature.albums.AlbumsFixtures
import com.justin13888.capsule.feature.albums.data.LibrarySample
import com.justin13888.capsule.feature.albums.ui.stacks.StackDetailUiState
import com.justin13888.capsule.feature.albums.ui.stacks.StackDetailViewModel
import com.justin13888.capsule.feature.albums.ui.stacks.StackMemberRow
import com.justin13888.capsule.feature.albums.ui.stacks.StacksUiState
import com.justin13888.capsule.feature.albums.ui.stacks.StacksViewModel
import com.justin13888.capsule.feature.albums.viewModelTest
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.advanceUntilIdle
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * Stacks, and the two edits the design says are metadata-only.
 *
 * The reorder test also pins the invariant that makes the rebuild safe: no member is lost and
 * the best-photo pointer survives, because leaving and rejoining a stack is a stamped absence
 * followed by a stamped membership rather than anything that touches bytes.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class StacksViewModelTest {
    @Test
    fun `every stack in the sample gets one row, with its real member count`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val model = StacksViewModel(mock.stacks, LibrarySample(mock.timeline, limit = SAMPLE, pageSize = SAMPLE))
            advanceUntilIdle()

            val state = assertInstanceOf(StacksUiState.Content::class.java, model.uiState.value)
            assertTrue(state.rows.isNotEmpty(), "the seeded library groups bursts and RAW pairs")
            state.rows.forEach { row -> assertEquals(mock.stacks.stackMembers(row.id).size, row.memberCount) }
            assertEquals(
                state.rows
                    .map { it.id }
                    .distinct()
                    .size,
                state.rows.size,
            )
        }

    @Test
    fun `choosing a different best photo moves the pointer and nothing else`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val stackId = requireNotNull(AlbumsFixtures.LIBRARY.landmarks.burstStackId)
            val model = StackDetailViewModel(stackId, mock.stacks, mock.assets)
            advanceUntilIdle()

            val members = assertInstanceOf(StackDetailUiState.Content::class.java, model.uiState.value).members
            val candidate = members.first { !it.isPrimary }

            model.setPrimary(candidate.member.assetId)
            advanceUntilIdle()

            val after = assertInstanceOf(StackDetailUiState.Content::class.java, model.uiState.value)
            assertEquals(candidate.member.assetId, after.stack.primaryAssetId)
            assertEquals(members.size, after.members.size)
        }

    @Test
    fun `reordering keeps every member and the best-photo pointer`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val stackId = requireNotNull(AlbumsFixtures.LIBRARY.landmarks.burstStackId)
            val model = StackDetailViewModel(stackId, mock.stacks, mock.assets)
            advanceUntilIdle()

            val before = assertInstanceOf(StackDetailUiState.Content::class.java, model.uiState.value)
            val ids = before.members.map { it.member.assetId }.toSet()
            val primary = before.stack.primaryAssetId

            model.moveLater(0)
            advanceUntilIdle()

            val after = assertInstanceOf(StackDetailUiState.Content::class.java, model.uiState.value)
            assertEquals(
                ids,
                after.members
                    .map(StackMemberRow::member)
                    .map { it.assetId }
                    .toSet(),
            )
            assertEquals(primary, requireNotNull(mock.stacks.stack(stackId).first()).primaryAssetId)
        }

    @Test
    fun `a stack that does not exist reports a failure rather than an empty stack`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val model =
                StackDetailViewModel(
                    com.justin13888.capsule.core.model
                        .StackId("nope"),
                    mock.stacks,
                    mock.assets,
                )
            advanceUntilIdle()

            assertInstanceOf(StackDetailUiState.Failed::class.java, model.uiState.value)
        }

    private companion object {
        const val SAMPLE = 512
    }
}
