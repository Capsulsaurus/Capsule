package com.justin13888.capsule.feature.search

import app.cash.turbine.test
import com.justin13888.capsule.core.data.ProtocolError
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockScenario
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.BeforeEach
import org.junit.jupiter.api.Test

/**
 * The search state machine, driven through the seeded fakes rather than hand-rolled doubles.
 *
 * `UnconfinedTestDispatcher` on `Dispatchers.Main`, because the view model does its work in
 * `viewModelScope` and every case here is about the state that work *settles* on rather than
 * about the order of the intermediate frames.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class SearchViewModelTest {
    @BeforeEach
    fun installMainDispatcher() {
        Dispatchers.setMain(UnconfinedTestDispatcher())
    }

    @AfterEach
    fun removeMainDispatcher() {
        Dispatchers.resetMain()
    }

    @Test
    fun `it opens on the entry screen with somewhere to start from`() =
        runTest {
            val viewModel = SearchFixtures.viewModel(SearchFixtures.repositories())

            val state = viewModel.state.first { it.phase != SearchPhase.Loading }

            assertEquals(SearchPhase.Entry, state.phase)
            assertTrue(state.suggested.isNotEmpty(), "the entry screen offers no facets to start from")
            assertTrue(state.groups.isNotEmpty())
        }

    @Test
    fun `a library holding superseded model outputs says so before any query runs`() =
        runTest {
            val viewModel = SearchFixtures.viewModel(SearchFixtures.repositories())

            val state = viewModel.state.first { it.phase != SearchPhase.Loading }

            val notice = requireNotNull(state.staleModels)
            assertTrue(notice.excludedCount > 0)
            assertTrue(notice.slots.isNotEmpty())
        }

    @Test
    fun `a query that matches produces results and is remembered`() =
        runTest {
            val viewModel = SearchFixtures.viewModel(SearchFixtures.repositories())
            viewModel.state.first { it.phase != SearchPhase.Loading }

            viewModel.onQueryChange(MATCHING_QUERY)
            viewModel.onSubmit()
            val state = viewModel.state.first { it.phase != SearchPhase.Loading }

            assertEquals(SearchPhase.Results, state.phase)
            assertTrue(state.results.isNotEmpty())
            assertEquals(listOf(MATCHING_QUERY), state.recents)
        }

    @Test
    fun `a query that matches nothing is a no-results screen, not an empty one`() =
        runTest {
            val viewModel = SearchFixtures.viewModel(SearchFixtures.repositories())
            viewModel.state.first { it.phase != SearchPhase.Loading }

            viewModel.onQueryChange("qwertyuiop-no-such-thing")
            viewModel.onSubmit()
            val state = viewModel.state.first { it.phase != SearchPhase.Loading }

            assertEquals(SearchPhase.NoResults, state.phase)
            assertTrue(state.results.isEmpty())
        }

    @Test
    fun `a facet narrows the answer without asking the repository again`() =
        runTest {
            val viewModel = SearchFixtures.viewModel(SearchFixtures.repositories())
            val opened = viewModel.state.first { it.phase != SearchPhase.Loading }
            val facet =
                opened.groups
                    .first { it.kind == FacetKind.Date }
                    .values
                    .first()

            viewModel.onToggleFacet(facet.ref)
            val narrowed = viewModel.state.value

            assertEquals(facet.count, narrowed.results.size)
            assertEquals(opened.hitCount, narrowed.hitCount, "counts must stay over the unfiltered hits")
            assertTrue(narrowed.phase == SearchPhase.Results)
        }

    @Test
    fun `clearing the filters puts the entry screen back`() =
        runTest {
            val viewModel = SearchFixtures.viewModel(SearchFixtures.repositories())
            val opened = viewModel.state.first { it.phase != SearchPhase.Loading }

            viewModel.onToggleFacet(
                opened.groups
                    .first()
                    .values
                    .first()
                    .ref,
            )
            viewModel.onClearFilters()

            assertEquals(SearchPhase.Entry, viewModel.state.value.phase)
            assertTrue(viewModel.state.value.filters.isEmpty)
        }

    @Test
    fun `a refused search surfaces the catalogue code, never a status`() =
        runTest {
            val scenario = MockScenario.Default.failing(MockOperation.Search, ProtocolError.VersionUnsupported)
            val viewModel = SearchFixtures.viewModel(SearchFixtures.repositories(scenario))
            viewModel.state.first { it.phase != SearchPhase.Loading }

            viewModel.onQueryChange(MATCHING_QUERY)
            viewModel.onSubmit()
            val state = viewModel.state.first { it.phase != SearchPhase.Loading }

            assertEquals(SearchPhase.Failed, state.phase)
            assertEquals(SearchFailure.Unsupported, state.failure)
            assertTrue(state.results.isEmpty())
        }

    @Test
    fun `retrying once the condition clears succeeds`() =
        runTest {
            val repositories = SearchFixtures.repositories()
            repositories.applyScenario(
                MockScenario.Default.failing(MockOperation.Search, ProtocolError.VersionUnsupported),
            )
            val viewModel = SearchFixtures.viewModel(repositories)
            viewModel.state.first { it.phase != SearchPhase.Loading }
            viewModel.onQueryChange(MATCHING_QUERY)
            viewModel.onSubmit()
            viewModel.state.first { it.phase == SearchPhase.Failed }

            repositories.applyScenario(MockScenario.Default)
            viewModel.onRetry()
            val state = viewModel.state.first { it.phase != SearchPhase.Loading }

            assertEquals(SearchPhase.Results, state.phase)
            assertNull(state.failure)
        }

    @Test
    fun `the sheet flag is the only thing an open or close changes`() =
        runTest {
            val viewModel = SearchFixtures.viewModel(SearchFixtures.repositories())
            viewModel.state.first { it.phase != SearchPhase.Loading }

            viewModel.state.test {
                val before = awaitItem()
                viewModel.onFilterSheetOpenChange(true)
                val after = awaitItem()

                assertTrue(after.filterSheetOpen)
                assertEquals(before, after.copy(filterSheetOpen = false))
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `recent searches can be forgotten`() =
        runTest {
            val viewModel = SearchFixtures.viewModel(SearchFixtures.repositories())
            viewModel.state.first { it.phase != SearchPhase.Loading }
            viewModel.onQueryChange(MATCHING_QUERY)
            viewModel.onSubmit()
            viewModel.state.first { it.phase != SearchPhase.Loading }

            viewModel.onClearRecents()

            assertTrue(
                viewModel.state.value.recents
                    .isEmpty(),
            )
        }

    private companion object {
        /** A word the seeded library's AI tagger actually produced, so the query has hits. */
        const val MATCHING_QUERY = "beach"
    }
}
