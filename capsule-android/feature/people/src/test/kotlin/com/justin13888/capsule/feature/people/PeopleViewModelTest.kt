package com.justin13888.capsule.feature.people

import com.justin13888.capsule.core.data.ModerationError
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
 * The People hub, driven through the seeded fakes rather than hand-rolled doubles.
 *
 * `UnconfinedTestDispatcher` on `Dispatchers.Main`, because the view model does its work in
 * `viewModelScope` and every case here is about the state that work *settles* on rather than
 * about the order of the intermediate frames.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class PeopleViewModelTest {
    @BeforeEach
    fun installMainDispatcher() {
        Dispatchers.setMain(UnconfinedTestDispatcher())
    }

    @AfterEach
    fun removeMainDispatcher() {
        Dispatchers.resetMain()
    }

    @Test
    fun `it opens on the people the user named, the groupings they have not, and the built albums`() =
        runTest {
            val viewModel = PeopleFixtures.hubViewModel(PeopleFixtures.repositories())

            val state = viewModel.state.first { it.phase != PeoplePhase.Loading }

            assertEquals(PeoplePhase.Content, state.phase)
            assertTrue(state.people.isNotEmpty())
            assertTrue(state.clusters.isNotEmpty())
            assertEquals(SmartAlbumKind.entries.toList(), state.smartAlbums.map(SmartAlbumCard::kind))
        }

    @Test
    fun `groupings from a replaced model version are surfaced, not hidden`() =
        runTest {
            val viewModel = PeopleFixtures.hubViewModel(PeopleFixtures.repositories())

            val state = viewModel.state.first { it.phase != PeoplePhase.Loading }

            val notice = requireNotNull(state.stale)
            assertTrue(notice.staleCount > 0)
            assertTrue(notice.slots.isNotEmpty())
            assertTrue(state.clusters.any(ClusterCard::isStale), "a stale grouping vanished from the hub")
        }

    @Test
    fun `attaching a grouping to somebody already named moves it off the unnamed list`() =
        runTest {
            val viewModel = PeopleFixtures.hubViewModel(PeopleFixtures.repositories())
            val opened = viewModel.state.first { it.phase != PeoplePhase.Loading }
            val cluster = opened.clusters.first()
            val person = opened.people.first()

            viewModel.onNameRequested(NameTarget(personId = null, clusterId = cluster.clusterId, initialName = ""))
            viewModel.onAttachCluster(person.personId)
            val after = viewModel.state.value

            assertTrue(after.clusters.none { it.clusterId == cluster.clusterId })
            assertTrue(after.people.first { it.personId == person.personId }.photoCount > person.photoCount)
            assertNull(after.nameTarget)
        }

    @Test
    fun `renaming a person keeps the name the user typed and nothing else`() =
        runTest {
            val viewModel = PeopleFixtures.hubViewModel(PeopleFixtures.repositories())
            val opened = viewModel.state.first { it.phase != PeoplePhase.Loading }
            val person = opened.people.first()

            viewModel.onNameRequested(
                NameTarget(personId = person.personId, clusterId = null, initialName = person.displayName),
            )
            viewModel.onNameSubmit("  Renamed  ")
            val after = viewModel.state.value

            val renamed = after.people.first { it.personId == person.personId }
            assertEquals("Renamed", renamed.displayName)
            assertEquals(person.photoCount, renamed.photoCount)
        }

    @Test
    fun `merging moves every grouping onto the target and deletes nothing`() =
        runTest {
            val viewModel = PeopleFixtures.hubViewModel(PeopleFixtures.repositories())
            val opened = viewModel.state.first { it.phase != PeoplePhase.Loading }
            val source = opened.people[0]
            val target = opened.people[1]

            viewModel.onMergeOpen(source)
            viewModel.onMergeInto(target.personId)
            val after = viewModel.state.value

            assertEquals(opened.people.size - 1, after.people.size)
            assertEquals(
                source.photoCount + target.photoCount,
                after.people.first { it.personId == target.personId }.photoCount,
            )
        }

    @Test
    fun `hiding a person offers an undo that brings them back`() =
        runTest {
            val viewModel = PeopleFixtures.hubViewModel(PeopleFixtures.repositories())
            val opened = viewModel.state.first { it.phase != PeoplePhase.Loading }
            val person = opened.people.first()

            viewModel.onHide(person)
            val hidden = viewModel.state.value
            assertTrue(hidden.people.none { it.personId == person.personId })
            assertEquals(person.displayName, requireNotNull(hidden.hidden).displayName)

            viewModel.onUndoHide()
            val restored = viewModel.state.value

            assertTrue(restored.people.any { it.personId == person.personId })
            assertNull(restored.hidden)
        }

    @Test
    fun `opening a built album needs no query, and closing it gives the space back`() =
        runTest {
            val viewModel = PeopleFixtures.hubViewModel(PeopleFixtures.repositories())
            val opened = viewModel.state.first { it.phase != PeoplePhase.Loading }
            val populated = opened.smartAlbums.first { it.photoCount > 0 }

            viewModel.onOpenSmartAlbum(populated.kind)
            val expanded = viewModel.state.value
            assertEquals(populated.kind, expanded.openedSmartAlbum)
            assertEquals(populated.photoCount, expanded.openedSmartAlbumPhotos.size)

            viewModel.onCloseSmartAlbum()

            assertNull(viewModel.state.value.openedSmartAlbum)
            assertTrue(
                viewModel.state.value.openedSmartAlbumPhotos
                    .isEmpty(),
            )
        }

    @Test
    fun `a refused load surfaces the catalogue code, never a status`() =
        runTest {
            val scenario = MockScenario.Default.failing(MockOperation.TimelineRead, ProtocolError.VersionUnsupported)
            val viewModel = PeopleFixtures.hubViewModel(PeopleFixtures.repositories(scenario))

            val state = viewModel.state.first { it.phase != PeoplePhase.Loading }

            assertEquals(PeoplePhase.Failed, state.phase)
            assertEquals(PeopleFailure.Unsupported, state.failure)
        }

    @Test
    fun `retrying once the condition clears reloads the hub`() =
        runTest {
            val repositories =
                PeopleFixtures.repositories(
                    MockScenario.Default.failing(MockOperation.TimelineRead, ProtocolError.VersionUnsupported),
                )
            val viewModel = PeopleFixtures.hubViewModel(repositories)
            viewModel.state.first { it.phase == PeoplePhase.Failed }

            repositories.applyScenario(MockScenario.Default)
            viewModel.onRetry()
            val state = viewModel.state.first { it.phase != PeoplePhase.Loading }

            assertEquals(PeoplePhase.Content, state.phase)
            assertNull(state.failure)
        }

    @Test
    fun `a refused write is reported in place rather than taking the hub away`() =
        runTest {
            val repositories = PeopleFixtures.repositories()
            val viewModel = PeopleFixtures.hubViewModel(repositories)
            val opened = viewModel.state.first { it.phase != PeoplePhase.Loading }
            repositories.applyScenario(
                MockScenario.Default.failing(MockOperation.PeopleWrite, ModerationError.AccountSuspended),
            )

            viewModel.onNameRequested(
                NameTarget(personId = opened.people.first().personId, clusterId = null, initialName = ""),
            )
            viewModel.onNameSubmit("Anything")
            val after = viewModel.state.value

            assertEquals(PeoplePhase.Content, after.phase)
            assertEquals(PeopleFailure.Suspended, after.actionFailure)
            assertTrue(after.people.isNotEmpty(), "the hub blanked on a failed action")

            viewModel.onDismissActionFailure()
            assertNull(viewModel.state.value.actionFailure)
        }

    @Test
    fun `an empty name is not a rename`() =
        runTest {
            val viewModel = PeopleFixtures.hubViewModel(PeopleFixtures.repositories())
            val opened = viewModel.state.first { it.phase != PeoplePhase.Loading }
            val person = opened.people.first()

            viewModel.onNameRequested(
                NameTarget(personId = person.personId, clusterId = null, initialName = person.displayName),
            )
            viewModel.onNameSubmit("   ")

            assertEquals(
                person.displayName,
                viewModel.state.value.people
                    .first()
                    .displayName,
            )
        }
}
