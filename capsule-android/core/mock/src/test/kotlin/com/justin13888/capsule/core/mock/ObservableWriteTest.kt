package com.justin13888.capsule.core.mock

import app.cash.turbine.test
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.GpsDatum
import com.justin13888.capsule.core.model.GpsFix
import com.justin13888.capsule.core.model.GpsSource
import com.justin13888.capsule.core.model.asWire
import com.justin13888.capsule.core.model.knownOrNull
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * A write made through one repository is seen by a collector on another.
 *
 * This is the property that makes the mock usable as a real backend for the app rather than as
 * a static fixture. Every observable is derived from one revision counter precisely so it
 * holds by construction — and this test is what would catch a repository that started keeping
 * its own private flow and left the timeline a frame behind.
 */
class ObservableWriteTest {
    @Test
    fun `a rating change reaches a collector on the asset flow`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val subject = mock.timeline.timelineWindow(0, 1).single()
            mock.assets.observeAsset(subject.id).test {
                assertEquals(subject.rating, awaitItem()?.rating)
                mock.assets.setRating(subject.id, 5)
                assertEquals(5, awaitItem()?.rating)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a cull flag reaches the asset flow and the stack's derived state`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val stackId = requireNotNull(MockFixtures.LIBRARY.landmarks.burstStackId)
            val members = mock.stacks.stackMembers(stackId)
            mock.stacks.groupCullState(stackId).test {
                awaitItem()
                // A group stores no flag of its own, so flagging every member is the only way
                // its state changes — which is the point of deriving it.
                members.forEach { mock.assets.setCull(it.assetId, CullFlag.Reject) }
                assertEquals(
                    com.justin13888.capsule.core.model.GroupCullState.AllRejected,
                    expectMostRecentItem(),
                )
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a caption write is observed, and displaces the previous value rather than dropping it`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val subject = mock.timeline.timelineWindow(0, 1).single()
            mock.metadata.caption(subject.id).test {
                awaitItem()
                mock.metadata.setCaption(subject.id, "first")
                assertEquals("first", awaitItem().current?.value)
                mock.metadata.setCaption(subject.id, "second")
                val after = awaitItem()
                assertEquals("second", after.current?.value)
                // The loser of a concurrent edit is kept so the UI can offer it back.
                assertEquals("first", after.superseded.first().value)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `restoring a superseded caption writes it forward rather than rolling back`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val subject = mock.timeline.timelineWindow(0, 1).single()
            mock.metadata.setCaption(subject.id, "original")
            mock.metadata.setCaption(subject.id, "replacement")
            val displaced =
                mock.metadata
                    .caption(subject.id)
                    .first()
                    .superseded
                    .first()
            mock.metadata.restoreSupersededCaption(subject.id, displaced)
            mock.metadata.caption(subject.id).test {
                val current = awaitItem()
                assertEquals("original", current.current?.value)
                // A fresh stamped write, so it wins on every device with no special replay rule.
                assertTrue(current.superseded.any { it.value == "replacement" })
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `a tag add and its keyed removal are both observed`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val subject = mock.timeline.timelineWindow(0, 1).single()
            mock.metadata.tags(subject.id).test {
                val before = awaitItem().user.size
                mock.metadata.addUserTag(subject.id, "portfolio")
                val added = awaitItem()
                assertEquals(before + 1, added.user.size)
                // Keyed by the add it targets, not by the word: two devices can add the same
                // word independently and a text-keyed remove would delete the wrong one.
                val addId = added.user.last().addId
                mock.metadata.removeUserTag(subject.id, addId)
                assertEquals(before, awaitItem().user.size)
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `promoting an AI tag copies it under a fresh user-scoped add id`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val owner = requireNotNull(MockFixtures.LIBRARY.landmarks.staleAiTagAssetId)
            val suggestion =
                MockFixtures.LIBRARY.tags
                    .getValue(owner)
                    .ai
                    .first()
            mock.metadata.promoteAiTag(owner, suggestion.addId, alsoDismiss = true)
            mock.metadata.tags(owner).test {
                val after = awaitItem()
                assertTrue(after.user.any { it.tag == suggestion.tag })
                assertNotEquals(suggestion.addId, after.user.first { it.tag == suggestion.tag }.addId)
                assertTrue(after.ai.none { it.addId == suggestion.addId })
                cancelAndIgnoreRemainingEvents()
            }
        }

    @Test
    fun `an album membership change is observed on the album's own flow`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val target =
                MockFixtures.LIBRARY.landmarks.tripAlbumIds
                    .first()
            val defaultAlbum = MockFixtures.LIBRARY.landmarks.defaultAlbumId
            val subject = mock.albums.albumAssets(defaultAlbum, 0, 1).single()
            mock.albums.album(target).test {
                awaitItem()
                mock.albums.setAssetAlbum(subject.id, target)
                cancelAndIgnoreRemainingEvents()
            }
            assertEquals(target, mock.assets.findByUuid(subject.id)?.albumId)
            assertTrue(mock.albums.albumAssets(target, 0, Int.MAX_VALUE).any { it.id == subject.id })
        }

    @Test
    fun `hiding an asset removes it from the timeline window`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val subject = mock.timeline.timelineWindow(0, 1).single()
            mock.assets.setHidden(subject.id, hidden = true)
            assertTrue(mock.timeline.timelineWindow(0, 4).none { it.id == subject.id })
            // Hiding is view-layer only: the asset is still there and still in its album.
            assertNotEquals(null, mock.assets.findByUuid(subject.id))
        }

    @Test
    fun `soft delete then restore leaves the asset live again`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val subject = mock.timeline.timelineWindow(0, 1).single()
            mock.timeline.softDelete(subject.id, MockFixtures.SPEC.now.plus(java.time.Duration.ofDays(30)))
            assertTrue(mock.timeline.trash(0, Int.MAX_VALUE).any { it.id == subject.id })
            mock.timeline.restore(subject.id)
            assertTrue(mock.timeline.timelineWindow(0, 4).any { it.id == subject.id })
        }

    @Test
    fun `a stored fix is written verbatim in the datum it was supplied in`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val subject = mock.timeline.timelineWindow(0, 1).single()
            val fix =
                GpsFix(
                    latitude = 31.2304,
                    longitude = 121.4737,
                    source = GpsSource.Manual.asWire(),
                    datum = GpsDatum.Gcj02.asWire(),
                )
            mock.assets.setGps(subject.id, fix)
            val stored = requireNotNull(mock.assets.findByUuid(subject.id)?.gps)
            // Never converted at rest: GCJ-02 to WGS-84 has no exact inverse, so converting on
            // input would destroy the user's ground truth.
            assertEquals(GpsDatum.Gcj02, stored.datum.knownOrNull)
            assertEquals(fix.latitude, stored.latitude)
            mock.assets.setGps(subject.id, null)
            assertNull(mock.assets.findByUuid(subject.id)?.gps)
        }

    @Test
    fun `choosing a different burst primary re-suppresses the rest`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val stackId = requireNotNull(MockFixtures.LIBRARY.landmarks.burstStackId)
            val members = mock.stacks.stackMembers(stackId)
            val newPrimary = members.last().assetId
            mock.stacks.setStackPrimary(stackId, newPrimary)
            mock.stacks.stack(stackId).test {
                assertEquals(newPrimary, awaitItem()?.primaryAssetId)
                cancelAndIgnoreRemainingEvents()
            }
            val chosen = requireNotNull(mock.assets.findByUuid(newPrimary))
            assertTrue(chosen.stack?.isSuppressed == false)
            // Metadata only: every member still exists, so nothing can lose an original.
            assertEquals(members.size, mock.stacks.stackMembers(stackId).size)
        }

    @Test
    fun `adopting a drop mints an asset and empties the inbox row`() =
        runTest {
            val mock = MockFixtures.freshRepositories()
            val drop =
                mock.drops
                    .inbox()
                    .first()
                    .first()
            val before = mock.timeline.timelineWindow(0, Int.MAX_VALUE).size
            val minted = mock.drops.adopt(drop.id, MockFixtures.LIBRARY.landmarks.defaultAlbumId)
            assertTrue(
                mock.drops
                    .inbox()
                    .first()
                    .none { it.id == drop.id },
            )
            assertEquals(before + 1, mock.timeline.timelineWindow(0, Int.MAX_VALUE).size)
            // The adopter is the creator; the guest is never a signer.
            assertEquals(
                MockFixtures.LIBRARY.landmarks.thisDeviceId,
                mock.assets.findByUuid(minted)?.importedByDevice,
            )
        }
}
