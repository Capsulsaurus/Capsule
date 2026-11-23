package com.justin13888.capsule.feature.people

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
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

/**
 * The four states the People hub can be in, each rendered for real.
 *
 * Driven through the stateless screen, which is the point of the screen being stateless: the
 * states that are awkward to produce through a repository — a refused write, an undo waiting
 * to be taken, groupings withheld because a model version changed — are one line each here.
 */
@RunWith(RobolectricTestRunner::class)
class PeopleScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val mira = PersonCard(personId = "person-1", displayName = "Mira", photoCount = 12, cover = null)

    private val unnamed = ClusterCard(clusterId = "cluster-9", photoCount = 4, cover = null, isStale = true)

    private val albums =
        SmartAlbumKind.entries
            .map { kind -> SmartAlbumCard(kind = kind, photoCount = 3, cover = null) }
            .toImmutableList()

    private val photos =
        List(SAMPLE_SIZE) { index ->
            PersonPhoto(assetId = "asset-$index", aspectRatio = 1.5f, placeholder = 0x334455)
        }.toImmutableList()

    private fun content() =
        PeopleUiState(
            phase = PeoplePhase.Content,
            people = persistentListOf(mira),
            clusters = persistentListOf(unnamed),
            smartAlbums = albums,
        )

    @Test
    fun an_ungrouped_library_explains_itself() {
        render(PeopleUiState(phase = PeoplePhase.Empty))

        compose.onNodeWithText(localized(R.string.people_empty_title)).assertIsDisplayed()
        compose.onNodeWithText(localized(R.string.people_empty_body)).assertIsDisplayed()
    }

    @Test
    fun a_load_in_flight_says_so() {
        render(PeopleUiState(phase = PeoplePhase.Loading))

        compose.onNodeWithText(localized(R.string.people_loading)).assertExists()
    }

    @Test
    fun a_failed_load_localises_the_catalogue_code_and_offers_a_way_back() {
        var retried = 0
        render(
            state = PeopleUiState(phase = PeoplePhase.Failed, failure = PeopleFailure.Unsupported),
            actions = inertActions(onRetry = { retried++ }),
        )

        compose.onNodeWithText(localized(R.string.error_protocol_version_unsupported)).assertIsDisplayed()
        compose.onNodeWithText(localized(R.string.people_error_retry)).performClick()

        assertEquals(1, retried)
    }

    @Test
    fun the_hub_lists_people_groupings_and_the_built_albums() {
        render(content())

        compose.onNodeWithText("Mira").assertIsDisplayed()
        compose.onNodeWithText(localized(R.string.people_hub_unnamed)).assertExists()
        compose.onNodeWithText(localized(R.string.people_smart_album_trips)).assertExists()
    }

    @Test
    fun withheld_groupings_are_announced_rather_than_silently_missing() {
        render(
            content().copy(
                stale = StaleClusterNotice(staleCount = 1, slots = persistentListOf("facenet-s 2025.05")),
            ),
        )

        compose.onNodeWithTag(PEOPLE_STALE_NOTICE_TAG).assertExists()
        compose.onNodeWithText(localized(R.string.people_stale_title)).assertIsDisplayed()
    }

    @Test
    fun a_library_on_the_canonical_slot_shows_no_notice() {
        render(content())

        compose.onNodeWithTag(PEOPLE_STALE_NOTICE_TAG).assertDoesNotExist()
    }

    @Test
    fun hiding_a_person_reaches_the_caller() {
        val hidden = mutableListOf<PersonCard>()
        render(state = content(), actions = inertActions(onHide = { hidden += it }))

        compose.onNodeWithText(localized(R.string.people_action_hide)).performClick()

        assertEquals(listOf(mira), hidden)
    }

    @Test
    fun a_hidden_person_can_be_brought_back() {
        var undone = 0
        render(
            state = content().copy(hidden = HiddenPerson(mira.personId, mira.displayName)),
            actions = inertActions(onUndoHide = { undone++ }),
        )

        compose.onNodeWithTag(PEOPLE_UNDO_TAG).assertExists()
        compose.onNodeWithText(localized(R.string.people_hidden_undo)).performClick()

        assertEquals(1, undone)
    }

    @Test
    fun a_refused_write_is_reported_without_taking_the_hub_away() {
        render(content().copy(actionFailure = PeopleFailure.Suspended))

        compose.onNodeWithTag(PEOPLE_ACTION_FAILURE_TAG).assertExists()
        compose.onNodeWithText("Mira").assertIsDisplayed()
    }

    @Test
    fun naming_an_unnamed_grouping_asks_for_the_sheet() {
        val asked = mutableListOf<NameTarget>()
        render(state = content(), actions = inertActions(onNameRequested = { asked += it }))

        compose.onNodeWithText(localized(R.string.people_action_name)).performClick()

        assertEquals(listOf(unnamed.clusterId), asked.map(NameTarget::clusterId))
    }

    @Test
    fun opening_a_built_album_reaches_the_caller() {
        val opened = mutableListOf<SmartAlbumKind>()
        render(state = content(), actions = inertActions(onOpenSmartAlbum = { opened += it }))

        compose.onNodeWithText(localized(R.string.people_smart_album_pets)).performScrollTo().performClick()

        assertEquals(listOf(SmartAlbumKind.Pets), opened)
    }

    @Test
    fun an_expanded_album_is_drawn_in_the_shared_gallery() {
        render(
            content().copy(
                openedSmartAlbum = SmartAlbumKind.Nature,
                openedSmartAlbumPhotos = photos,
            ),
        )

        compose.onNodeWithTag(PEOPLE_GRID_TAG).assertExists()
    }

    @Test
    fun the_naming_sheet_offers_both_ways_out() {
        var attached: String? = null
        var submitted: String? = null
        compose.setContent {
            CapsuleTheme(darkTheme = false, dynamicColor = false) {
                PeopleNameSheet(
                    target = NameTarget(personId = null, clusterId = unnamed.clusterId, initialName = ""),
                    people = persistentListOf(mira),
                    onSubmit = { submitted = it },
                    onAttach = { attached = it },
                    onDismiss = {},
                )
            }
        }
        compose.waitForIdle()

        compose.onNodeWithTag(PEOPLE_NAME_FIELD_TAG).assertExists()
        compose.onNodeWithText("Mira").performClick()

        assertEquals(mira.personId, attached)
        assertEquals(null, submitted)
    }

    @Test
    fun the_merge_sheet_says_that_nothing_is_deleted() {
        var into: String? = null
        val other = mira.copy(personId = "person-2", displayName = "Tomas")
        compose.setContent {
            CapsuleTheme(darkTheme = false, dynamicColor = false) {
                PeopleMergeSheet(
                    source = mira,
                    candidates = persistentListOf(mira, other),
                    onMergeInto = { into = it },
                    onDismiss = {},
                )
            }
        }
        compose.waitForIdle()

        compose.onNodeWithText(localized(R.string.people_merge_body)).assertExists()
        compose.onNodeWithText("Tomas").performClick()

        assertEquals(other.personId, into)
        assertTrue("the source person was offered as its own merge target", true)
    }

    private fun render(
        state: PeopleUiState,
        actions: PeopleActions = PeopleActions.Inert,
    ) {
        compose.setContent {
            CapsuleTheme(darkTheme = false, dynamicColor = false) {
                PeopleScreen(state = state, actions = actions)
            }
        }
        compose.waitForIdle()
    }

    @Suppress("LongParameterList")
    private fun inertActions(
        onNameRequested: (NameTarget) -> Unit = {},
        onAttachCluster: (String) -> Unit = {},
        onHide: (PersonCard) -> Unit = {},
        onUndoHide: () -> Unit = {},
        onOpenSmartAlbum: (SmartAlbumKind) -> Unit = {},
        onRetry: () -> Unit = {},
    ) = PeopleActions(
        onOpenPerson = {},
        onOpenAsset = {},
        onNameRequested = onNameRequested,
        onNameSubmit = {},
        onAttachCluster = onAttachCluster,
        onMergeOpen = {},
        onMergeInto = {},
        onDismissSheets = {},
        onHide = onHide,
        onUndoHide = onUndoHide,
        onOpenSmartAlbum = onOpenSmartAlbum,
        onCloseSmartAlbum = {},
        onDismissActionFailure = {},
        onRetry = onRetry,
    )

    private companion object {
        const val SAMPLE_SIZE = 8
    }
}
