package com.justin13888.capsule.feature.albums.ui

import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.model.Operator
import com.justin13888.capsule.core.model.QueryField
import com.justin13888.capsule.core.model.SmartAlbumDefinition
import com.justin13888.capsule.core.model.SmartAlbumId
import com.justin13888.capsule.core.model.SmartAlbumValidation
import com.justin13888.capsule.core.model.SmartAlbumValidator
import com.justin13888.capsule.feature.albums.AlbumsFixtures
import com.justin13888.capsule.feature.albums.builder.DraftGroup
import com.justin13888.capsule.feature.albums.builder.DraftNode
import com.justin13888.capsule.feature.albums.builder.DraftNodeId
import com.justin13888.capsule.feature.albums.builder.DraftOperand
import com.justin13888.capsule.feature.albums.builder.EditRefusal
import com.justin13888.capsule.feature.albums.builder.GroupKind
import com.justin13888.capsule.feature.albums.data.LibrarySample
import com.justin13888.capsule.feature.albums.data.LocalSmartAlbumPreview
import com.justin13888.capsule.feature.albums.ui.smart.SmartAlbumBuilderViewModel
import com.justin13888.capsule.feature.albums.viewModelTest
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.advanceUntilIdle
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The builder as a whole: edit, validate, count, save.
 *
 * Both gates are exercised here rather than only the editor's, because they catch different
 * things. The editor stops an out-of-bounds tree ever existing; the definition validator is
 * the same gate a definition arriving over sync has to pass, so running it on every edit is
 * what guarantees the builder cannot produce something another device would refuse.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class SmartAlbumBuilderViewModelTest {
    private fun viewModel(
        mock: MockRepositories,
        id: SmartAlbumId? = null,
    ) = SmartAlbumBuilderViewModel(
        smartAlbumId = id,
        search = mock.search,
        sessions = mock.sessions,
        preview = LocalSmartAlbumPreview(LibrarySample(mock.timeline, limit = SAMPLE, pageSize = SAMPLE)),
        clock = AlbumsFixtures.CLOCK,
    )

    @Test
    fun `an empty rule cannot be saved until it is named`() =
        viewModelTest {
            val model = viewModel(AlbumsFixtures.repositories())
            advanceUntilIdle()
            assertFalse(model.uiState.value.canSave)

            model.setName("Keepers")
            advanceUntilIdle()
            assertTrue(model.uiState.value.canSave)
        }

    @Test
    fun `the live count is recomputed as the rule changes`() =
        viewModelTest {
            val model = viewModel(AlbumsFixtures.repositories())
            model.setName("Keepers")
            advanceUntilIdle()
            val everything = requireNotNull(model.uiState.value.matchCount).matches

            val root = model.uiState.value.draft.root.id
            model.addCondition(root)
            advanceUntilIdle()
            val id =
                model.uiState.value.draft.root.children
                    .single()
                    .id
            model.setField(id, QueryField.Rating)
            advanceUntilIdle()
            model.setOperator(id, Operator.Gte)
            advanceUntilIdle()
            model.setOperand(id, DraftOperand.Number("5"))
            advanceUntilIdle()

            val narrowed = requireNotNull(model.uiState.value.matchCount).matches
            assertTrue(narrowed < everything, "a five-star filter should match fewer than an empty rule")
        }

    @Test
    fun `an operator illegal for the chosen field is refused and the rule is left alone`() =
        viewModelTest {
            val model = viewModel(AlbumsFixtures.repositories())
            model.setName("Keepers")
            model.addCondition(model.uiState.value.draft.root.id)
            advanceUntilIdle()
            val id =
                model.uiState.value.draft.root.children
                    .single()
                    .id
            val before = model.uiState.value.draft

            model.setOperator(id, Operator.Contains)
            advanceUntilIdle()

            assertInstanceOf(EditRefusal.OperatorNotValidForField::class.java, model.uiState.value.refusal)
            assertEquals(before.root, model.uiState.value.draft.root)
        }

    @Test
    fun `a half-typed operand blocks the save without being reported as a violation`() =
        viewModelTest {
            val model = viewModel(AlbumsFixtures.repositories())
            model.setName("Keepers")
            model.addCondition(model.uiState.value.draft.root.id)
            advanceUntilIdle()
            val id =
                model.uiState.value.draft.root.children
                    .single()
                    .id

            model.setOperand(id, DraftOperand.TemporalPoint("2026-0"))
            advanceUntilIdle()

            assertEquals(listOf(id), model.uiState.value.incomplete)
            assertTrue(
                model.uiState.value.violations
                    .isEmpty(),
            )
            assertFalse(model.uiState.value.canSave)
        }

    @Test
    fun `the depth limit is refused at the editor rather than at the save`() =
        viewModelTest {
            val model = viewModel(AlbumsFixtures.repositories())
            model.setName("Deep")
            advanceUntilIdle()
            var parent = model.uiState.value.draft.root.id
            repeat(7) {
                model.addGroup(parent, GroupKind.All)
                advanceUntilIdle()
                parent = deepestGroupId(model)
            }
            model.addGroup(parent, GroupKind.All)
            advanceUntilIdle()

            assertEquals(EditRefusal.DepthLimit, model.uiState.value.refusal)
        }

    @Test
    fun `saving writes one whole definition that the validator accepts`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val model = viewModel(mock)
            model.setName("Five stars")
            model.addCondition(model.uiState.value.draft.root.id)
            advanceUntilIdle()
            val id =
                model.uiState.value.draft.root.children
                    .single()
                    .id
            model.setField(id, QueryField.Rating)
            advanceUntilIdle()
            model.setOperator(id, Operator.Gte)
            advanceUntilIdle()
            model.setOperand(id, DraftOperand.Number("5"))
            advanceUntilIdle()

            model.save()
            advanceUntilIdle()

            assertTrue(model.uiState.value.isSaved)
            val saved =
                mock.search
                    .smartAlbums()
                    .first()
                    .first { it.displayName.value == "Five stars" }
            assertInstanceOf(SmartAlbumValidation.Evaluable::class.java, SmartAlbumValidator.validate(saved))
        }

    @Test
    fun `a stored rule reopens in the editor exactly as written`() =
        viewModelTest {
            val mock = AlbumsFixtures.repositories()
            val existing: SmartAlbumDefinition =
                mock.search
                    .smartAlbums()
                    .first()
                    .first { SmartAlbumValidator.validate(it) is SmartAlbumValidation.Evaluable }
            val model = viewModel(mock, existing.id)
            advanceUntilIdle()

            assertFalse(model.uiState.value.isLoading)
            assertFalse(model.uiState.value.isForwardSchema)
            assertEquals(existing.displayName.value, model.uiState.value.name)
            assertNotNull(model.uiState.value.matchCount)
        }

    private fun deepestGroupId(model: SmartAlbumBuilderViewModel): DraftNodeId {
        var node: DraftNode = model.uiState.value.draft.root
        while (node is DraftGroup && node.children.isNotEmpty()) node = node.children.last()
        return node.id
    }

    private companion object {
        /** Small enough to keep the live count quick in a suite. */
        const val SAMPLE = 256
    }
}
