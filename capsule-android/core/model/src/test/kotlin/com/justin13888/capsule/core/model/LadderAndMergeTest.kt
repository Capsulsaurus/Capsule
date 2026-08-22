package com.justin13888.capsule.core.model

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Nested
import org.junit.jupiter.api.Test
import java.time.Duration
import java.time.Instant

/**
 * The ordered ladders — representation, upload tier, connection class — and the CRDT merges.
 *
 * Ordering is the load-bearing property in all three: degrade walks *down* the representation
 * ladder, staged upload walks *up* the tier ladder, and a last-writer-wins merge needs a total
 * order or two devices land on different values.
 */
class LadderAndMergeTest {
    @Nested
    inner class RepresentationLadder {
        @Test
        fun `the ladder is ordered cheapest first`() {
            assertEquals(
                listOf(
                    RepresentationTier.Lqip,
                    RepresentationTier.Thumbnail,
                    RepresentationTier.Preview,
                    RepresentationTier.Original,
                ),
                RepresentationTier.entries.sortedBy(RepresentationTier::rank),
            )
        }

        @Test
        fun `ranks are distinct so the ordering is total`() {
            val ranks = RepresentationTier.entries.map(RepresentationTier::rank)
            assertEquals(ranks.size, ranks.toSet().size)
        }

        @Test
        fun `degrading walks down to the best rung actually held`() {
            val held = HeldRepresentations(setOf(RepresentationTier.Lqip, RepresentationTier.Thumbnail))
            assertEquals(RepresentationTier.Thumbnail, held.degradeFrom(RepresentationTier.Original))
            assertEquals(RepresentationTier.Thumbnail, held.degradeFrom(RepresentationTier.Preview))
            assertEquals(RepresentationTier.Thumbnail, held.degradeFrom(RepresentationTier.Thumbnail))
            assertEquals(RepresentationTier.Lqip, held.degradeFrom(RepresentationTier.Lqip))
        }

        @Test
        fun `degrading never climbs above what was asked for`() {
            // Asking for a thumbnail must not hand back the original: the caller asked for
            // the cheap rung for a reason, and a grid that drew originals would be ruinous.
            val everything = HeldRepresentations(RepresentationTier.entries.toSet())
            assertEquals(RepresentationTier.Thumbnail, everything.degradeFrom(RepresentationTier.Thumbnail))
        }

        @Test
        fun `the placeholder is always the floor`() {
            // The ladder can always draw something, which is why a missing derivative never
            // removes an asset from the library.
            val nothing = HeldRepresentations(emptySet())
            assertEquals(RepresentationTier.Lqip, nothing.degradeFrom(RepresentationTier.Original))
            assertEquals(RepresentationTier.Lqip, HeldRepresentations(emptySet()).best)
            assertEquals(RepresentationTier.Lqip, HeldRepresentations.LQIP_ONLY.best)
        }

        @Test
        fun `best reports the highest rung held`() {
            val held = HeldRepresentations(setOf(RepresentationTier.Lqip, RepresentationTier.Preview))
            assertEquals(RepresentationTier.Preview, held.best)
            assertTrue(held.holds(RepresentationTier.Preview))
            assertFalse(held.holds(RepresentationTier.Original))
        }

        @Test
        fun `each sync scope names the highest tier it fetches eagerly`() {
            assertEquals(RepresentationTier.Lqip, SyncScope.MetadataOnly.tier)
            assertEquals(RepresentationTier.Thumbnail, SyncScope.MetadataAndThumbnails.tier)
            assertEquals(RepresentationTier.Original, SyncScope.MetadataThumbnailsAndOriginal.tier)
        }
    }

    @Nested
    inner class UploadLadder {
        @Test
        fun `the tiers are ordered index, preview, original`() {
            assertEquals(
                listOf(UploadTier.Index, UploadTier.Preview, UploadTier.Original),
                UploadTier.entries.sortedBy(UploadTier::rank),
            )
        }

        @Test
        fun `the index tier opens on a link nothing else would use`() {
            // The whole point of staging is that the index escapes the device even on a link
            // too poor for anything else.
            assertEquals(ConnectionClass.Adverse, UploadTier.Index.opensAt)
            assertEquals(ConnectionClass.Metered, UploadTier.Preview.opensAt)
            assertEquals(ConnectionClass.Unmetered, UploadTier.Original.opensAt)
        }

        @Test
        fun `a small reconciliation proceeds on any usable, non-saver link`() {
            assertTrue(ReconciliationSize.Small.mayProceedOn(ConnectionClass.Unmetered))
            assertTrue(ReconciliationSize.Small.mayProceedOn(ConnectionClass.Metered))
            assertFalse(ReconciliationSize.Small.mayProceedOn(ConnectionClass.Constrained))
            assertFalse(ReconciliationSize.Small.mayProceedOn(ConnectionClass.Offline))
        }

        @Test
        fun `a large reconciliation waits for unmetered`() {
            assertTrue(ReconciliationSize.Large.mayProceedOn(ConnectionClass.Unmetered))
            assertFalse(ReconciliationSize.Large.mayProceedOn(ConnectionClass.Metered))
            assertFalse(ReconciliationSize.Large.mayProceedOn(ConnectionClass.Adverse))
        }

        @Test
        fun `offline is never usable and never allows bulk`() {
            assertFalse(ConnectionClass.Offline.isUsable)
            ConnectionClass.entries.filter { it != ConnectionClass.Offline }.forEach {
                assertTrue(it.isUsable, "$it should be usable")
            }
            assertEquals(
                listOf(ConnectionClass.Unmetered),
                ConnectionClass.entries.filter(ConnectionClass::allowsBulk),
            )
        }
    }

    @Nested
    inner class RetryBounds {
        @Test
        fun `each class carries the bounds the design pins`() {
            assertEquals(2, RetryClass.Interactive.policy.maxRetries)
            assertEquals(Duration.ofMillis(200), RetryClass.Interactive.policy.baseDelay)
            assertEquals(8, RetryClass.BulkTransfer.policy.maxRetries)
            assertEquals(Duration.ofSeconds(30), RetryClass.BulkTransfer.policy.maxDelay)
            assertEquals(12, RetryClass.ControlCeremony.policy.maxRetries)
            assertEquals(Duration.ofSeconds(600), RetryClass.ControlCeremony.policy.maxDelay)
        }

        @Test
        fun `the ceiling doubles and then caps`() {
            val policy = RetryPolicy(8, Duration.ofMillis(500), Duration.ofSeconds(30))
            assertEquals(Duration.ofMillis(500), policy.ceilingFor(0))
            assertEquals(Duration.ofSeconds(1), policy.ceilingFor(1))
            assertEquals(Duration.ofSeconds(2), policy.ceilingFor(2))
            assertEquals(Duration.ofSeconds(30), policy.ceilingFor(7))
        }

        @Test
        fun `every retry loop is bounded`() {
            // No surface may hot-loop; the give-up is what turns a retry into a visible state.
            val policy = RetryClass.Interactive.policy
            assertTrue(policy.mayRetry(0))
            assertTrue(policy.mayRetry(1))
            assertFalse(policy.mayRetry(2))
        }

        @Test
        fun `an absurd attempt count still yields the capped ceiling`() {
            assertEquals(Duration.ofSeconds(30), RetryClass.BulkTransfer.policy.ceilingFor(Int.MAX_VALUE))
        }
    }

    @Nested
    inner class LwwMerge {
        @Test
        fun `the later write wins`() {
            val early = Lww("first", LwwStamp(T0, DeviceId("a")))
            val late = Lww("second", LwwStamp(T0.plusSeconds(1), DeviceId("a")))
            assertEquals(late, early.merge(late))
            assertEquals(late, late.merge(early))
        }

        @Test
        fun `an exact timestamp tie breaks on the device id`() {
            val fromA = Lww("a wrote this", LwwStamp(T0, DeviceId("aaa")))
            val fromB = Lww("b wrote this", LwwStamp(T0, DeviceId("bbb")))
            assertEquals(fromB, fromA.merge(fromB))
            assertEquals(fromB, fromB.merge(fromA))
        }

        @Test
        fun `merging is commutative, associative and idempotent`() {
            // Those three properties are what make arrival order irrelevant, which is the
            // entire reason the field is a register rather than a plain value.
            val a = Lww("a", LwwStamp(T0, DeviceId("a")))
            val b = Lww("b", LwwStamp(T0.plusSeconds(5), DeviceId("b")))
            val c = Lww("c", LwwStamp(T0.plusSeconds(2), DeviceId("c")))
            val expected = b
            listOf(
                a.merge(b).merge(c),
                a.merge(c).merge(b),
                b.merge(a).merge(c),
                b.merge(c).merge(a),
                c.merge(a).merge(b),
                c.merge(b).merge(a),
            ).forEach { assertEquals(expected, it) }
            assertEquals(expected, expected.merge(expected))
        }

        @Test
        fun `a stamped absence is an ordinary value and can win`() {
            // Leaving a stack, clearing a caption and deleting a smart album are all this
            // write; if a tombstone could not win, none of them would converge.
            val value = Lww("caption", LwwStamp(T0, DeviceId("a")))
            val cleared = Lww<String>(null, LwwStamp(T0.plusSeconds(1), DeviceId("a")))
            assertEquals(cleared, value.merge(cleared))
            assertTrue(cleared.isTombstone)
            assertFalse(value.isTombstone)
        }

        @Test
        fun `stamps order by time and then by device`() {
            assertTrue(LwwStamp(T0, DeviceId("z")) < LwwStamp(T0.plusMillis(1), DeviceId("a")))
            assertTrue(LwwStamp(T0, DeviceId("a")) < LwwStamp(T0, DeviceId("b")))
            assertEquals(0, LwwStamp(T0, DeviceId("a")).compareTo(LwwStamp(T0, DeviceId("a"))))
        }
    }

    @Nested
    inner class DerivedGroupState {
        @Test
        fun `a group of rejects is all-rejected`() {
            assertEquals(
                GroupCullState.AllRejected,
                deriveGroupCullState(List(3) { CullFlag.Reject.asWire() }),
            )
        }

        @Test
        fun `one pick makes the group a pick`() {
            assertEquals(
                GroupCullState.AnyPick,
                deriveGroupCullState(listOf(CullFlag.Reject.asWire(), CullFlag.Pick.asWire())),
            )
        }

        @Test
        fun `anything else is mixed`() {
            assertEquals(
                GroupCullState.Mixed,
                deriveGroupCullState(listOf(CullFlag.Neutral.asWire(), CullFlag.Reject.asWire())),
            )
        }

        @Test
        fun `an empty group is mixed, because nothing has been decided about it`() {
            assertEquals(GroupCullState.Mixed, deriveGroupCullState(emptyList()))
        }

        @Test
        fun `an unrecognised flag cannot make a group look all-rejected`() {
            // Treating a token this build cannot read as a reject would let a newer peer's
            // value sweep a member into trash.
            assertEquals(
                GroupCullState.Mixed,
                deriveGroupCullState(listOf(CullFlag.Reject.asWire(), Wire.Unknown("maybe"))),
            )
        }
    }

    @Nested
    inner class Tags {
        @Test
        fun `stale AI tags are kept but excluded from the fresh set`() {
            // They are the regeneration queue, so dropping them would lose work; showing them
            // would compare across model versions, which is forbidden.
            val slot = ModelSlot(ModelId("mobileclip-b"), ModelVersion("1"))
            val tags =
                AssetTags(
                    user = listOf(UserTag("beach", AddId(DeviceId("d"), 1))),
                    ai =
                        listOf(
                            AiTag("dog", AddId(DeviceId("d"), 2), slot),
                            AiTag("cat", AddId(DeviceId("d"), 3), slot, isStale = true),
                        ),
                )
            assertEquals(listOf("dog"), tags.freshAi.map(AiTag::tag))
            assertEquals(2, tags.ai.size)
        }

        @Test
        fun `a model output status decides queryability`() {
            assertTrue(ModelOutputStatus.Current.isQueryable)
            assertFalse(ModelOutputStatus.Stale.isQueryable)
            assertFalse(ModelOutputStatus.Unrecognised.isQueryable)
        }
    }

    @Nested
    inner class LifecycleActions {
        @Test
        fun `only create has no prior chain link`() {
            assertFalse(LifecycleAction.Create.hasPriorLink)
            LifecycleAction.entries.filter { it != LifecycleAction.Create }.forEach {
                assertTrue(it.hasPriorLink, "$it must chain from a prior head")
            }
        }

        @Test
        fun `the closed action set has exactly seven members`() {
            assertEquals(7, LifecycleAction.entries.size)
        }
    }

    @Nested
    inner class SyncStates {
        @Test
        fun `awaiting-original is its own state, not a failure`() {
            // Fetching such an asset's original is a transient pending condition explicitly
            // distinct from a permanent gone, so it must not be reachable as a Failed state.
            val states: List<SyncState> =
                listOf(
                    SyncState.AwaitingOriginal,
                    SyncState.Failed(ErrorCode("error.blob.pending_upload")),
                )
            assertEquals(1, states.filterIsInstance<SyncState.Failed>().size)
            assertNull(states.first().let { it as? SyncState.Failed }?.code)
        }

        @Test
        fun `an upload in flight reports its tier and its progress`() {
            val uploading = SyncState.Uploading(UploadTier.Original, bytesSent = 512, bytesTotal = 2048)
            assertEquals(UploadTier.Original, uploading.tier)
            assertEquals(512L, uploading.bytesSent)
            assertEquals(UploadTier.Preview, SyncState.Queued(UploadTier.Preview).nextTier)
        }

        @Test
        fun `a permanent fetch failure is distinct from an authorization change`() {
            assertEquals(3, FetchFailureClass.entries.size)
            assertEquals(
                setOf("transient", "permanent", "authorization_changed"),
                FetchFailureClass.entries.map(FetchFailureClass::wire).toSet(),
            )
        }
    }

    private companion object {
        val T0: Instant = Instant.parse("2026-01-01T00:00:00Z")
    }
}
