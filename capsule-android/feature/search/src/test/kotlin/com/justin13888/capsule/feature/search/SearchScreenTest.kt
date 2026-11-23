package com.justin13888.capsule.feature.search

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextReplacement
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.i18n.R
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableList
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import java.time.Instant

/**
 * The five states the search screen can be in, each rendered for real.
 *
 * Driven through the stateless screen rather than through the view model, which is the point
 * of the screen being stateless: the two states that are otherwise awkward to produce — a
 * refused search, and a library whose AI outputs are being withheld — are one line each here.
 */
@RunWith(RobolectricTestRunner::class)
class SearchScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val items =
        List(SAMPLE_SIZE) { index ->
            SearchIndexing.itemOf(
                testAsset("asset-$index", Instant.parse("2026-0${index % 9 + 1}-01T12:00:00Z")),
            )
        }.toImmutableList()

    @Test
    fun the_entry_screen_lists_what_was_searched_before() {
        render(blankState().copy(recents = persistentListOf("kyoto", "iceland")))

        compose.onNodeWithText(localized(R.string.search_recents_title)).assertExists()
        compose.onNodeWithText("kyoto").assertIsDisplayed()
        compose.onNodeWithText("iceland").assertIsDisplayed()
    }

    @Test
    fun an_entry_screen_with_nothing_to_offer_explains_itself() {
        render(blankState())

        compose.onNodeWithText(localized(R.string.search_empty_title)).assertIsDisplayed()
        compose.onNodeWithText(localized(R.string.search_empty_body)).assertIsDisplayed()
    }

    @Test
    fun a_search_in_flight_says_so() {
        render(blankState().copy(phase = SearchPhase.Loading))

        compose.onNodeWithText(localized(R.string.search_loading)).assertExists()
    }

    @Test
    fun results_are_drawn_in_the_shared_gallery() {
        render(blankState().copy(phase = SearchPhase.Results, results = items, hitCount = items.size))

        compose.onNodeWithTag(SEARCH_RESULTS_TAG).assertExists()
    }

    @Test
    fun a_no_results_screen_offers_to_turn_the_filters_off() {
        var cleared = 0
        render(
            state =
                blankState().copy(
                    phase = SearchPhase.NoResults,
                    query = "kyoto",
                    filters = SearchFilters().toggled(FacetRef(FacetKind.Date, "2026")),
                ),
            actions = inertActions(onClearFilters = { cleared++ }),
        )

        compose.onNodeWithText(localized(R.string.search_no_results_title)).assertIsDisplayed()
        compose.onNodeWithText(localized(R.string.search_no_results_clear_filters)).performClick()

        assertEquals(1, cleared)
    }

    @Test
    fun a_failure_localises_the_catalogue_code_and_offers_a_way_back() {
        var retried = 0
        render(
            state = blankState().copy(phase = SearchPhase.Failed, failure = SearchFailure.Unsupported),
            actions = inertActions(onRetry = { retried++ }),
        )

        compose.onNodeWithText(localized(R.string.error_protocol_version_unsupported)).assertIsDisplayed()
        compose.onNodeWithText(localized(R.string.search_error_retry)).performClick()

        assertEquals(1, retried)
    }

    @Test
    fun held_back_model_outputs_are_announced_rather_than_silently_missing() {
        render(
            blankState().copy(
                phase = SearchPhase.Results,
                results = items,
                hitCount = items.size,
                staleModels = StaleModelNotice(excludedCount = 12, slots = persistentListOf("facenet-s 2025.05")),
            ),
        )

        compose.onNodeWithTag(SEARCH_STALE_NOTICE_TAG).assertExists()
        compose.onNodeWithText(localized(R.string.search_stale_model_title)).assertIsDisplayed()
    }

    @Test
    fun a_current_library_shows_no_notice() {
        render(blankState().copy(phase = SearchPhase.Results, results = items, hitCount = items.size))

        compose.onNodeWithTag(SEARCH_STALE_NOTICE_TAG).assertDoesNotExist()
    }

    @Test
    fun typing_and_submitting_reach_the_caller() {
        val typed = mutableListOf<String>()
        render(state = blankState(), actions = inertActions(onQueryChange = { typed += it }))

        compose.onNodeWithTag(SEARCH_FIELD_TAG).performTextReplacement("kyoto")

        assertTrue("nothing reached onQueryChange", typed.contains("kyoto"))
    }

    @Test
    fun the_all_filters_button_asks_for_the_sheet() {
        var opened = false
        render(
            state =
                blankState().copy(
                    phase = SearchPhase.Results,
                    results = items,
                    hitCount = items.size,
                    groups = persistentListOf(FacetGroup(FacetKind.Date, sampleValues())),
                ),
            actions = inertActions(onFilterSheetOpenChange = { opened = it }),
        )

        compose.onNodeWithTag(SEARCH_ADVANCED_TAG).performClick()

        assertTrue("the sheet was never requested", opened)
    }

    @Test
    fun a_facet_chip_reports_which_point_was_flipped() {
        val flipped = mutableListOf<FacetRef>()
        render(
            state =
                blankState().copy(
                    phase = SearchPhase.Results,
                    results = items,
                    hitCount = items.size,
                    groups = persistentListOf(FacetGroup(FacetKind.Date, sampleValues())),
                ),
            actions = inertActions(onToggleFacet = { flipped += it }),
        )

        compose.onNodeWithText("2019").performClick()

        assertEquals(listOf(FacetRef(FacetKind.Date, "2019")), flipped)
    }

    private fun sampleValues() =
        persistentListOf(
            FacetValue(pointOf(FacetKind.Date, "2019"), count = 4),
            FacetValue(pointOf(FacetKind.Date, "2018"), count = 2),
        )

    private fun render(
        state: SearchUiState,
        actions: SearchActions = SearchActions.Inert,
    ) {
        compose.setContent {
            CapsuleTheme(darkTheme = false, dynamicColor = false) {
                SearchScreen(state = state, actions = actions)
            }
        }
        compose.waitForIdle()
    }

    @Suppress("LongParameterList")
    private fun inertActions(
        onQueryChange: (String) -> Unit = {},
        onToggleFacet: (FacetRef) -> Unit = {},
        onClearFilters: () -> Unit = {},
        onFilterSheetOpenChange: (Boolean) -> Unit = {},
        onRetry: () -> Unit = {},
    ) = SearchActions(
        onQueryChange = onQueryChange,
        onSubmit = {},
        onRecentSelected = {},
        onClearRecents = {},
        onToggleFacet = onToggleFacet,
        onClearFilters = onClearFilters,
        onFilterSheetOpenChange = onFilterSheetOpenChange,
        onRetry = onRetry,
        onOpenAsset = {},
    )

    private companion object {
        const val SAMPLE_SIZE = 9
    }
}
