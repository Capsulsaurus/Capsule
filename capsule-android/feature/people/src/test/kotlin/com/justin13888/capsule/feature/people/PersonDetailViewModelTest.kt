package com.justin13888.capsule.feature.people

import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.model.AiTag
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.UserTag
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.UnconfinedTestDispatcher
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
 * One person's screen.
 *
 * The promotion cases are the ones worth the setup. AI-derived values live in their own
 * namespace so a model can never overwrite user intent; the consequence a test has to hold
 * onto is that promoting *copies* into the user's set under a fresh user-scoped add id and
 * leaves the model's own entry exactly where it was.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class PersonDetailViewModelTest {
    @BeforeEach
    fun installMainDispatcher() {
        Dispatchers.setMain(UnconfinedTestDispatcher())
    }

    @AfterEach
    fun removeMainDispatcher() {
        Dispatchers.resetMain()
    }

    @Test
    fun `it loads the name the user typed and the photographs behind it`() =
        runTest {
            val repositories = PeopleFixtures.repositories()
            val person =
                repositories.people
                    .people()
                    .first()
                    .first()
            val viewModel = PeopleFixtures.personViewModel(repositories, person.id.value)

            val state = viewModel.state.first { it.phase != PeoplePhase.Loading }

            assertEquals(person.displayName, state.displayName)
            assertEquals(PeoplePhase.Content, state.phase)
            assertTrue(state.photos.isNotEmpty())
        }

    @Test
    fun `a person who is not there is a failure with a code, not a blank screen`() =
        runTest {
            val viewModel = PeopleFixtures.personViewModel(PeopleFixtures.repositories(), "no-such-person")

            val state = viewModel.state.first { it.phase != PeoplePhase.Loading }

            assertEquals(PeoplePhase.Failed, state.phase)
            assertEquals(PeopleFailure.Missing, state.failure)
        }

    @Test
    fun `renaming writes the name the user typed`() =
        runTest {
            val repositories = PeopleFixtures.repositories()
            val person =
                repositories.people
                    .people()
                    .first()
                    .first()
            val viewModel = PeopleFixtures.personViewModel(repositories, person.id.value)
            viewModel.state.first { it.phase != PeoplePhase.Loading }

            viewModel.onRenameRequested(open = true)
            viewModel.onRenameSubmit("Someone Else")

            assertEquals("Someone Else", viewModel.state.value.displayName)
            assertFalse(viewModel.state.value.renaming)
        }

    @Test
    fun `promoting a suggestion copies it into the user's own tags and leaves the model's alone`() =
        runTest {
            val repositories = PeopleFixtures.repositories()
            val suggestion = firstSuggestion(repositories)
            val assetId = AssetId(suggestion.assetId)
            val person =
                repositories.people
                    .people()
                    .first()
                    .first()
            val viewModel = PeopleFixtures.personViewModel(repositories, person.id.value)
            viewModel.state.first { it.phase != PeoplePhase.Loading }

            viewModel.onPromote(suggestion)
            val tags = repositories.metadata.tags(assetId).first()

            assertTrue(tags.user.map(UserTag::tag).contains(suggestion.tag), "the tag never reached the user set")
            assertTrue(tags.ai.map(AiTag::tag).contains(suggestion.tag), "the model's own entry was moved, not copied")
            assertNull(viewModel.state.value.actionFailure)
        }

    @Test
    fun `a promoted tag carries a fresh user-scoped add, never the model's`() =
        runTest {
            val repositories = PeopleFixtures.repositories()
            val suggestion = firstSuggestion(repositories)
            val assetId = AssetId(suggestion.assetId)
            val person =
                repositories.people
                    .people()
                    .first()
                    .first()
            val viewModel = PeopleFixtures.personViewModel(repositories, person.id.value)
            viewModel.state.first { it.phase != PeoplePhase.Loading }

            viewModel.onPromote(suggestion)
            val tags = repositories.metadata.tags(assetId).first()

            val promoted = tags.user.first { it.tag == suggestion.tag }
            val model = tags.ai.first { it.tag == suggestion.tag }
            assertFalse(promoted.addId == model.addId, "the promotion reused the model's add id")
        }

    @Test
    fun `dismissing a suggestion removes only the model's entry`() =
        runTest {
            val repositories = PeopleFixtures.repositories()
            val suggestion = firstSuggestion(repositories)
            val assetId = AssetId(suggestion.assetId)
            val person =
                repositories.people
                    .people()
                    .first()
                    .first()
            val viewModel = PeopleFixtures.personViewModel(repositories, person.id.value)
            viewModel.state.first { it.phase != PeoplePhase.Loading }

            viewModel.onDismissSuggestion(suggestion)
            val tags = repositories.metadata.tags(assetId).first()

            assertFalse(tags.ai.any { it.addId.counter == suggestion.counter && it.tag == suggestion.tag })
        }

    /**
     * The first model suggestion anywhere in the fixture.
     *
     * Found rather than fabricated: the seeded library places suggestions sparsely, and a
     * hand-built one would test the view model against a shape the repository never produces.
     */
    private suspend fun firstSuggestion(repositories: MockRepositories): AiSuggestion {
        val window = repositories.timeline.timelineWindow(0, WINDOW)
        val tags = window.associate { it.id to repositories.metadata.tags(it.id).first() }
        return requireNotNull(
            PeopleProjection.suggestions(window.map(Asset::id), tags, limit = 1).firstOrNull(),
        ) { "the fixture holds no current model suggestions" }
    }

    private companion object {
        const val WINDOW = 200
    }
}
