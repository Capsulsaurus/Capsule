package com.justin13888.capsule.screens.transfer

import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.BlobHash
import com.justin13888.capsule.screens.domain.StampedTransferEvent
import com.justin13888.capsule.screens.domain.TransferEvent
import com.justin13888.capsule.screens.domain.TransferFeedItem
import com.justin13888.capsule.screens.domain.TransferRetryReason
import com.justin13888.capsule.screens.domain.UploadId
import com.justin13888.capsule.screens.transfer.FfiTransferEventPort.Companion.toEvent
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.runBlocking
import uniffi.capsule_sdk.FfiRetryReason
import uniffi.capsule_sdk.FfiStampedTransferEvent
import uniffi.capsule_sdk.FfiTransferEvent
import uniffi.capsule_sdk.FfiTransferFeedItem
import uniffi.capsule_sdk.FfiTransferSubscriptionInterface
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

/**
 * The SDK's transfer subscription reaches the transfer view as the domain's feed: each
 * event field for field with the SDK's stamp, a lag as its count, the end of the session
 * as the end of the stream, and the subscription released however the collector stops.
 */
class FfiTransferEventPortTest {
    /** A subscription that yields [script] in order, then answers `null` as a closed one does. */
    private class ScriptedSubscription(
        script: List<FfiTransferFeedItem>,
    ) : FfiTransferSubscriptionInterface,
        AutoCloseable {
        private val pending = ArrayDeque(script)
        var pulls = 0
        var closed = false

        override suspend fun next(): FfiTransferFeedItem? {
            pulls += 1
            return pending.removeFirstOrNull()
        }

        override fun close() {
            closed = true
        }
    }

    private val blob = "ab".repeat(32)
    private val session = "0190f2a4-0000-7000-8000-000000000001"
    private val asset = "0190f2a4-0000-7000-8000-000000000002"

    private fun stamped(
        atMs: ULong,
        event: FfiTransferEvent,
    ) = FfiTransferFeedItem.Event(FfiStampedTransferEvent(atMs, event))

    @Test
    fun eventsCarryTheSdksStampAndALagItsCountUntilTheSessionIsGone() =
        runBlocking {
            val subscription =
                ScriptedSubscription(
                    listOf(
                        stamped(0uL, FfiTransferEvent.ChunkSent(blob, session, 0uL, 1024uL)),
                        FfiTransferFeedItem.Lagged(3uL),
                        stamped(420uL, FfiTransferEvent.Finalize(blob, session, 1024uL)),
                    ),
                )
            val items = FfiTransferEventPort { subscription }.events().toList()
            assertEquals(
                listOf(
                    TransferFeedItem.Event(StampedTransferEvent(0, TransferEvent.ChunkSent(BlobHash(blob), UploadId(session), 0, 1024)))
                        as TransferFeedItem,
                    TransferFeedItem.Lagged(3),
                    TransferFeedItem.Event(StampedTransferEvent(420, TransferEvent.Finalize(BlobHash(blob), UploadId(session), 1024))),
                ),
                items,
            )
            assertTrue(subscription.closed, "the subscription is released once the stream ends")
        }

    @Test
    fun aCollectorThatStopsEarlyReleasesTheSubscriptionAndPullsNoFurther() =
        runBlocking {
            val subscription =
                ScriptedSubscription(
                    listOf(
                        stamped(1uL, FfiTransferEvent.FetchVerified(blob, 9uL)),
                        stamped(2uL, FfiTransferEvent.FetchVerified(blob, 9uL)),
                    ),
                )
            FfiTransferEventPort { subscription }.events().first()
            assertEquals(1, subscription.pulls)
            assertTrue(subscription.closed)
        }

    @Test
    fun eachCollectionOpensItsOwnSubscription() =
        runBlocking {
            var opened = 0
            val port =
                FfiTransferEventPort {
                    opened += 1
                    ScriptedSubscription(emptyList())
                }
            port.events().toList()
            port.events().toList()
            assertEquals(2, opened)
        }

    @Test
    fun anUnreachableCoreEndsTheStreamInsteadOfThrowing() =
        runBlocking {
            val atOpen = FfiTransferEventPort { throw UnsatisfiedLinkError("no libcapsule_sdk") }
            assertEquals(emptyList(), atOpen.events().toList())

            val failing =
                object : FfiTransferSubscriptionInterface {
                    override suspend fun next(): FfiTransferFeedItem? = throw NoClassDefFoundError("uniffi")
                }
            assertEquals(emptyList(), FfiTransferEventPort { failing }.events().toList())
        }

    @Test
    fun unsignedCountsPastLongMaxSaturate() =
        runBlocking {
            val huge = ULong.MAX_VALUE
            val subscription =
                ScriptedSubscription(
                    listOf(stamped(huge, FfiTransferEvent.RangeRequested(blob, huge, null)), FfiTransferFeedItem.Lagged(huge)),
                )
            assertEquals(
                listOf(
                    TransferFeedItem.Event(
                        StampedTransferEvent(Long.MAX_VALUE, TransferEvent.RangeRequested(BlobHash(blob), Long.MAX_VALUE, null)),
                    ) as TransferFeedItem,
                    TransferFeedItem.Lagged(Long.MAX_VALUE),
                ),
                FfiTransferEventPort { subscription }.events().toList(),
            )
        }

    @Test
    fun everySdkEventMapsFieldForField() {
        val b = BlobHash(blob)
        val s = UploadId(session)
        val a = AssetId(asset)
        val cases: List<Pair<FfiTransferEvent, TransferEvent>> =
            listOf(
                FfiTransferEvent.SessionCreated(blob, session, 10uL, 2uL) to TransferEvent.SessionCreated(b, s, 10, 2),
                FfiTransferEvent.SessionResumed(blob, session, 10uL, 4uL) to TransferEvent.SessionResumed(b, s, 10, 4),
                FfiTransferEvent.ChunkSent(blob, session, 1uL, 2uL) to TransferEvent.ChunkSent(b, s, 1, 2),
                FfiTransferEvent.ChunkAcked(blob, session, 1uL, 2uL, 3uL) to TransferEvent.ChunkAcked(b, s, 1, 2, 3),
                FfiTransferEvent.ChunkRetried(blob, session, 1uL, 2uL, FfiRetryReason.TransportReset(250uL)) to
                    TransferEvent.ChunkRetried(b, s, 1, 2, TransferRetryReason.TransportReset(250)),
                FfiTransferEvent.ChunkRetried(blob, session, 1uL, 2uL, FfiRetryReason.OffsetRealign(7uL)) to
                    TransferEvent.ChunkRetried(b, s, 1, 2, TransferRetryReason.OffsetRealign(7)),
                FfiTransferEvent.ChunkRetried(blob, session, 1uL, 2uL, FfiRetryReason.SessionLost) to
                    TransferEvent.ChunkRetried(b, s, 1, 2, TransferRetryReason.SessionLost),
                FfiTransferEvent.ChunkRetried(blob, session, 1uL, 2uL, FfiRetryReason.ServerWait(900uL)) to
                    TransferEvent.ChunkRetried(b, s, 1, 2, TransferRetryReason.ServerWait(900)),
                FfiTransferEvent.ChecksumRejected(blob, session, 1uL, 2uL) to TransferEvent.ChecksumRejected(b, s, 1, 2),
                FfiTransferEvent.Finalize(blob, session, 10uL) to TransferEvent.Finalize(b, s, 10),
                FfiTransferEvent.Merged(blob, asset) to TransferEvent.Merged(b, asset),
                FfiTransferEvent.Verify(asset, true, 3uL, 1uL) to TransferEvent.Verify(a, true, 3, 1),
                FfiTransferEvent.Receipt(asset, blob, 5uL) to TransferEvent.Receipt(a, b, 5),
                FfiTransferEvent.RangeRequested(blob, 0uL, 64uL) to TransferEvent.RangeRequested(b, 0, 64),
                FfiTransferEvent.RangeReceived(blob, 0uL, 32uL) to TransferEvent.RangeReceived(b, 0, 32),
                FfiTransferEvent.FetchRetried(blob, 32uL, true, 500uL) to TransferEvent.FetchRetried(b, 32, true, 500),
                FfiTransferEvent.FetchVerified(blob, 64uL) to TransferEvent.FetchVerified(b, 64),
                FfiTransferEvent.IntegrityRejected(blob, 64uL) to TransferEvent.IntegrityRejected(b, 64),
            )
        cases.forEach { (sdk, domain) -> assertEquals(domain, sdk.toEvent(), "mapping of $sdk") }
    }
}
